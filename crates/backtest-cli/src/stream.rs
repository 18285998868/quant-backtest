//! 分块流式装载与回放编排。
//!
//! 数据规划阶段只保留时间线、资金费与哈希;每次窗口按绝对下标重新装载 OHLCV,
//! 引擎状态跨窗口延续。窗口大小与 `config_hash` 无关,同一份数据无论怎么切块
//! 都应得到相同结果。

use backtest_core::config::RunConfig;
use backtest_core::engine::{BacktestStats, run_backtest_stream, run_backtest_stream_stats};
use backtest_core::error::Result;
use backtest_core::model::BacktestResult;
use backtest_data::{
    DataPlan, LoadDiagnostics, LoadOptions, LoadSource, load_window, plan_universe_reporting,
};
use backtest_strategy::build;

use crate::sweep::IndexRange;

pub(crate) fn load_options(config: &RunConfig) -> LoadOptions<'_> {
    LoadOptions {
        source: LoadSource::from(&config.data),
        timeframe: config.timeframe,
        instruments: &config.instruments,
        start: config.start,
        end: config.end,
    }
}

/// 扫描数据并计算 v2 `data_hash`,不保留 OHLCV。
pub(crate) fn plan_data(config: &RunConfig) -> Result<(DataPlan, LoadDiagnostics)> {
    plan_universe_reporting(&load_options(config))
}

/// 时间线与资金费之外的固定常驻字节数。
fn fixed_bytes(plan: &DataPlan) -> u64 {
    let timeline = plan.len() as u64 * std::mem::size_of::<i64>() as u64;
    let funding = plan
        .funding
        .iter()
        .map(|item| {
            item.len() as u64 * (std::mem::size_of::<i64>() + std::mem::size_of::<f64>()) as u64
        })
        .sum::<u64>();
    timeline + funding
}

/// 选择窗口大小。`None` 表示整段装载。
///
/// - `requested > 0`:强制分块,窗口不超过数据长度;
/// - 否则仅在 `memory_limit_gb > 0` 且全量估算超过上限时自动分块;
/// - 自动分块按并行 worker 数均分预算,每个窗口只占单 worker 的一半,
///   另一半留给归档装载峰值与结果对象。
pub(crate) fn choose_chunk(
    requested: usize,
    memory_limit_gb: f64,
    plan: &DataPlan,
    limit_bytes: Option<u64>,
    parallelism: usize,
    history_bars: usize,
) -> Option<usize> {
    if plan.len() < 2 {
        return None;
    }
    if requested > 0 {
        let size = requested.min(plan.len());
        return (size > 0 && size < plan.len()).then_some(size);
    }
    if memory_limit_gb <= 0.0 {
        return None;
    }
    let limit = limit_bytes?;
    if plan.estimated_data_bytes() <= limit {
        return None;
    }
    let per_worker = (limit / parallelism.max(1) as u64).max(1);
    let budget = per_worker / 2;
    let available = budget.saturating_sub(fixed_bytes(plan));
    let per_bar = plan.bytes_per_bar().max(1);
    let bars = (available / per_bar) as usize;
    let size = bars.saturating_sub(history_bars).clamp(1, plan.len());
    (size < plan.len()).then_some(size)
}

/// 逐窗口装载并回放。窗口按绝对下标连续推进,块首额外携带策略历史。
pub(crate) fn run_streaming(
    config: &RunConfig,
    plan: &DataPlan,
    threads: usize,
    range: IndexRange,
    chunk_bars: usize,
) -> Result<BacktestResult> {
    let mut strategy = build(&config.strategy, plan.symbol_count());
    let history = strategy.history_bars();
    let input = crate::sweep::engine_input(config, threads, range);
    let options = load_options(config);
    let mut cursor = range.start;
    run_backtest_stream(&input, strategy.as_mut(), || {
        next_stream_window(plan, &options, range, chunk_bars, history, &mut cursor)
    })
}

/// 分块回放,只保留指标与逐期 Sharpe。
pub(crate) fn run_streaming_stats(
    config: &RunConfig,
    plan: &DataPlan,
    threads: usize,
    range: IndexRange,
    chunk_bars: usize,
) -> Result<BacktestStats> {
    let mut strategy = build(&config.strategy, plan.symbol_count());
    let history = strategy.history_bars();
    let input = crate::sweep::engine_input(config, threads, range);
    let options = load_options(config);
    let mut cursor = range.start;
    run_backtest_stream_stats(&input, strategy.as_mut(), || {
        next_stream_window(plan, &options, range, chunk_bars, history, &mut cursor)
    })
}

fn next_stream_window(
    plan: &DataPlan,
    options: &LoadOptions<'_>,
    range: IndexRange,
    chunk_bars: usize,
    history: usize,
    cursor: &mut usize,
) -> Result<Option<backtest_core::model::Universe>> {
    if *cursor >= range.end {
        return Ok(None);
    }
    let end = (*cursor).saturating_add(chunk_bars.max(1)).min(range.end);
    let window = load_window(plan, options, *cursor, end, history)?;
    *cursor = end;
    Ok(Some(window))
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::model::{AssetClass, InstrumentSpec, Timeframe};

    fn plan(bars: usize, symbols: usize) -> DataPlan {
        let specs = (0..symbols)
            .map(|index| InstrumentSpec {
                symbol: format!("S{index}"),
                asset_class: AssetClass::Crypto,
                currency: "USD".into(),
                multiplier: 1.0,
                lot_size: 0.0,
                margin_rate: 1.0,
                fee_bps: 0.0,
                fee_fixed: 0.0,
                slippage_bps: 0.0,
                periods_per_year: Some(365),
            })
            .collect::<Vec<_>>();
        DataPlan {
            timeframe: Timeframe::ONE_DAY,
            specs,
            timeline: (0..bars as i64).collect(),
            funding: Vec::new(),
            counts: vec![bars; symbols],
            data_hash: "test".into(),
        }
    }

    #[test]
    fn explicit_chunk_is_clamped_to_data_length() {
        let plan = plan(1_000, 1);
        assert_eq!(choose_chunk(100, 0.0, &plan, None, 1, 0), Some(100));
        assert_eq!(choose_chunk(1_000, 0.0, &plan, None, 1, 0), None);
        assert_eq!(choose_chunk(5_000, 0.0, &plan, None, 1, 0), None);
        assert_eq!(choose_chunk(1, 0.0, &plan, None, 1, 0), Some(1));
    }

    #[test]
    fn auto_chunk_requires_explicit_memory_limit_and_overflow() {
        let plan = plan(1_000, 1);
        let full = plan.estimated_data_bytes();
        assert_eq!(choose_chunk(0, 0.0, &plan, Some(full / 2), 1, 0), None);
        assert_eq!(choose_chunk(0, 1.0, &plan, Some(full * 2), 1, 0), None);
        assert_eq!(choose_chunk(0, 1.0, &plan, None, 1, 0), None);
        let chunk = choose_chunk(0, 1.0, &plan, Some(full / 2), 1, 0).expect("chunk");
        assert!(chunk > 0 && chunk < plan.len());
    }

    #[test]
    fn auto_chunk_accounts_for_history_and_workers() {
        let plan = plan(10_000, 4);
        let limit = plan.estimated_data_bytes() / 4;
        let one = choose_chunk(0, 1.0, &plan, Some(limit), 1, 0).expect("one");
        let many = choose_chunk(0, 1.0, &plan, Some(limit), 4, 0).expect("many");
        assert!(many < one, "多 worker 应得到更小的窗口: {many} vs {one}");
        let with_history = choose_chunk(0, 1.0, &plan, Some(limit), 1, one / 2).expect("history");
        assert!(with_history <= one / 2);
    }
}
