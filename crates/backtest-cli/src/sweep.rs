//! 参数扫描:网格展开、并行回测、排序与 top-K 输出。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use backtest_core::config::RunConfig;
use backtest_core::engine::{EngineInput, run_backtest};
use backtest_core::error::{ConfigError, Error, ReportError, Result};
use backtest_core::model::{
    DrawdownEpisode, EquityPoint, FillRecord, Metrics, MonthlyReturn, TradeRecord, Universe,
};
use backtest_core::system::{ResolvedWorkers, peak_rss_bytes};
use backtest_strategy::build;
use chrono::{SecondsFormat, Utc};
use rayon::prelude::*;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "1.0";

#[derive(Debug)]
pub(crate) struct SweepRun {
    pub summary: SweepSummary,
    pub tasks: Vec<SweepTask>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SweepSummary {
    pub schema_version: &'static str,
    pub sweep_hash: String,
    pub meta: SweepMeta,
    pub grid: BTreeMap<String, Vec<Value>>,
    pub counts: SweepCounts,
    pub results: Vec<SweepTopResult>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SweepMeta {
    pub strategy: String,
    pub objective: String,
    pub top_k: usize,
    pub oos_split: f64,
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

struct Combo {
    params: BTreeMap<String, Value>,
    config: RunConfig,
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
    let raw = expand_grid(&sweep.grid)?;
    let total = raw.len();
    let mut combos = Vec::with_capacity(total);
    let mut skipped_invalid = 0;
    for params in raw {
        let mut strategy = config.strategy.clone();
        for (name, value) in &params {
            strategy.apply_param(name, value)?;
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

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers.threads)
        .build()
        .map_err(|error| ConfigError::Invalid {
            field: "sweep.threads".into(),
            message: error.to_string(),
        })?;
    let symbols = universe.symbol_count();
    let threads = workers.threads;

    let mut tasks: Vec<SweepTask> = pool.install(|| {
        combos
            .par_iter()
            .enumerate()
            .map(|(index, combo)| {
                let mut strategy = build(&combo.config.strategy, symbols);
                let input = engine_input(&combo.config, threads);
                let result = run_backtest(universe, &input, strategy.as_mut())?;
                let objective_value = sweep.objective.value(&result.metrics);
                Ok(SweepTask {
                    index,
                    params: combo.params.clone(),
                    metrics: result.metrics,
                    config_hash: combo.config.config_hash.clone(),
                    warning_count: result.warnings.len(),
                    objective_value,
                })
            })
            .collect::<Result<Vec<_>>>()
    })?;
    tasks.sort_by(|a, b| {
        let av = a.objective_value.unwrap_or(f64::NEG_INFINITY);
        let bv = b.objective_value.unwrap_or(f64::NEG_INFINITY);
        bv.total_cmp(&av).then_with(|| a.index.cmp(&b.index))
    });

    let keep = sweep.top_k.min(tasks.len());
    let top_results: Vec<SweepTopResult> = pool.install(|| {
        tasks[..keep]
            .par_iter()
            .enumerate()
            .map(|(rank, task)| {
                let combo = &combos[task.index];
                let mut strategy = build(&combo.config.strategy, symbols);
                let input = engine_input(&combo.config, threads);
                let result = run_backtest(universe, &input, strategy.as_mut())?;
                let result_hash = backtest_report::content_hash(&result)?;
                Ok(SweepTopResult {
                    rank: rank + 1,
                    params: task.params.clone(),
                    config_hash: task.config_hash.clone(),
                    result_hash,
                    metrics: result.metrics,
                    equity: result.equity,
                    fills: result.fills,
                    trades: result.trades,
                    drawdowns: result.drawdowns,
                    monthly: result.monthly,
                    warnings: result.warnings,
                })
            })
            .collect::<Result<Vec<_>>>()
    })?;

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
    let mut summary = SweepSummary {
        schema_version: SCHEMA_VERSION,
        sweep_hash: String::new(),
        meta: SweepMeta {
            strategy: config.strategy.name().to_string(),
            objective: sweep.objective.name().to_string(),
            top_k: keep,
            oos_split: sweep.oos_split,
            config_hash: config.config_hash.clone(),
            data_hash: universe.data_hash.clone(),
            threads,
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
        counts: SweepCounts {
            total,
            valid: combos.len(),
            skipped_invalid,
            warnings,
        },
        results: top_results,
    };
    summary.sweep_hash = summary_hash(&summary)?;
    Ok(SweepRun { summary, tasks })
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

fn engine_input(config: &RunConfig, threads: usize) -> EngineInput {
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
    }
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
        "top_k": meta.top_k,
        "oos_split": meta.oos_split,
        "config_hash": meta.config_hash,
        "data_hash": meta.data_hash,
        "grid": summary.grid,
        "counts": summary.counts,
        "results": summary.results,
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
        let data = root.join("fixtures/sma_6bars.csv");
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
end = "2020-01-07T00:00:00Z"
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
            path: &config.data_path,
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
}
