//! T1.3:单标的缺失 bar 时,信号顺延到下一根可用 bar 的开盘成交。

use std::collections::BTreeMap;

use backtest_core::config::{ExecutionConfig, RiskConfig};
use backtest_core::engine::{EngineInput, run_backtest};
use backtest_core::model::{AssetClass, InstrumentSpec, Timeframe};
use backtest_core::strategy::{MarketContext, Strategy, TargetPosition};
use backtest_data::{LoadOptions, LoadSource, load_universe};

const DAY: i64 = 86_400_000;
const BASE_TS: i64 = 1_577_836_800_000;

struct SignalAt {
    index: usize,
}

impl Strategy for SignalAt {
    fn on_bar(&mut self, ctx: &MarketContext<'_>, out: &mut Vec<TargetPosition>) {
        if ctx.index == self.index {
            out.push(TargetPosition {
                symbol: 0,
                weight: 0.99,
            });
        }
    }
}

fn input() -> EngineInput {
    EngineInput {
        initial_cash: 100_000.0,
        risk_free_rate: 0.0,
        execution: ExecutionConfig::default(),
        risk: RiskConfig::default(),
        strategy_name: "signal_at".into(),
        params: BTreeMap::new(),
        config_hash: "test".into(),
        threads: 1,
        requested_start: None,
        requested_end: None,
        start_index: 0,
        trade_start_index: 0,
        end_index: usize::MAX,
        collect_daily_returns: false,
    }
}

#[test]
fn missing_bar_fills_at_next_available_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gap.csv");
    let mut body = String::from("ts,symbol,open,high,low,close,volume\n");
    // t0..t5 连续,t6 缺失,t7 open=95。
    for index in [0_i64, 1, 2, 3, 4, 5, 7] {
        let price = if index == 7 { 95.0 } else { 100.0 };
        body.push_str(&format!(
            "{},{},{price},{price},{price},{price},1000\n",
            BASE_TS + index * DAY,
            "GAP"
        ));
    }
    std::fs::write(&path, body).expect("write csv");

    let spec = InstrumentSpec {
        symbol: "GAP".into(),
        asset_class: AssetClass::Crypto,
        currency: "USD".into(),
        multiplier: 1.0,
        lot_size: 0.0,
        margin_rate: 1.0,
        fee_bps: 0.0,
        fee_fixed: 0.0,
        slippage_bps: 0.0,
        periods_per_year: Some(365),
    };
    let universe = load_universe(&LoadOptions {
        source: LoadSource::Csv(&path),
        timeframe: Timeframe::ONE_DAY,
        instruments: std::slice::from_ref(&spec),
        start: None,
        end: None,
    })
    .expect("load");

    // 加载器不填补缺失日期:t6 不存在,timeline 直接跳到 t7。
    assert_eq!(universe.len(), 7);
    assert_eq!(universe.timeline[5], BASE_TS + 5 * DAY);
    assert_eq!(universe.timeline[6], BASE_TS + 7 * DAY);

    let mut strategy = SignalAt { index: 5 };
    let result = run_backtest(&universe, &input(), &mut strategy).expect("run");

    assert_eq!(result.fills.len(), 1);
    assert_eq!(result.fills[0].ts, BASE_TS + 7 * DAY);
    assert!((result.fills[0].price - 95.0).abs() < 1e-12);
    assert!((result.fills[0].qty - 99_000.0 / 95.0).abs() < 1e-9);
}
