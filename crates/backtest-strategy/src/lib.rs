//! 内置策略与策略工厂。

use std::collections::VecDeque;

use backtest_core::config::StrategyConfig;
use backtest_core::model::SymbolId;
use backtest_core::strategy::{MarketContext, Strategy, TargetPosition};

/// 双均线:快线上穿慢线做多,下穿时平仓或做空。
///
/// 首次得到有效均线时只记录状态,不立即入场;之后只在多空状态变化时输出目标权重,
/// 避免持仓自然漂移时每根 bar 都触发再平衡。
#[derive(Debug)]
pub struct SmaCross {
    fast: usize,
    slow: usize,
    weight: f64,
    allow_short: bool,
    states: Vec<SmaState>,
}

#[derive(Debug, Default)]
struct SmaState {
    fast_sum: f64,
    slow_sum: f64,
    fast_buf: VecDeque<f64>,
    slow_buf: VecDeque<f64>,
    prev_regime: i8,
    initialized: bool,
}

impl SmaCross {
    pub fn new(fast: usize, slow: usize, weight: f64, allow_short: bool, symbols: usize) -> Self {
        Self {
            fast,
            slow,
            weight,
            allow_short,
            states: (0..symbols).map(|_| SmaState::default()).collect(),
        }
    }

    fn signal(&mut self, symbol: SymbolId, ctx: &MarketContext<'_>) -> Option<f64> {
        let state = self.states.get_mut(symbol as usize)?;
        let bar = ctx.universe.try_bar(symbol, ctx.index)?;
        state.fast_sum += bar.close;
        state.fast_buf.push_back(bar.close);
        if state.fast_buf.len() > self.fast
            && let Some(old) = state.fast_buf.pop_front()
        {
            state.fast_sum -= old;
        }
        state.slow_sum += bar.close;
        state.slow_buf.push_back(bar.close);
        if state.slow_buf.len() > self.slow
            && let Some(old) = state.slow_buf.pop_front()
        {
            state.slow_sum -= old;
        }
        if state.slow_buf.len() < self.slow || state.fast_buf.len() < self.fast {
            return None;
        }
        let fast_avg = state.fast_sum / self.fast as f64;
        let slow_avg = state.slow_sum / self.slow as f64;
        let regime = if fast_avg > slow_avg {
            1
        } else if fast_avg < slow_avg {
            -1
        } else {
            0
        };
        if regime == 0 {
            return None;
        }
        if !state.initialized {
            state.initialized = true;
            state.prev_regime = regime;
            return None;
        }
        if regime == state.prev_regime {
            return None;
        }
        state.prev_regime = regime;
        Some(match regime {
            1 => self.weight,
            _ if self.allow_short => -self.weight,
            _ => 0.0,
        })
    }
}

impl Strategy for SmaCross {
    fn on_bar(&mut self, ctx: &MarketContext<'_>, out: &mut Vec<TargetPosition>) {
        for symbol in 0..self.states.len() {
            if let Some(weight) = self.signal(symbol as SymbolId, ctx) {
                out.push(TargetPosition {
                    symbol: symbol as SymbolId,
                    weight,
                });
            }
        }
    }

    fn warmup_bars(&self) -> usize {
        self.slow
    }
}

/// 根据类型化配置构造策略。
pub fn build(config: &StrategyConfig, symbols: usize) -> Box<dyn Strategy> {
    match config {
        StrategyConfig::SmaCross {
            fast,
            slow,
            weight,
            allow_short,
        } => Box::new(SmaCross::new(*fast, *slow, *weight, *allow_short, symbols)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::model::{AssetClass, BarSeries, InstrumentSpec, Timeframe, Universe};
    use backtest_core::strategy::PositionView;

    fn universe(closes: &[f64]) -> Universe {
        let n = closes.len();
        let base = 1_700_000_000_000_i64;
        let series = BarSeries {
            symbol: 0,
            ts: (0..n).map(|i| base + i as i64 * 86_400_000).collect(),
            open: closes.to_vec(),
            high: closes.iter().map(|c| c + 1.0).collect(),
            low: closes.iter().map(|c| c - 1.0).collect(),
            close: closes.to_vec(),
            volume: vec![1_000.0; n],
        };
        Universe {
            timeframe: Timeframe::ONE_DAY,
            specs: vec![InstrumentSpec {
                symbol: "T".into(),
                asset_class: AssetClass::Crypto,
                currency: "USD".into(),
                multiplier: 1.0,
                lot_size: 0.0,
                margin_rate: 1.0,
                fee_bps: 0.0,
                fee_fixed: 0.0,
                slippage_bps: 0.0,
                periods_per_year: Some(365),
            }],
            timeline: series.ts.clone(),
            series: vec![series],
            data_hash: String::new(),
        }
    }

    #[test]
    fn first_cross_is_the_only_initial_signal() {
        let u = universe(&[100.0, 99.0, 98.0, 100.0, 102.0, 103.0]);
        let mut strategy = SmaCross::new(2, 3, 0.99, true, 1);
        let positions = [PositionView {
            symbol: 0,
            qty: 0.0,
            avg_entry: 0.0,
            mark: 0.0,
            stop: None,
            take_profit: None,
        }];
        let mut all = Vec::new();
        for index in 0..u.len() {
            let ctx = MarketContext {
                ts: u.timeline[index],
                index,
                universe: &u,
                cash: 100_000.0,
                equity: 100_000.0,
                positions: &positions,
            };
            let mut out = Vec::new();
            strategy.on_bar(&ctx, &mut out);
            for target in out {
                all.push((index, target.weight));
            }
        }
        assert_eq!(all, vec![(4, 0.99)]);
    }
}
