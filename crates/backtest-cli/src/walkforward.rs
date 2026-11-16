//! Walk-forward 编排:逐窗选参、预热、OOS 拼接与报告组装。

use std::collections::BTreeMap;
use std::time::Instant;

use backtest_core::config::{RunConfig, WalkForwardConfig};
use backtest_core::engine::run_backtest;
use backtest_core::error::{ConfigError, Result};
use backtest_core::metrics::{
    MetricInput, compute_metrics, deflated_sharpe, drawdown_episodes, equity_returns,
    monthly_returns,
};
use backtest_core::model::{BacktestResult, FillRecord, RunMeta, TradeRecord, Universe};
use backtest_core::system::ResolvedWorkers;
use backtest_core::walkforward::{
    WalkForwardResult, WalkForwardWindow, param_stability, plan_gaps, plan_windows, stitch_oos,
};
use backtest_strategy::build;
use chrono::{SecondsFormat, Utc};

use crate::sweep::{self, Grid, IndexRange};

pub(crate) struct WalkForwardRun {
    pub base: BacktestResult,
    pub result: WalkForwardResult,
}

pub(crate) fn execute(
    config: &RunConfig,
    wf: &WalkForwardConfig,
    universe: &Universe,
    workers: &ResolvedWorkers,
) -> Result<WalkForwardRun> {
    let started = Instant::now();
    let grid = Grid::build(config)?;
    let grid_warmup = grid.max_warmup_bars();
    if wf.warmup_bars < grid_warmup {
        return Err(ConfigError::Invalid {
            field: "walk_forward.warmup_bars".into(),
            message: format!("网格中策略最长窗口为 {grid_warmup},warmup_bars 至少需要该值"),
        }
        .into());
    }
    let plan = plan_windows(universe.len(), wf)?;
    let gaps = plan_gaps(&plan.windows);
    let pool = sweep::build_pool(workers.threads)?;
    let mut skipped = plan.skipped.clone();
    let mut windows = Vec::new();
    let mut segments: Vec<(Vec<backtest_core::model::EquityPoint>, f64)> = Vec::new();
    let mut trades: Vec<TradeRecord> = Vec::new();
    let mut fills: Vec<FillRecord> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut trial_sharpe: Vec<f64> = Vec::new();

    for window in &plan.windows {
        let train_range = IndexRange::with_warmup(
            window.train_start.saturating_sub(wf.warmup_bars),
            window.train_start,
            window.train_end,
        );
        let mut tasks = sweep::run_grid(&pool, &grid, universe, train_range, wf.objective)?;
        sweep::sort_tasks(&mut tasks);
        let Some(best) = tasks
            .iter()
            .find(|task| task.objective_value.is_some())
            .cloned()
        else {
            skipped.push(format!("窗口 {} 训练集内没有可用参数,已跳过", window.index));
            continue;
        };
        trial_sharpe.extend(tasks.iter().filter_map(|task| task.period_sharpe));
        if window.train_start == 0 {
            warnings.push(format!(
                "窗口 {} 训练区间从 0 开始,没有预热历史",
                window.index
            ));
        }

        let test_range = IndexRange::with_warmup(
            window.test_start.saturating_sub(wf.warmup_bars),
            window.test_start,
            window.test_end,
        );
        let combo_config = best_config(config, &best.params)?;
        let mut strategy = build(&combo_config.strategy, universe.symbol_count());
        let input = sweep::engine_input(&combo_config, workers.threads, test_range);
        let test = run_backtest(universe, &input, strategy.as_mut())?;
        warnings.extend(test.warnings.iter().cloned());
        trades.extend(test.trades.iter().cloned());
        fills.extend(test.fills.iter().cloned());
        segments.push((test.equity.clone(), config.initial_cash));

        windows.push(WalkForwardWindow {
            index: window.index,
            train_start: window.train_start,
            train_end: window.train_end,
            test_start: window.test_start,
            test_end: window.test_end,
            train_start_ts: universe.timeline[window.train_start],
            train_end_ts: universe.timeline[window.train_end - 1],
            test_start_ts: universe.timeline[window.test_start],
            test_end_ts: universe.timeline[window.test_end - 1],
            best_params: best.params.clone(),
            is_metrics: best.metrics.clone(),
            oos_metrics: test.metrics.clone(),
            warnings: test.warnings.clone(),
        });
    }

    if windows.is_empty() {
        return Err(ConfigError::Invalid {
            field: "walk_forward".into(),
            message: format!("没有可用窗口:{}", skipped.join("; ")),
        }
        .into());
    }

    let segment_slices: Vec<(&[backtest_core::model::EquityPoint], f64)> = segments
        .iter()
        .map(|(equity, cash)| (equity.as_slice(), *cash))
        .collect();
    let oos_equity = stitch_oos(config.initial_cash, &segment_slices);
    let ppy = config.periods_per_year();
    let oos_metrics = compute_metrics(&MetricInput {
        equity: &oos_equity,
        trades: &trades,
        fills: &fills,
        initial_cash: config.initial_cash,
        periods_per_year: ppy,
        risk_free_rate: config.risk_free_rate,
    });
    let dsr = deflated_sharpe(
        &equity_returns(&oos_equity, config.initial_cash),
        &trial_sharpe,
    );
    let stability = param_stability(&windows);
    let elapsed_ms = started.elapsed().as_millis();
    let result = WalkForwardResult {
        mode: wf.mode,
        train_bars: wf.train_bars,
        test_bars: wf.test_bars,
        step_bars: wf.step_bars,
        embargo_bars: wf.embargo_bars,
        warmup_bars: wf.warmup_bars,
        objective: wf.objective.name().to_string(),
        windows,
        oos_equity: oos_equity.clone(),
        oos_metrics: oos_metrics.clone(),
        param_stability: stability,
        skipped_windows: skipped.clone(),
        gaps,
        dsr,
        elapsed_ms,
    };

    let base = build_base_result(
        config,
        universe,
        &result,
        workers.threads,
        trades,
        fills,
        warnings,
    );
    Ok(WalkForwardRun { base, result })
}

fn best_config(
    base: &RunConfig,
    params: &BTreeMap<String, serde_json::Value>,
) -> Result<RunConfig> {
    let mut strategy = base.strategy.clone();
    for (name, value) in params {
        strategy.apply_param(name, value)?;
    }
    base.with_strategy(strategy)
}

fn build_base_result(
    config: &RunConfig,
    universe: &Universe,
    result: &WalkForwardResult,
    threads: usize,
    trades: Vec<TradeRecord>,
    fills: Vec<FillRecord>,
    warnings: Vec<String>,
) -> BacktestResult {
    let meta_params: BTreeMap<String, serde_json::Value> = result
        .param_stability
        .iter()
        .map(|series| (series.name.clone(), serde_json::json!("每窗选参")))
        .collect();
    let drawdowns = drawdown_episodes(&result.oos_equity, config.initial_cash, 5);
    let monthly = monthly_returns(&result.oos_equity, config.initial_cash);
    let mut all_warnings = warnings;
    all_warnings.extend(result.skipped_windows.iter().cloned());
    let meta = RunMeta {
        strategy: config.strategy.name().to_string(),
        params: meta_params,
        symbols: config
            .instruments
            .iter()
            .map(|spec| spec.symbol.clone())
            .collect(),
        requested_start: config.start,
        requested_end: config.end,
        first_bar_ts: result.oos_equity.first().map(|point| point.ts),
        last_bar_ts: result.oos_equity.last().map(|point| point.ts),
        timeframe: config.timeframe.to_string(),
        config_hash: config.config_hash.clone(),
        data_hash: universe.data_hash.clone(),
        threads,
        walk_forward_window: None,
        engine_version: env!("CARGO_PKG_VERSION").into(),
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        warnings: all_warnings.clone(),
    };
    BacktestResult {
        meta,
        equity: result.oos_equity.clone(),
        fills,
        trades,
        metrics: result.oos_metrics.clone(),
        drawdowns,
        monthly,
        warnings: all_warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::system::ResolvedWorkers;
    use std::path::{Path, PathBuf};

    fn fixture_config() -> (tempfile::TempDir, PathBuf) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let data = root.join("fixtures/s5_daily.csv");
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("walkforward.toml");
        let text = format!(
            r#"[run]
data = "{}"
initial_cash = 100000.0
start = "2020-01-01T00:00:00Z"
end = "2021-03-01T00:00:00Z"
output_dir = "{}"
threads = 2
memory_limit_gb = 0
timeframe = "1d"

[strategy]
name = "sma_cross"
fast = 5
slow = 20
weight = 0.99
allow_short = false

[sweep]
objective = "sharpe"
top_k = 5
oos_split = 0.7

[sweep.grid]
fast = [3, 5, 8, 10]
slow = [13, 20, 30, 40]

[walk_forward]
mode = "rolling"
train_bars = 180
test_bars = 60
step_bars = 60
embargo_bars = 1
warmup_bars = 40
objective = "sharpe"

[[instruments]]
symbol = "TEST"
asset_class = "crypto"
currency = "USD"
multiplier = 1.0
lot_size = 0.0
margin_rate = 1.0
fee_bps = 2.0
fee_fixed = 0.0
slippage_bps = 1.0
periods_per_year = 365
"#,
            data.display(),
            dir.path().join("out").display()
        );
        std::fs::write(&config_path, text).expect("write config");
        (dir, config_path)
    }

    fn run(path: &Path) -> WalkForwardRun {
        let config = backtest_core::config::load_config(path).expect("config");
        let wf = config.walk_forward.clone().expect("walk_forward");
        let workers = ResolvedWorkers {
            threads: 2,
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
        })
        .expect("universe");
        execute(&config, &wf, &universe, &workers).expect("walkforward")
    }

    #[test]
    fn t5_9_t5_11_t5_12_windows_stitch_and_hash_are_stable() {
        let (_dir, path) = fixture_config();
        let first = run(&path);
        let second = run(&path);
        let result = &first.result;

        assert_eq!(result.windows.len(), 3);
        assert_eq!(result.skipped_windows.len(), 1);
        let expected = [
            (0, 180, 181, 241),
            (60, 240, 241, 301),
            (120, 300, 301, 361),
        ];
        for (window, (train_start, train_end, test_start, test_end)) in
            result.windows.iter().zip(expected)
        {
            assert_eq!(window.train_start, train_start);
            assert_eq!(window.train_end, train_end);
            assert_eq!(window.test_start, test_start);
            assert_eq!(window.test_end, test_end);
            assert!(!window.best_params.is_empty());
            assert!(window.is_metrics.sharpe.is_some());
            assert!(window.oos_metrics.exposure.is_some());
        }
        assert_eq!(result.oos_equity.len(), 180);
        assert_eq!(result.oos_equity[0].equity, 100_000.0);
        assert!(result.oos_metrics.exposure.unwrap() > 0.0);
        assert_eq!(
            result
                .param_stability
                .iter()
                .map(|series| (series.name.as_str(), series.values.len()))
                .collect::<Vec<_>>(),
            vec![("fast", 3), ("slow", 3)]
        );
        let dsr = result.dsr.as_ref().expect("dsr");
        assert_eq!(dsr.trials, 48);

        assert_eq!(
            backtest_report::content_hash(&first.base).expect("hash"),
            backtest_report::content_hash(&second.base).expect("hash")
        );
        assert_eq!(
            result.windows[0].best_params,
            second.result.windows[0].best_params
        );
        assert_eq!(result.oos_equity.len(), second.result.oos_equity.len());
    }
}
