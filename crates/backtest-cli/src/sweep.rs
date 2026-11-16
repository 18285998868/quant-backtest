//! 参数扫描:网格展开、并行回测、排序、top-K、样本外与敏感性。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use backtest_core::config::{Objective, RunConfig};
use backtest_core::engine::{EngineInput, run_backtest};
use backtest_core::error::{ConfigError, Error, ReportError, Result};
use backtest_core::metrics::{deflated_sharpe, equity_returns, period_sharpe};
use backtest_core::model::{
    BacktestResult, DeflatedSharpe, DrawdownEpisode, EquityPoint, FillRecord, Metrics,
    MonthlyReturn, TradeRecord, Universe,
};
use backtest_core::system::{ResolvedWorkers, peak_rss_bytes};
use backtest_report::{
    HeatmapCell, MarginalPoint, OosReport, ParamMarginal, SweepCounts as ReportCounts,
    SweepHeatmap, SweepReport, SweepRow,
};
use backtest_strategy::build;
use chrono::{SecondsFormat, Utc};
use rayon::prelude::*;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "1.1";
const OOS_SAMPLE_LIMIT: usize = 100;
const HEATMAP_VALUE_LIMIT: usize = 12;

#[derive(Debug)]
pub(crate) struct SweepRun {
    pub summary: SweepSummary,
    pub tasks: Vec<SweepTask>,
    pub report: SweepReport,
    pub best_full: BacktestResult,
}

#[derive(Debug, Serialize)]
pub(crate) struct SweepSummary {
    pub schema_version: &'static str,
    pub sweep_hash: String,
    pub meta: SweepMeta,
    pub grid: BTreeMap<String, Vec<Value>>,
    pub counts: SweepCounts,
    pub results: Vec<SweepTopResult>,
    pub oos: Option<OosReport>,
    pub dsr: Option<DeflatedSharpe>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SweepMeta {
    pub strategy: String,
    pub objective: String,
    pub evaluation: &'static str,
    pub top_k: usize,
    pub oos_split: f64,
    pub split_index: usize,
    pub oos_warmup_bars: usize,
    pub config_hash: String,
    pub data_hash: String,
    pub threads: usize,
    pub threads_overridden: bool,
    pub memory_overridden: bool,
    pub cpu_available: usize,
    pub memory_available_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub worker_budget_bytes: u64,
    pub generated_at: String,
    pub elapsed_ms: u128,
    pub peak_rss_bytes: Option<u64>,
    pub retained_equity_points: usize,
    pub retained_bytes_estimate: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SweepCounts {
    pub total: usize,
    pub valid: usize,
    pub skipped_invalid: usize,
    pub warnings: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SweepTask {
    pub index: usize,
    pub params: BTreeMap<String, Value>,
    pub metrics: Metrics,
    pub config_hash: String,
    pub warning_count: usize,
    pub objective_value: Option<f64>,
    pub period_sharpe: Option<f64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SweepTopResult {
    pub rank: usize,
    pub params: BTreeMap<String, Value>,
    pub config_hash: String,
    pub result_hash: String,
    pub metrics: Metrics,
    pub equity: Vec<EquityPoint>,
    pub fills: Vec<FillRecord>,
    pub trades: Vec<TradeRecord>,
    pub drawdowns: Vec<DrawdownEpisode>,
    pub monthly: Vec<MonthlyReturn>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct SweepPaths {
    pub csv: PathBuf,
    pub json: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IndexRange {
    pub start: usize,
    pub trade_start: usize,
    pub end: usize,
}

impl IndexRange {
    pub fn full(len: usize) -> Self {
        Self {
            start: 0,
            trade_start: 0,
            end: len,
        }
    }

    pub fn head(end: usize) -> Self {
        Self {
            start: 0,
            trade_start: 0,
            end,
        }
    }

    pub fn with_warmup(start: usize, trade_start: usize, end: usize) -> Self {
        Self {
            start: start.min(trade_start),
            trade_start,
            end,
        }
    }
}

struct Combo {
    params: BTreeMap<String, Value>,
    config: RunConfig,
}

/// 已校验、已展开的网格。构造一次,可复用到多个窗口。
pub(crate) struct Grid {
    combos: Vec<Combo>,
    pub total: usize,
    pub skipped_invalid: usize,
}

impl Grid {
    pub fn build(config: &RunConfig) -> Result<Self> {
        let sweep = config.sweep.as_ref().ok_or_else(|| ConfigError::Invalid {
            field: "sweep".into(),
            message: "配置缺少 [sweep] 段".into(),
        })?;
        let raw = expand_grid(&sweep.grid)?;
        let total = raw.len();
        let mut combos = Vec::with_capacity(total);
        let mut skipped_invalid = 0;
        for params in raw {
            let mut strategy = config.strategy.clone();
            for (name, value) in params.iter() {
                strategy.apply_param(name.as_str(), value)?;
            }
            match config.with_strategy(strategy) {
                Ok(combo_config) => combos.push(Combo {
                    params,
                    config: combo_config,
                }),
                Err(Error::Config(ConfigError::Invalid { .. })) => skipped_invalid += 1,
                Err(error) => return Err(error),
            }
        }
        if combos.is_empty() {
            return Err(ConfigError::Invalid {
                field: "sweep.grid".into(),
                message: format!("{total} 个组合全部非法"),
            }
            .into());
        }
        Ok(Self {
            combos,
            total,
            skipped_invalid,
        })
    }

    pub fn len(&self) -> usize {
        self.combos.len()
    }

    pub fn max_warmup_bars(&self) -> usize {
        self.combos
            .iter()
            .map(|combo| combo.config.strategy.warmup_bars())
            .max()
            .unwrap_or(0)
    }

    fn params(&self, index: usize) -> &BTreeMap<String, Value> {
        &self.combos[index].params
    }
}

pub(crate) fn build_pool(threads: usize) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|error| {
            ConfigError::Invalid {
                field: "sweep.threads".into(),
                message: error.to_string(),
            }
            .into()
        })
}

fn expand_grid(grid: &BTreeMap<String, Vec<Value>>) -> Result<Vec<BTreeMap<String, Value>>> {
    let total = grid.values().try_fold(1usize, |acc, values| {
        acc.checked_mul(values.len()).ok_or_else(|| {
            Error::Config(ConfigError::Invalid {
                field: "sweep.grid".into(),
                message: "组合数量超出 usize 范围".into(),
            })
        })
    })?;
    let mut combos: Vec<BTreeMap<String, Value>> = Vec::with_capacity(total);
    combos.push(BTreeMap::new());
    for (name, values) in grid {
        let mut next = Vec::with_capacity(combos.len() * values.len());
        for combo in &combos {
            for value in values {
                let mut item = combo.clone();
                item.insert(name.clone(), value.clone());
                next.push(item);
            }
        }
        combos = next;
    }
    Ok(combos)
}

pub(crate) fn engine_input(config: &RunConfig, threads: usize, range: IndexRange) -> EngineInput {
    EngineInput {
        initial_cash: config.initial_cash,
        risk_free_rate: config.risk_free_rate,
        execution: config.execution,
        risk: config.risk,
        strategy_name: config.strategy.name().to_string(),
        params: config.strategy.params(),
        config_hash: config.config_hash.clone(),
        threads,
        requested_start: config.start,
        requested_end: config.end,
        start_index: range.start,
        trade_start_index: range.trade_start,
        end_index: range.end,
    }
}

pub(crate) fn run_single(
    config: &RunConfig,
    universe: &Universe,
    threads: usize,
    range: IndexRange,
) -> Result<BacktestResult> {
    let mut strategy = build(&config.strategy, universe.symbol_count());
    let input = engine_input(config, threads, range);
    run_backtest(universe, &input, strategy.as_mut())
}

pub(crate) fn run_grid(
    pool: &rayon::ThreadPool,
    grid: &Grid,
    universe: &Universe,
    range: IndexRange,
    objective: Objective,
) -> Result<Vec<SweepTask>> {
    let threads = pool.current_num_threads();
    let initial_cash = grid.combos[0].config.initial_cash;
    pool.install(|| {
        grid.combos
            .par_iter()
            .enumerate()
            .map(|(index, combo)| {
                let result = run_single(&combo.config, universe, threads, range)?;
                let returns = equity_returns(&result.equity, initial_cash);
                let objective_value = objective.value(&result.metrics);
                Ok(SweepTask {
                    index,
                    params: combo.params.clone(),
                    metrics: result.metrics,
                    config_hash: combo.config.config_hash.clone(),
                    warning_count: result.warnings.len(),
                    objective_value,
                    period_sharpe: period_sharpe(&returns),
                })
            })
            .collect::<Result<Vec<_>>>()
    })
}

fn run_selected(
    pool: &rayon::ThreadPool,
    grid: &Grid,
    indices: &[usize],
    universe: &Universe,
    range: IndexRange,
) -> Result<Vec<(usize, BacktestResult)>> {
    let threads = pool.current_num_threads();
    pool.install(|| {
        indices
            .par_iter()
            .map(|index| {
                let result = run_single(&grid.combos[*index].config, universe, threads, range)?;
                Ok((*index, result))
            })
            .collect::<Result<Vec<_>>>()
    })
}

pub(crate) fn sort_tasks(tasks: &mut [SweepTask]) {
    tasks.sort_by(|a, b| {
        let av = a.objective_value.unwrap_or(f64::NEG_INFINITY);
        let bv = b.objective_value.unwrap_or(f64::NEG_INFINITY);
        bv.total_cmp(&av).then_with(|| a.index.cmp(&b.index))
    });
}

pub(crate) fn execute(
    config: &RunConfig,
    universe: &Universe,
    workers: &ResolvedWorkers,
    threads_overridden: bool,
    memory_overridden: bool,
) -> Result<SweepRun> {
    let sweep = config.sweep.as_ref().ok_or_else(|| ConfigError::Invalid {
        field: "sweep".into(),
        message: "配置缺少 [sweep] 段".into(),
    })?;
    let started = Instant::now();
    let grid = Grid::build(config)?;
    if universe.len() < 2 {
        return Err(ConfigError::Invalid {
            field: "sweep.oos_split".into(),
            message: "样本外切分至少需要 2 根 bar".into(),
        }
        .into());
    }
    let split_index = ((universe.len() as f64) * sweep.oos_split).round() as usize;
    let split_index = split_index.clamp(1, universe.len() - 1);
    let warmup = grid.max_warmup_bars();
    let mut oos_warnings = Vec::new();
    if split_index < warmup {
        oos_warnings.push(format!(
            "样本外预热不足:切分下标 {split_index},最长策略窗口 {warmup};预热从 0 开始"
        ));
    }
    let is_range = IndexRange::head(split_index);
    let oos_range = IndexRange::with_warmup(
        split_index.saturating_sub(warmup),
        split_index,
        universe.len(),
    );

    let pool = build_pool(workers.threads)?;
    let mut tasks = run_grid(&pool, &grid, universe, is_range, sweep.objective)?;
    sort_tasks(&mut tasks);
    let keep = sweep.top_k.min(tasks.len());
    let best_index = tasks
        .first()
        .filter(|task| task.objective_value.is_some())
        .map(|task| task.index);

    let top_indices: Vec<usize> = tasks[..keep].iter().map(|task| task.index).collect();
    let top_runs = run_selected(&pool, &grid, &top_indices, universe, is_range)?;
    let mut top_results = Vec::with_capacity(top_runs.len());
    for (rank, (index, result)) in top_runs.into_iter().enumerate() {
        let result_hash = backtest_report::content_hash(&result)?;
        top_results.push(SweepTopResult {
            rank: rank + 1,
            params: grid.params(index).clone(),
            config_hash: result.meta.config_hash.clone(),
            result_hash,
            metrics: result.metrics,
            equity: result.equity,
            fills: result.fills,
            trades: result.trades,
            drawdowns: result.drawdowns,
            monthly: result.monthly,
            warnings: result.warnings,
        });
    }

    let sample_indices: Vec<usize> = tasks
        .iter()
        .take(OOS_SAMPLE_LIMIT)
        .map(|task| task.index)
        .collect();
    let oos_runs = run_selected(&pool, &grid, &sample_indices, universe, oos_range)?;
    let oos_metrics: BTreeMap<usize, Metrics> = oos_runs
        .into_iter()
        .map(|(index, result)| (index, result.metrics))
        .collect();

    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for task in tasks.iter().take(OOS_SAMPLE_LIMIT) {
        let Some(is_value) = task.objective_value else {
            continue;
        };
        let Some(oos_value) = oos_metrics
            .get(&task.index)
            .and_then(|metrics| sweep.objective.value(metrics))
        else {
            continue;
        };
        xs.push(is_value);
        ys.push(oos_value);
    }
    let rank_correlation = spearman(&xs, &ys);

    let trial_sr: Vec<f64> = tasks.iter().filter_map(|task| task.period_sharpe).collect();
    let dsr = top_results.first().and_then(|best| {
        let returns = equity_returns(&best.equity, config.initial_cash);
        deflated_sharpe(&returns, &trial_sr)
    });

    let rows: Vec<SweepRow> = tasks
        .iter()
        .take(OOS_SAMPLE_LIMIT)
        .enumerate()
        .map(|(rank, task)| SweepRow {
            rank: rank + 1,
            params: task.params.clone(),
            objective_value: task.objective_value,
            metrics: task.metrics.clone(),
            oos_metrics: oos_metrics.get(&task.index).cloned(),
        })
        .collect();

    let warnings = tasks.iter().map(|task| task.warning_count).sum();
    let retained_equity_points = top_results.iter().map(|result| result.equity.len()).sum();
    let retained_bytes_estimate = top_results
        .iter()
        .map(|result| {
            result.equity.len() * std::mem::size_of::<EquityPoint>()
                + result.fills.len() * std::mem::size_of::<FillRecord>()
                + result.trades.len() * std::mem::size_of::<TradeRecord>()
                + result.drawdowns.len() * std::mem::size_of::<DrawdownEpisode>()
                + result.monthly.len() * std::mem::size_of::<MonthlyReturn>()
        })
        .sum::<usize>() as u64;

    let best_params = best_index
        .map(|index| grid.params(index).clone())
        .unwrap_or_default();
    let in_sample = tasks
        .first()
        .map(|task| task.metrics.clone())
        .unwrap_or_else(|| Metrics::empty(config.periods_per_year(), config.risk_free_rate));
    let out_sample = best_index
        .and_then(|index| oos_metrics.get(&index).cloned())
        .unwrap_or_else(|| Metrics::empty(config.periods_per_year(), config.risk_free_rate));
    let oos = OosReport {
        split: sweep.oos_split,
        split_index,
        warmup_bars: warmup,
        best_params: best_params.clone(),
        in_sample,
        out_sample,
        rank_correlation,
        sample_size: sample_indices.len(),
        truncated: tasks.len() > OOS_SAMPLE_LIMIT,
        warnings: oos_warnings,
    };

    let marginals = build_marginals(&sweep.grid, &tasks);
    let heatmap = build_heatmap(&sweep.grid, &marginals, &tasks);
    let counts = SweepCounts {
        total: grid.total,
        valid: grid.len(),
        skipped_invalid: grid.skipped_invalid,
        warnings,
    };
    let mut summary = SweepSummary {
        schema_version: SCHEMA_VERSION,
        sweep_hash: String::new(),
        meta: SweepMeta {
            strategy: config.strategy.name().to_string(),
            objective: sweep.objective.name().to_string(),
            evaluation: "in_sample",
            top_k: keep,
            oos_split: sweep.oos_split,
            split_index,
            oos_warmup_bars: warmup,
            config_hash: config.config_hash.clone(),
            data_hash: universe.data_hash.clone(),
            threads: workers.threads,
            threads_overridden,
            memory_overridden,
            cpu_available: workers.cpu_available,
            memory_available_bytes: workers.memory_available_bytes,
            memory_limit_bytes: workers.memory_limit_bytes,
            worker_budget_bytes: workers.worker_budget_bytes,
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            elapsed_ms: started.elapsed().as_millis(),
            peak_rss_bytes: peak_rss_bytes(),
            retained_equity_points,
            retained_bytes_estimate,
        },
        grid: sweep.grid.clone(),
        counts,
        results: top_results,
        oos: Some(oos.clone()),
        dsr: dsr.clone(),
    };
    summary.sweep_hash = summary_hash(&summary)?;

    let report = SweepReport {
        objective: sweep.objective.name().to_string(),
        top_k: keep,
        counts: ReportCounts {
            total: summary.counts.total,
            valid: summary.counts.valid,
            skipped_invalid: summary.counts.skipped_invalid,
            warnings: summary.counts.warnings,
        },
        grid: sweep.grid.clone(),
        rows,
        marginals,
        heatmap,
        oos: Some(oos),
        dsr,
        sweep_hash: summary.sweep_hash.clone(),
        elapsed_ms: summary.meta.elapsed_ms,
    };

    let best_config = best_index.map(|index| &grid.combos[index].config);
    let best_full = match best_config {
        Some(combo) => run_single(
            combo,
            universe,
            workers.threads,
            IndexRange::full(universe.len()),
        )?,
        None => {
            return Err(ConfigError::Invalid {
                field: "sweep.grid".into(),
                message: "所有组合的目标值均为 null,无法选出最优参数".into(),
            }
            .into());
        }
    };

    Ok(SweepRun {
        summary,
        tasks,
        report,
        best_full,
    })
}

fn build_marginals(grid: &BTreeMap<String, Vec<Value>>, tasks: &[SweepTask]) -> Vec<ParamMarginal> {
    let mut names: Vec<&String> = grid.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let points = grid[name]
                .iter()
                .map(|value| {
                    let matching: Vec<&SweepTask> = tasks
                        .iter()
                        .filter(|task| task.params.get(name) == Some(value))
                        .collect();
                    let values: Vec<f64> = matching
                        .iter()
                        .filter_map(|task| task.objective_value)
                        .collect();
                    MarginalPoint {
                        value: value.clone(),
                        mean_objective: if values.is_empty() {
                            None
                        } else {
                            Some(values.iter().sum::<f64>() / values.len() as f64)
                        },
                        count: matching.len(),
                    }
                })
                .collect();
            ParamMarginal {
                name: name.clone(),
                points,
            }
        })
        .collect()
}

fn build_heatmap(
    grid: &BTreeMap<String, Vec<Value>>,
    marginals: &[ParamMarginal],
    tasks: &[SweepTask],
) -> Option<SweepHeatmap> {
    let mut ranked: Vec<(&ParamMarginal, f64)> = marginals
        .iter()
        .filter(|marginal| marginal.points.len() >= 2)
        .map(|marginal| {
            let values: Vec<f64> = marginal
                .points
                .iter()
                .filter_map(|point| point.mean_objective)
                .collect();
            let range = if values.is_empty() {
                0.0
            } else {
                values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                    - values.iter().copied().fold(f64::INFINITY, f64::min)
            };
            (marginal, range)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    if ranked.len() < 2 {
        return None;
    }
    let (x_marginal, _) = ranked[0];
    let (y_marginal, _) = ranked[1];
    let x_values = limit_values(&x_marginal.points);
    let y_values = limit_values(&y_marginal.points);
    let mut cells = Vec::new();
    for y in &y_values {
        for x in &x_values {
            let matching: Vec<&SweepTask> = tasks
                .iter()
                .filter(|task| {
                    task.params.get(&x_marginal.name) == Some(x)
                        && task.params.get(&y_marginal.name) == Some(y)
                })
                .collect();
            let values: Vec<f64> = matching
                .iter()
                .filter_map(|task| task.objective_value)
                .collect();
            if !values.is_empty() {
                cells.push(HeatmapCell {
                    x: x.clone(),
                    y: y.clone(),
                    mean_objective: Some(values.iter().sum::<f64>() / values.len() as f64),
                    count: matching.len(),
                });
            }
        }
    }
    if cells.is_empty() {
        return None;
    }
    let _ = grid;
    Some(SweepHeatmap {
        x_param: x_marginal.name.clone(),
        y_param: y_marginal.name.clone(),
        x_values,
        y_values,
        cells,
    })
}

fn limit_values(points: &[MarginalPoint]) -> Vec<Value> {
    let mut ranked: Vec<&MarginalPoint> = points.iter().collect();
    ranked.sort_by(|a, b| {
        let av = a.mean_objective.unwrap_or(f64::NEG_INFINITY);
        let bv = b.mean_objective.unwrap_or(f64::NEG_INFINITY);
        bv.total_cmp(&av)
    });
    let mut values: Vec<Value> = ranked
        .into_iter()
        .take(HEATMAP_VALUE_LIMIT)
        .map(|point| point.value.clone())
        .collect();
    values.sort_by_key(value_text);
    values
}

fn spearman(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() < 2 || xs.len() != ys.len() {
        return None;
    }
    let spread = |values: &[f64]| {
        let first = values[0];
        values.iter().any(|value| *value != first)
    };
    if !spread(xs) || !spread(ys) {
        return None;
    }
    let rx = ranks(xs);
    let ry = ranks(ys);
    let n = xs.len() as f64;
    let d2: f64 = rx
        .iter()
        .zip(&ry)
        .map(|(left, right)| (left - right) * (left - right))
        .sum();
    let rho = 1.0 - 6.0 * d2 / (n * (n * n - 1.0));
    rho.is_finite().then_some(rho)
}

fn ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|left, right| values[*left].total_cmp(&values[*right]));
    let mut out = vec![0.0; values.len()];
    let mut index = 0;
    while index < order.len() {
        let mut end = index + 1;
        while end < order.len() && values[order[end]] == values[order[index]] {
            end += 1;
        }
        let average = (index + 1 + end) as f64 / 2.0;
        for slot in &order[index..end] {
            out[*slot] = average;
        }
        index = end;
    }
    out
}

pub(crate) fn write_outputs(run: &SweepRun, output_dir: &Path) -> Result<SweepPaths> {
    std::fs::create_dir_all(output_dir).map_err(|source| ReportError::CreateDir {
        path: output_dir.to_path_buf(),
        source,
    })?;
    let csv = output_dir.join("sweep.csv");
    let json = output_dir.join("sweep_summary.json");
    write_csv(&run.tasks, &run.summary, &csv)?;
    let text =
        serde_json::to_string_pretty(&run.summary).map_err(|error| ReportError::Serialize {
            message: error.to_string(),
        })?;
    std::fs::write(&json, text).map_err(|source| ReportError::Write {
        path: json.clone(),
        source,
    })?;
    Ok(SweepPaths { csv, json })
}

fn write_csv(tasks: &[SweepTask], summary: &SweepSummary, path: &Path) -> Result<()> {
    let params: Vec<&String> = summary.grid.keys().collect();
    let mut writer = csv::Writer::from_path(path).map_err(|error| ReportError::Serialize {
        message: format!("创建 {} 失败: {error}", path.display()),
    })?;
    let mut header = vec![
        "rank".to_string(),
        "objective".to_string(),
        "period_sharpe".to_string(),
        "sharpe".to_string(),
        "cagr".to_string(),
        "calmar".to_string(),
        "total_return".to_string(),
        "max_drawdown".to_string(),
        "annual_volatility".to_string(),
        "trades".to_string(),
        "warnings".to_string(),
        "config_hash".to_string(),
    ];
    header.extend(params.iter().map(|name| format!("param:{name}")));
    writer
        .write_record(header)
        .map_err(|error| ReportError::Serialize {
            message: error.to_string(),
        })?;
    for (rank, task) in tasks.iter().enumerate() {
        let mut row = vec![
            (rank + 1).to_string(),
            option_f64(task.objective_value),
            option_f64(task.period_sharpe),
            option_f64(task.metrics.sharpe),
            option_f64(task.metrics.cagr),
            option_f64(task.metrics.calmar),
            option_f64(task.metrics.total_return),
            option_f64(task.metrics.max_drawdown),
            option_f64(task.metrics.annual_volatility),
            task.metrics.trades.to_string(),
            task.warning_count.to_string(),
            task.config_hash.clone(),
        ];
        row.extend(
            params
                .iter()
                .map(|name| task.params.get(*name).map(value_text).unwrap_or_default()),
        );
        writer
            .write_record(row)
            .map_err(|error| ReportError::Serialize {
                message: error.to_string(),
            })?;
    }
    writer.flush().map_err(|source| ReportError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn option_f64(value: Option<f64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn summary_hash(summary: &SweepSummary) -> Result<String> {
    let meta = &summary.meta;
    let value = serde_json::json!({
        "schema_version": summary.schema_version,
        "strategy": meta.strategy,
        "objective": meta.objective,
        "evaluation": meta.evaluation,
        "top_k": meta.top_k,
        "oos_split": meta.oos_split,
        "split_index": meta.split_index,
        "oos_warmup_bars": meta.oos_warmup_bars,
        "config_hash": meta.config_hash,
        "data_hash": meta.data_hash,
        "grid": summary.grid,
        "counts": summary.counts,
        "results": summary.results,
        "oos": summary.oos,
        "dsr": summary.dsr,
    });
    let bytes = serde_json::to_vec(&value).map_err(|error| ConfigError::Invalid {
        field: "sweep_hash".into(),
        message: error.to_string(),
    })?;
    Ok(sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_config(weight_values: usize, top_k: usize) -> (tempfile::TempDir, PathBuf) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let data = root.join("fixtures/s5_daily.csv");
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("sweep.toml");
        let weights: Vec<String> = (0..weight_values)
            .map(|index| {
                let step = 0.8 / weight_values as f64;
                format!("{}", 0.01 + index as f64 * step)
            })
            .collect();
        let text = format!(
            r#"[run]
data = "{}"
initial_cash = 100000.0
start = "2020-01-01T00:00:00Z"
end = "2021-03-01T00:00:00Z"
output_dir = "{}"
threads = 1
memory_limit_gb = 0
timeframe = "1d"

[strategy]
name = "sma_cross"
fast = 2
slow = 3
weight = 0.99
allow_short = false

[sweep]
objective = "sharpe"
top_k = {}
oos_split = 0.7

[sweep.grid]
weight = [{}]

[[instruments]]
symbol = "TEST"
asset_class = "crypto"
currency = "USD"
multiplier = 1.0
lot_size = 0.0
margin_rate = 1.0
fee_bps = 0.0
fee_fixed = 0.0
slippage_bps = 0.0
periods_per_year = 365
"#,
            data.display(),
            dir.path().join("out").display(),
            top_k,
            weights.join(", ")
        );
        std::fs::write(&config_path, text).expect("write config");
        (dir, config_path)
    }

    fn run(config_path: &Path, threads: usize) -> Result<SweepRun> {
        let config = backtest_core::config::load_config(config_path)?;
        let workers = ResolvedWorkers {
            threads,
            cpu_available: 12,
            memory_available_bytes: Some(16 * 1024 * 1024 * 1024),
            memory_limit_bytes: Some(16 * 1024 * 1024 * 1024),
            worker_budget_bytes: 64 * 1024 * 1024,
            warnings: Vec::new(),
        };
        let universe = backtest_data::load_universe(&backtest_data::LoadOptions {
            source: backtest_data::LoadSource::from(&config.data),
            timeframe: config.timeframe,
            instruments: &config.instruments,
            start: config.start,
            end: config.end,
        })?;
        execute(&config, &universe, &workers, false, false)
    }

    #[test]
    fn serial_and_parallel_metrics_match() {
        let (_dir, path) = fixture_config(100, 5);
        let serial = run(&path, 1).expect("serial");
        let parallel = run(&path, 2).expect("parallel");
        assert_eq!(serial.tasks.len(), 100);
        assert_eq!(parallel.tasks.len(), 100);
        for (a, b) in serial.tasks.iter().zip(&parallel.tasks) {
            assert_eq!(a.params, b.params);
            assert_eq!(a.config_hash, b.config_hash);
            assert_eq!(a.metrics.sharpe, b.metrics.sharpe);
            assert_eq!(a.metrics.cagr, b.metrics.cagr);
            assert_eq!(a.metrics.total_return, b.metrics.total_return);
        }
        let oos = serial.summary.oos.as_ref().expect("oos report");
        assert_eq!(oos.split, 0.7);
        assert_eq!(oos.split_index, 280);
        assert_eq!(oos.sample_size, 100);
        assert!(oos.in_sample.sharpe.is_some());
        assert!(oos.out_sample.sharpe.is_some());
    }

    #[test]
    fn repeated_sweeps_have_same_hash() {
        let (_dir, path) = fixture_config(20, 3);
        let first = run(&path, 2).expect("first");
        let second = run(&path, 2).expect("second");
        assert_eq!(first.summary.sweep_hash, second.summary.sweep_hash);
        assert_eq!(first.summary.results.len(), 3);
        assert_eq!(first.summary.counts.valid, 20);
    }

    #[test]
    fn top_k_keeps_only_requested_curves() {
        let (_dir, path) = fixture_config(1000, 20);
        let result = run(&path, 2).expect("sweep");
        assert_eq!(result.summary.results.len(), 20);
        assert_eq!(result.summary.meta.top_k, 20);
        assert_eq!(
            result.summary.meta.retained_equity_points,
            result
                .summary
                .results
                .iter()
                .map(|item| item.equity.len())
                .sum::<usize>()
        );
    }

    #[test]
    fn invalid_combinations_are_skipped() {
        let (_dir, path) = fixture_config(1, 1);
        let text = std::fs::read_to_string(&path)
            .expect("read")
            .replace("weight = [0.01]", "fast = [2, 3]\nslow = [3]");
        std::fs::write(&path, text).expect("write");
        let result = run(&path, 1).expect("sweep");
        assert_eq!(result.summary.counts.total, 2);
        assert_eq!(result.summary.counts.valid, 1);
        assert_eq!(result.summary.counts.skipped_invalid, 1);
    }

    #[test]
    fn all_invalid_combinations_are_an_error() {
        let (_dir, path) = fixture_config(1, 1);
        let text = std::fs::read_to_string(&path)
            .expect("read")
            .replace("weight = [0.01]", "fast = [5]\nslow = [3]");
        std::fs::write(&path, text).expect("write");
        let error = run(&path, 1).expect_err("all invalid");
        assert!(error.to_string().contains("全部非法"));
    }

    #[test]
    fn write_outputs_creates_csv_and_summary() {
        let (dir, path) = fixture_config(3, 2);
        let result = run(&path, 1).expect("sweep");
        let out = dir.path().join("out");
        let paths = write_outputs(&result, &out).expect("write");
        assert!(paths.csv.exists());
        assert!(paths.json.exists());
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(paths.json).expect("read"))
                .expect("json");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["counts"]["valid"], 3);
        assert_eq!(value["results"].as_array().expect("results").len(), 2);
    }

    #[test]
    fn spearman_matches_known_ranking() {
        let rho = spearman(&[1.0, 2.0, 3.0], &[10.0, 20.0, 30.0]).expect("rho");
        assert!((rho - 1.0).abs() < 1e-12);
        let rho = spearman(&[1.0, 2.0, 3.0], &[30.0, 20.0, 10.0]).expect("rho");
        assert!((rho + 1.0).abs() < 1e-12);
        assert!(spearman(&[1.0, 1.0], &[1.0, 2.0]).is_none());
    }
}
