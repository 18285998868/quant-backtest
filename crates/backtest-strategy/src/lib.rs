//! 内置策略与策略工厂。

use std::collections::VecDeque;

use backtest_core::config::{StrategyConfig, Weighting};
use backtest_core::model::SymbolId;
use backtest_core::strategy::{
    MarketContext, Strategy, TargetPosition, inverse_vol_weights_checked,
};

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
        let close = ctx.universe.try_close(symbol, ctx.index)?;
        state.fast_sum += close;
        state.fast_buf.push_back(close);
        if state.fast_buf.len() > self.fast
            && let Some(old) = state.fast_buf.pop_front()
        {
            state.fast_sum -= old;
        }
        state.slow_sum += close;
        state.slow_buf.push_back(close);
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

/// 截面动量:按 `lookback` 收益选 top_k,方向由动量符号决定,按逆波动率分配权重。
///
/// 只在调仓 bar 输出;输出覆盖全部标的,未入选的权重为 0,用于平掉掉出组合的持仓。
/// 非调仓 bar 不输出,持仓按价格自然漂移。
#[derive(Debug)]
pub struct XsMomentum {
    lookback: usize,
    top_k: usize,
    rebalance_bars: usize,
    weighting: Weighting,
    gross: f64,
    warmup: usize,
    closes: Vec<VecDeque<f64>>,
    warnings: Vec<String>,
}

impl XsMomentum {
    pub fn new(
        lookback: usize,
        top_k: usize,
        rebalance_bars: usize,
        weighting: Weighting,
        gross: f64,
        symbols: usize,
    ) -> Self {
        let vol_window = match weighting {
            Weighting::InverseVol { lookback, .. } => lookback,
            Weighting::Equal => 0,
        };
        let warmup = lookback.max(vol_window);
        let history_len = warmup + 1;
        Self {
            lookback,
            top_k,
            rebalance_bars,
            weighting,
            gross,
            warmup,
            closes: (0..symbols)
                .map(|_| VecDeque::with_capacity(history_len))
                .collect(),
            warnings: Vec::new(),
        }
    }

    fn record(&mut self, symbol: usize, ctx: &MarketContext<'_>) {
        let Some(close) = ctx.universe.try_close(symbol as SymbolId, ctx.index) else {
            return;
        };
        let Some(history) = self.closes.get_mut(symbol) else {
            return;
        };
        history.push_back(close);
        if history.len() > self.warmup + 1 {
            history.pop_front();
        }
    }

    fn momentum(&self, symbol: usize) -> Option<f64> {
        let closes = self.closes.get(symbol)?;
        if closes.len() < self.lookback + 1 {
            return None;
        }
        let last = *closes.back()?;
        let base = closes[closes.len() - 1 - self.lookback];
        let momentum = last / base - 1.0;
        momentum.is_finite().then_some(momentum)
    }

    /// 对数收益的样本标准差(ddof=1);不足 2 个收益或 σ 非正时返回 None。
    fn realized_vol(&self, symbol: usize, window: usize) -> Option<f64> {
        let closes = self.closes.get(symbol)?;
        if window < 2 || closes.len() < window + 1 {
            return None;
        }
        let start = closes.len() - window - 1;
        let mut returns = Vec::with_capacity(window);
        let mut prev = closes[start];
        for &value in closes.iter().skip(start + 1) {
            let ret = (value / prev).ln();
            if !ret.is_finite() {
                return None;
            }
            returns.push(ret);
            prev = value;
        }
        let mean = returns.iter().sum::<f64>() / window as f64;
        let squares: f64 = returns.iter().map(|ret| (ret - mean) * (ret - mean)).sum();
        let variance = squares / (window as f64 - 1.0);
        let sigma = variance.sqrt();
        (sigma.is_finite() && sigma > 0.0).then_some(sigma)
    }

    fn rebalance(&mut self, ctx: &MarketContext<'_>, out: &mut Vec<TargetPosition>) {
        let mut candidates = Vec::new();
        for symbol in 0..self.closes.len() {
            if let Some(momentum) = self.momentum(symbol) {
                candidates.push((symbol, momentum));
            }
        }
        if candidates.is_empty() {
            return;
        }
        candidates.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });
        let selected: Vec<(usize, f64)> = candidates.into_iter().take(self.top_k).collect();
        let directions: Vec<i8> = selected
            .iter()
            .map(|(_, momentum)| {
                if *momentum > 0.0 {
                    1
                } else if *momentum < 0.0 {
                    -1
                } else {
                    0
                }
            })
            .collect();
        let weights = match self.weighting {
            Weighting::Equal => {
                let active = directions
                    .iter()
                    .filter(|direction| **direction != 0)
                    .count();
                if active == 0 {
                    vec![0.0; selected.len()]
                } else {
                    let each = self.gross / active as f64;
                    directions
                        .iter()
                        .map(|direction| *direction as f64 * each)
                        .collect()
                }
            }
            Weighting::InverseVol { lookback, floor } => {
                let vols: Vec<f64> = selected
                    .iter()
                    .map(|(symbol, _)| self.realized_vol(*symbol, lookback).unwrap_or(f64::NAN))
                    .collect();
                let result = inverse_vol_weights_checked(&vols, &directions, self.gross, floor);
                if let Some(reason) = result.fallback {
                    let names: Vec<&str> = selected
                        .iter()
                        .map(|(symbol, _)| ctx.universe.specs[*symbol].symbol.as_str())
                        .collect();
                    self.warnings.push(format!(
                        "ts={} xs_momentum 逆波动率回退等权({}):标的 {}",
                        ctx.ts,
                        reason.message(),
                        names.join(",")
                    ));
                }
                result.weights
            }
        };

        for symbol in 0..self.closes.len() {
            let weight = selected
                .iter()
                .position(|(selected_symbol, _)| *selected_symbol == symbol)
                .map(|index| weights[index])
                .unwrap_or(0.0);
            out.push(TargetPosition {
                symbol: symbol as SymbolId,
                weight,
            });
        }
    }
}

impl Strategy for XsMomentum {
    fn on_bar(&mut self, ctx: &MarketContext<'_>, out: &mut Vec<TargetPosition>) {
        for symbol in 0..self.closes.len() {
            self.record(symbol, ctx);
        }
        if self.rebalance_bars == 0 || !ctx.index.is_multiple_of(self.rebalance_bars) {
            return;
        }
        self.rebalance(ctx, out);
    }

    fn warmup_bars(&self) -> usize {
        self.warmup
    }

    fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
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
        StrategyConfig::XsMomentum {
            lookback,
            top_k,
            rebalance_bars,
            weighting,
            gross,
        } => Box::new(XsMomentum::new(
            *lookback,
            *top_k,
            *rebalance_bars,
            *weighting,
            *gross,
            symbols,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::model::{AssetClass, BarSeries, InstrumentSpec, Timeframe, Ts, Universe};
    use backtest_core::strategy::PositionView;

    const DAY: i64 = 86_400_000;

    fn universe(closes: &[f64]) -> Universe {
        universe_multi(&[closes])
    }

    fn universe_multi(series: &[&[f64]]) -> Universe {
        assert!(!series.is_empty());
        let n = series[0].len();
        assert!(series.iter().all(|closes| closes.len() == n));
        let base = 1_700_000_000_000_i64;
        let ts: Vec<Ts> = (0..n).map(|i| base + i as i64 * DAY).collect();
        let specs = series
            .iter()
            .enumerate()
            .map(|(index, _)| InstrumentSpec {
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
            .collect();
        let bars = series
            .iter()
            .enumerate()
            .map(|(index, closes)| BarSeries {
                symbol: index as u32,
                ts: ts.clone(),
                open: closes.to_vec(),
                high: closes.iter().map(|close| close + 1.0).collect(),
                low: closes.iter().map(|close| close - 1.0).collect(),
                close: closes.to_vec(),
                volume: vec![1_000_000.0; n],
            })
            .collect();
        Universe {
            timeframe: Timeframe::ONE_DAY,
            specs,
            series: bars,
            funding: Vec::new(),
            timeline: ts,
            data_hash: String::new(),
            base_index: 0,
        }
    }

    fn returns_with(total_log: f64, vol: f64, n: usize) -> Vec<f64> {
        let mean = total_log / n as f64;
        let deviation = vol * (((n - 1) as f64) / n as f64).sqrt();
        (0..n)
            .map(|index| {
                if index % 2 == 0 {
                    mean + deviation
                } else {
                    mean - deviation
                }
            })
            .collect()
    }

    fn closes_from_returns(returns: &[f64]) -> Vec<f64> {
        let mut closes = Vec::with_capacity(returns.len() + 1);
        let mut price = 100.0;
        closes.push(price);
        for ret in returns {
            price *= ret.exp();
            closes.push(price);
        }
        closes
    }

    fn run_xs(strategy: &mut XsMomentum, universe: &Universe) -> Vec<(usize, Vec<(u32, f64)>)> {
        let positions: Vec<PositionView> = universe
            .specs
            .iter()
            .enumerate()
            .map(|(index, _)| PositionView {
                symbol: index as u32,
                qty: 0.0,
                avg_entry: 0.0,
                mark: 0.0,
                stop: None,
                take_profit: None,
            })
            .collect();
        let mut outputs = Vec::new();
        for index in 0..universe.len() {
            let ctx = MarketContext {
                ts: universe.timeline[index],
                index,
                universe,
                cash: 100_000.0,
                equity: 100_000.0,
                positions: &positions,
            };
            let mut out = Vec::new();
            strategy.on_bar(&ctx, &mut out);
            if !out.is_empty() {
                outputs.push((
                    index,
                    out.iter()
                        .map(|target| (target.symbol, target.weight))
                        .collect(),
                ));
            }
        }
        outputs
    }

    fn weight_of(weights: &[(u32, f64)], symbol: u32) -> f64 {
        weights
            .iter()
            .find(|(candidate, _)| *candidate == symbol)
            .map(|(_, weight)| *weight)
            .expect("symbol must be present")
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

    #[test]
    fn t3_6_top_k_selection_and_inverse_vol_weights() {
        let a = closes_from_returns(&returns_with((1.03_f64).ln(), 0.01, 20));
        let b = closes_from_returns(&returns_with((1.02_f64).ln(), 0.02, 20));
        let c = closes_from_returns(&returns_with((1.01_f64).ln(), 0.015, 20));
        let universe = universe_multi(&[&a, &b, &c]);
        let mut strategy = XsMomentum::new(
            20,
            2,
            1,
            Weighting::InverseVol {
                lookback: 20,
                floor: 1e-4,
            },
            1.0,
            3,
        );

        let outputs = run_xs(&mut strategy, &universe);
        let (index, weights) = outputs.last().expect("rebalance output");
        assert_eq!(*index, 20);
        assert_eq!(weights.len(), 3);
        assert!((weight_of(weights, 0) - 2.0 / 3.0).abs() < 1e-9);
        assert!((weight_of(weights, 1) - 1.0 / 3.0).abs() < 1e-9);
        assert!(weight_of(weights, 2).abs() < 1e-12);
        let gross: f64 = weights.iter().map(|(_, weight)| weight.abs()).sum();
        assert!((gross - 1.0).abs() < 1e-9);
        assert!(strategy.take_warnings().is_empty());
    }

    #[test]
    fn t3_7_inverse_vol_keeps_long_short_directions() {
        let a = closes_from_returns(&returns_with((1.03_f64).ln(), 0.01, 20));
        let b = closes_from_returns(&returns_with((0.98_f64).ln(), 0.02, 20));
        let c = closes_from_returns(&returns_with((0.95_f64).ln(), 0.015, 20));
        let universe = universe_multi(&[&a, &b, &c]);
        let mut strategy = XsMomentum::new(
            20,
            2,
            1,
            Weighting::InverseVol {
                lookback: 20,
                floor: 1e-4,
            },
            1.0,
            3,
        );

        let outputs = run_xs(&mut strategy, &universe);
        let (_, weights) = outputs.last().expect("rebalance output");
        assert!((weight_of(weights, 0) - 2.0 / 3.0).abs() < 1e-9);
        assert!((weight_of(weights, 1) + 1.0 / 3.0).abs() < 1e-9);
        assert!(weight_of(weights, 2).abs() < 1e-12);
        let gross: f64 = weights.iter().map(|(_, weight)| weight.abs()).sum();
        assert!((gross - 1.0).abs() < 1e-9);
    }

    #[test]
    fn t3_8_zero_vol_falls_back_to_equal_weights_with_warning() {
        let flat_a = vec![(1.03_f64).ln() / 20.0; 20];
        let flat_b = vec![(1.02_f64).ln() / 20.0; 20];
        let a = closes_from_returns(&flat_a);
        let b = closes_from_returns(&flat_b);
        let universe = universe_multi(&[&a, &b]);
        let mut strategy = XsMomentum::new(
            20,
            2,
            1,
            Weighting::InverseVol {
                lookback: 20,
                floor: 1e-4,
            },
            1.0,
            2,
        );

        let outputs = run_xs(&mut strategy, &universe);
        let (_, weights) = outputs.last().expect("rebalance output");
        assert!((weight_of(weights, 0) - 0.5).abs() < 1e-12);
        assert!((weight_of(weights, 1) - 0.5).abs() < 1e-12);
        let warnings = strategy.take_warnings();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("回退") && warning.contains("波动率"))
        );
    }

    #[test]
    fn t3_8_insufficient_vol_history_falls_back_to_equal_weights() {
        let a = closes_from_returns(&[0.10, 0.05]);
        let b = closes_from_returns(&[0.02, 0.01]);
        let universe = universe_multi(&[&a, &b]);
        let mut strategy = XsMomentum::new(
            2,
            2,
            1,
            Weighting::InverseVol {
                lookback: 5,
                floor: 1e-4,
            },
            1.0,
            2,
        );

        let outputs = run_xs(&mut strategy, &universe);
        let (index, weights) = outputs.last().expect("rebalance output");
        assert_eq!(*index, 2);
        assert!((weight_of(weights, 0) - 0.5).abs() < 1e-12);
        assert!((weight_of(weights, 1) - 0.5).abs() < 1e-12);
        assert!(
            strategy
                .take_warnings()
                .iter()
                .any(|warning| warning.contains("回退"))
        );
    }

    #[test]
    fn t3_9_only_rebalance_bars_emit_targets() {
        let a = [
            100.0, 101.0, 102.0, 103.0, 104.0, 103.0, 102.0, 101.0, 100.0, 99.0, 98.0,
        ];
        let b = [
            50.0, 49.0, 48.0, 47.0, 46.0, 47.0, 48.0, 49.0, 50.0, 51.0, 52.0,
        ];
        let universe = universe_multi(&[&a, &b]);
        let mut strategy = XsMomentum::new(2, 2, 5, Weighting::Equal, 1.0, 2);

        let outputs = run_xs(&mut strategy, &universe);
        let indexes: Vec<usize> = outputs.iter().map(|(index, _)| *index).collect();
        assert_eq!(indexes, vec![5, 10]);
        for (_, weights) in &outputs {
            assert_eq!(weights.len(), 2);
        }
    }

    #[test]
    fn non_rebalance_bars_keep_history_for_next_signal() {
        let a = [100.0, 101.0, 102.0, 103.0, 104.0, 105.0, 106.0];
        let b = [50.0, 49.0, 48.0, 47.0, 46.0, 45.0, 44.0];
        let universe = universe_multi(&[&a, &b]);
        let mut strategy = XsMomentum::new(2, 2, 3, Weighting::Equal, 1.0, 2);

        let outputs = run_xs(&mut strategy, &universe);
        let indexes: Vec<usize> = outputs.iter().map(|(index, _)| *index).collect();
        assert_eq!(indexes, vec![3, 6]);
        let (_, weights) = &outputs[1];
        assert!((weight_of(weights, 0) - 0.5).abs() < 1e-12);
        assert!((weight_of(weights, 1) + 0.5).abs() < 1e-12);
    }
}
