//! 策略契约。具体策略实现放在 `backtest-strategy`。

use crate::model::{SymbolId, Ts, Universe};

#[derive(Clone, Copy, Debug)]
pub struct TargetPosition {
    pub symbol: SymbolId,
    pub weight: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct PositionView {
    pub symbol: SymbolId,
    pub qty: f64,
    pub avg_entry: f64,
    pub mark: f64,
    pub stop: Option<f64>,
    pub take_profit: Option<f64>,
}

#[derive(Debug)]
pub struct MarketContext<'a> {
    pub ts: Ts,
    pub index: usize,
    pub universe: &'a Universe,
    pub cash: f64,
    pub equity: f64,
    pub positions: &'a [PositionView],
}

pub trait Strategy: Send {
    fn on_bar(&mut self, ctx: &MarketContext<'_>, out: &mut Vec<TargetPosition>);

    /// 策略声明的最长指标窗口。walk-forward 预热与配置校验会使用。
    fn warmup_bars(&self) -> usize {
        0
    }
}
