use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};

use crate::config::{ExecutionConfig, RiskConfig, Trigger};
use crate::error::{ConfigError, ExecutionError, Result};
use crate::metrics::{MetricInput, compute_metrics, drawdown_episodes, monthly_returns};
use crate::model::{
    BacktestResult, EquityPoint, FillReason, FillRecord, RunMeta, Side, TradeRecord, Ts, Universe,
};
use crate::strategy::{MarketContext, PositionView, Strategy, TargetPosition};

#[derive(Debug)]
pub struct EngineInput {
    pub initial_cash: f64,
    pub risk_free_rate: f64,
    pub execution: ExecutionConfig,
    pub risk: RiskConfig,
    pub strategy_name: String,
    pub params: BTreeMap<String, serde_json::Value>,
    pub config_hash: String,
    pub threads: usize,
    pub requested_start: Option<Ts>,
    pub requested_end: Option<Ts>,
}

#[derive(Default)]
struct Position {
    qty: f64,
    avg_entry: f64,
    entry_fees: f64,
    entry_ts: Ts,
    entry_index: usize,
    stop: Option<f64>,
    take_profit: Option<f64>,
}

struct PendingOrder {
    kind: PendingKind,
}

#[derive(Clone, Copy)]
enum PendingKind {
    Target { weight: f64, signal_equity: f64 },
    RiskExit { reason: FillReason },
}

#[derive(Clone, Copy)]
struct RiskTrigger {
    reason: FillReason,
    reference: f64,
    ambiguous: bool,
}

#[derive(Default)]
struct FillOutcome {
    filled: bool,
    volume_clipped: bool,
    cash_clipped: bool,
    margin_clipped: bool,
}

#[derive(Clone, Copy)]
struct FeeModel {
    rate: f64,
    fixed: f64,
}

#[derive(Clone, Copy)]
struct Fill {
    delta: f64,
    price: f64,
    fee: f64,
}

struct Engine<'a> {
    universe: &'a Universe,
    input: &'a EngineInput,
    cash: f64,
    positions: Vec<Position>,
    pending: Vec<Option<PendingOrder>>,
    target_weights: Vec<f64>,
    equity: Vec<EquityPoint>,
    fills: Vec<FillRecord>,
    trades: Vec<TradeRecord>,
    warnings: Vec<String>,
    peak: f64,
    targets: Vec<TargetPosition>,
    updates: Vec<Option<f64>>,
    position_views: Vec<PositionView>,
    risk_locked: Vec<bool>,
}

pub fn run_backtest(
    universe: &Universe,
    input: &EngineInput,
    strategy: &mut dyn Strategy,
) -> Result<BacktestResult> {
    if universe.is_empty() {
        return Err(ConfigError::Invalid {
            field: "data".into(),
            message: "没有可回测的 bar".into(),
        }
        .into());
    }
    let n = universe.symbol_count();
    let mut engine = Engine {
        universe,
        input,
        cash: input.initial_cash,
        positions: (0..n).map(|_| Position::default()).collect(),
        pending: (0..n).map(|_| None).collect(),
        target_weights: vec![0.0; n],
        equity: Vec::with_capacity(universe.len()),
        fills: Vec::new(),
        trades: Vec::new(),
        warnings: Vec::new(),
        peak: input.initial_cash,
        targets: Vec::with_capacity(n),
        updates: vec![None; n],
        position_views: (0..n)
            .map(|symbol| PositionView {
                symbol: symbol as u32,
                qty: 0.0,
                avg_entry: 0.0,
                mark: 0.0,
                stop: None,
                take_profit: None,
            })
            .collect(),
        risk_locked: vec![false; n],
    };
    engine.run(strategy)
}

impl Engine<'_> {
    fn run(&mut self, strategy: &mut dyn Strategy) -> Result<BacktestResult> {
        for index in 0..self.universe.len() {
            let ts = self.universe.timeline[index];
            self.begin_bar();
            self.scan_open_risk(index)?;
            self.process_open(index, ts)?;
            self.process_intrabar_risk(index, ts)?;
            self.mark_close(index, ts);
            self.queue_close_risk(index);
            self.run_strategy(index, ts, strategy)?;
        }
        for (symbol, order) in self.pending.iter().enumerate() {
            if let Some(order) = order {
                let what = match order.kind {
                    PendingKind::RiskExit { .. } => "未成交风险退出订单",
                    PendingKind::Target { .. } => "未成交订单",
                };
                self.warnings.push(format!(
                    "回测结束时标的 {} 仍有{what}",
                    self.universe.specs[symbol].symbol
                ));
            }
        }
        let ppy = self.universe.specs[0].effective_periods_per_year(self.universe.timeframe);
        let metrics = compute_metrics(&MetricInput {
            equity: &self.equity,
            trades: &self.trades,
            fills: &self.fills,
            initial_cash: self.input.initial_cash,
            periods_per_year: ppy,
            risk_free_rate: self.input.risk_free_rate,
        });
        let drawdowns = drawdown_episodes(&self.equity, self.input.initial_cash, 5);
        let monthly = monthly_returns(&self.equity, self.input.initial_cash);
        let meta = RunMeta {
            strategy: self.input.strategy_name.clone(),
            params: self.input.params.clone(),
            symbols: self
                .universe
                .specs
                .iter()
                .map(|spec| spec.symbol.clone())
                .collect(),
            requested_start: self.input.requested_start,
            requested_end: self.input.requested_end,
            first_bar_ts: self.universe.timeline.first().copied(),
            last_bar_ts: self.universe.timeline.last().copied(),
            timeframe: self.universe.timeframe.to_string(),
            config_hash: self.input.config_hash.clone(),
            data_hash: self.universe.data_hash.clone(),
            threads: self.input.threads,
            walk_forward_window: None,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            warnings: self.warnings.clone(),
        };
        Ok(BacktestResult {
            meta,
            equity: std::mem::take(&mut self.equity),
            fills: std::mem::take(&mut self.fills),
            trades: std::mem::take(&mut self.trades),
            metrics,
            drawdowns,
            monthly,
            warnings: self.warnings.clone(),
        })
    }

    fn begin_bar(&mut self) {
        for symbol in 0..self.positions.len() {
            self.risk_locked[symbol] = self.pending[symbol]
                .as_ref()
                .is_some_and(|order| matches!(order.kind, PendingKind::RiskExit { .. }));
        }
    }

    fn scan_open_risk(&mut self, index: usize) -> Result<()> {
        for symbol in 0..self.positions.len() {
            if self.positions[symbol].qty == 0.0 {
                continue;
            }
            if matches!(
                self.pending[symbol].as_ref().map(|order| &order.kind),
                Some(PendingKind::RiskExit { .. })
            ) {
                continue;
            }
            let bar = self.universe.bar(symbol as u32, index);
            let Some(trigger) = open_trigger(&self.positions[symbol], &bar, self.input.risk) else {
                continue;
            };
            self.target_weights[symbol] = 0.0;
            self.risk_locked[symbol] = true;
            self.pending[symbol] = Some(PendingOrder {
                kind: PendingKind::RiskExit {
                    reason: trigger.reason,
                },
            });
        }
        Ok(())
    }

    fn process_open(&mut self, index: usize, ts: Ts) -> Result<()> {
        for symbol in 0..self.positions.len() {
            let Some(order) = self.pending[symbol].take() else {
                continue;
            };
            let bar = self.universe.bar(symbol as u32, index);
            if bar.volume <= 0.0 {
                self.pending[symbol] = Some(order);
                continue;
            }
            let keep = match order.kind {
                PendingKind::Target {
                    weight,
                    signal_equity,
                } => self.try_fill(symbol, index, ts, bar, weight, signal_equity)?,
                PendingKind::RiskExit { reason } => {
                    self.execute_risk(symbol, index, ts, bar, bar.open, reason, false)?
                }
            };
            if keep {
                self.pending[symbol] = Some(order);
            }
        }
        Ok(())
    }

    fn process_intrabar_risk(&mut self, index: usize, ts: Ts) -> Result<()> {
        for symbol in 0..self.positions.len() {
            if self.positions[symbol].qty == 0.0 {
                continue;
            }
            if matches!(
                self.pending[symbol].as_ref().map(|order| &order.kind),
                Some(PendingKind::RiskExit { .. })
            ) {
                continue;
            }
            let bar = self.universe.bar(symbol as u32, index);
            let Some(trigger) = intrabar_trigger(&self.positions[symbol], &bar, self.input.risk)
            else {
                continue;
            };
            if trigger.ambiguous {
                self.warnings.push(format!(
                    "ts={ts} 标的 {} 同 bar 止损与止盈同时触发,按止损成交,ambiguous_bar=true",
                    self.universe.specs[symbol].symbol
                ));
            }
            self.target_weights[symbol] = 0.0;
            self.risk_locked[symbol] = true;
            self.pending[symbol] = None;
            if bar.volume <= 0.0 {
                self.pending[symbol] = Some(PendingOrder {
                    kind: PendingKind::RiskExit {
                        reason: trigger.reason,
                    },
                });
                continue;
            }
            let keep = self.execute_risk(
                symbol,
                index,
                ts,
                bar,
                trigger.reference,
                trigger.reason,
                trigger.ambiguous,
            )?;
            if keep {
                self.pending[symbol] = Some(PendingOrder {
                    kind: PendingKind::RiskExit {
                        reason: trigger.reason,
                    },
                });
            }
        }
        Ok(())
    }

    fn queue_close_risk(&mut self, index: usize) {
        for symbol in 0..self.positions.len() {
            if self.positions[symbol].qty == 0.0 {
                continue;
            }
            if matches!(
                self.pending[symbol].as_ref().map(|order| &order.kind),
                Some(PendingKind::RiskExit { .. })
            ) {
                continue;
            }
            let bar = self.universe.bar(symbol as u32, index);
            let Some(trigger) = close_trigger(&self.positions[symbol], &bar, self.input.risk)
            else {
                continue;
            };
            self.target_weights[symbol] = 0.0;
            self.risk_locked[symbol] = true;
            self.pending[symbol] = Some(PendingOrder {
                kind: PendingKind::RiskExit {
                    reason: trigger.reason,
                },
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_risk(
        &mut self,
        symbol: usize,
        index: usize,
        ts: Ts,
        bar: crate::model::BarView,
        reference: f64,
        reason: FillReason,
        ambiguous: bool,
    ) -> Result<bool> {
        let qty = self.positions[symbol].qty;
        if qty == 0.0 {
            return Ok(false);
        }
        let outcome = self.execute_delta(
            symbol, index, ts, reference, -qty, bar.volume, reason, ambiguous,
        )?;
        if outcome.volume_clipped {
            self.warnings.push(format!(
                "ts={ts} 标的 {} 风险退出受成交量限制,部分成交,剩余顺延",
                self.universe.specs[symbol].symbol
            ));
        }
        if outcome.cash_clipped {
            self.warnings.push(format!(
                "ts={ts} 标的 {} 风险退出受现金约束,成交数量被裁剪",
                self.universe.specs[symbol].symbol
            ));
        }
        if outcome.margin_clipped {
            self.warnings.push(format!(
                "ts={ts} 标的 {} 风险退出受保证金约束,成交数量被裁剪",
                self.universe.specs[symbol].symbol
            ));
        }
        if !outcome.filled {
            self.warnings.push(format!(
                "ts={ts} 标的 {} 风险退出未能成交,顺延到下一根可用 bar",
                self.universe.specs[symbol].symbol
            ));
        }
        Ok(self.positions[symbol].qty != 0.0)
    }

    fn try_fill(
        &mut self,
        symbol: usize,
        index: usize,
        ts: Ts,
        bar: crate::model::BarView,
        weight: f64,
        signal_equity: f64,
    ) -> Result<bool> {
        let spec = &self.universe.specs[symbol];
        let multiplier = spec.multiplier;
        let lot_size = spec.lot_size;
        let slippage_bps = spec.slippage_bps;
        let slip_rate = slippage_bps / 10_000.0;

        let old_qty = self.positions[symbol].qty;
        let current_notional = old_qty * bar.open * multiplier;
        let target_notional = weight * signal_equity;
        let buying = target_notional > current_notional;
        let fill_price = if buying {
            bar.open * (1.0 + slip_rate)
        } else {
            bar.open * (1.0 - slip_rate)
        };
        if !fill_price.is_finite() || fill_price <= 0.0 {
            return Err(ExecutionError::Invariant {
                ts,
                message: format!("成交价非法: {fill_price}"),
            }
            .into());
        }
        let target_qty = round_to_lot(target_notional / (fill_price * multiplier), lot_size);
        let delta = target_qty - old_qty;
        let reason = if weight == 0.0 {
            FillReason::Close
        } else if old_qty == 0.0 {
            FillReason::Entry
        } else {
            FillReason::Rebalance
        };
        let cap = if self.input.execution.participation_rate > 0.0 {
            bar.volume * self.input.execution.participation_rate
        } else {
            f64::INFINITY
        };
        let outcome = self.execute_delta(symbol, index, ts, bar.open, delta, cap, reason, false)?;
        if outcome.volume_clipped {
            self.warnings.push(format!(
                "ts={ts} 标的 {} 受成交量参与率限制,部分成交,剩余顺延",
                self.universe.specs[symbol].symbol
            ));
        }
        if outcome.cash_clipped {
            let message = if outcome.filled {
                "现金不足,买入数量被裁剪"
            } else {
                "现金不足,订单被拒绝"
            };
            self.warnings.push(format!(
                "ts={ts} 标的 {} {message}",
                self.universe.specs[symbol].symbol
            ));
        }
        if outcome.margin_clipped {
            let message = if outcome.filled {
                "受保证金约束,成交数量被裁剪"
            } else {
                "现金/保证金不足,订单被拒绝"
            };
            self.warnings.push(format!(
                "ts={ts} 标的 {} {message}",
                self.universe.specs[symbol].symbol
            ));
        }
        Ok(outcome.volume_clipped)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_delta(
        &mut self,
        symbol: usize,
        index: usize,
        ts: Ts,
        reference: f64,
        desired_delta: f64,
        volume_cap: f64,
        reason: FillReason,
        ambiguous: bool,
    ) -> Result<FillOutcome> {
        let spec = &self.universe.specs[symbol];
        let multiplier = spec.multiplier;
        let lot_size = spec.lot_size;
        let fee_rate = spec.fee_bps / 10_000.0;
        let fee_fixed = spec.fee_fixed;
        let slip_rate = spec.slippage_bps / 10_000.0;
        let fees = FeeModel {
            rate: fee_rate,
            fixed: fee_fixed,
        };
        let mut outcome = FillOutcome::default();
        if desired_delta == 0.0 {
            return Ok(outcome);
        }
        let fill_price = if desired_delta > 0.0 {
            reference * (1.0 + slip_rate)
        } else {
            reference * (1.0 - slip_rate)
        };
        if !fill_price.is_finite() || fill_price <= 0.0 {
            return Err(ExecutionError::Invariant {
                ts,
                message: format!("成交价非法: {fill_price}"),
            }
            .into());
        }
        let min_step = if lot_size > 0.0 { lot_size } else { 1e-12 };
        let mut delta = desired_delta;
        if delta.abs() < min_step {
            return Ok(outcome);
        }

        if delta.abs() > volume_cap {
            let capped = floor_abs_to_lot(volume_cap, lot_size);
            delta = delta.signum() * capped;
            outcome.volume_clipped = true;
        }
        if delta.abs() < min_step {
            return Ok(outcome);
        }

        if delta > 0.0 {
            let per_unit = fill_price * multiplier * (1.0 + fee_rate);
            let affordable = (self.cash - fee_fixed).max(0.0) / per_unit;
            if delta > affordable {
                delta = floor_abs_to_lot(affordable, lot_size);
                outcome.cash_clipped = true;
            }
        }
        if delta.abs() < min_step {
            return Ok(outcome);
        }

        if !self.margin_feasible(symbol, index, delta, fill_price, fees) {
            let capped = self.max_feasible_delta(symbol, index, delta, fill_price, fees, lot_size);
            if capped.abs() < min_step {
                outcome.margin_clipped = true;
                return Ok(outcome);
            }
            delta = capped;
            outcome.margin_clipped = true;
        }

        let qty = delta.abs();
        let notional = qty * fill_price * multiplier;
        let fee = notional * fee_rate + fee_fixed;
        let signed_cash = delta * fill_price * multiplier;
        self.cash -= signed_cash + fee;
        let tolerance = 1e-9 * self.input.initial_cash.max(1.0);
        if self.cash < -tolerance {
            return Err(ExecutionError::Invariant {
                ts,
                message: format!(
                    "成交后现金为负: cash={}, delta={}, price={}",
                    self.cash, delta, fill_price
                ),
            }
            .into());
        }
        let side = if delta > 0.0 { Side::Buy } else { Side::Sell };
        let trade = self.apply_fill(
            symbol,
            index,
            ts,
            Fill {
                delta,
                price: fill_price,
                fee,
            },
            reason,
        );
        self.fills.push(FillRecord {
            ts,
            symbol: symbol as u32,
            side,
            qty,
            price: fill_price,
            notional,
            fee,
            slippage: qty * (fill_price - reference).abs() * multiplier,
            reason,
            ambiguous_bar: ambiguous,
        });
        if let Some(trade) = trade {
            self.trades.push(trade);
        }
        outcome.filled = true;
        Ok(outcome)
    }

    fn margin_feasible(
        &self,
        symbol: usize,
        index: usize,
        delta: f64,
        fill_price: f64,
        fees: FeeModel,
    ) -> bool {
        if delta == 0.0 {
            return true;
        }
        let notional = (delta * fill_price * self.universe.specs[symbol].multiplier).abs();
        let fee = notional * fees.rate + fees.fixed;
        let projected_cash =
            self.cash - delta * fill_price * self.universe.specs[symbol].multiplier - fee;
        let mut equity = projected_cash;
        let mut margin = 0.0;
        for (other, position) in self.positions.iter().enumerate() {
            let price = if other == symbol {
                fill_price
            } else {
                self.universe.bar(other as u32, index).open
            };
            let qty = if other == symbol {
                position.qty + delta
            } else {
                position.qty
            };
            let value = qty * price * self.universe.specs[other].multiplier;
            equity += value;
            margin += value.abs() * self.universe.specs[other].margin_rate;
        }
        let tolerance = 1e-9 * self.input.initial_cash.max(1.0);
        projected_cash >= -tolerance && equity + tolerance >= margin
    }

    fn max_feasible_delta(
        &self,
        symbol: usize,
        index: usize,
        delta: f64,
        fill_price: f64,
        fees: FeeModel,
        lot_size: f64,
    ) -> f64 {
        let direction = delta.signum();
        let mut low = 0.0;
        let mut high = delta.abs();
        for _ in 0..60 {
            let mid = (low + high) / 2.0;
            if self.margin_feasible(symbol, index, direction * mid, fill_price, fees) {
                low = mid;
            } else {
                high = mid;
            }
        }
        direction * floor_abs_to_lot(low, lot_size)
    }

    fn apply_fill(
        &mut self,
        symbol: usize,
        index: usize,
        ts: Ts,
        fill: Fill,
        reason: FillReason,
    ) -> Option<TradeRecord> {
        let Fill { delta, price, fee } = fill;
        let multiplier = self.universe.specs[symbol].multiplier;
        let risk = self.input.risk;
        let position = &mut self.positions[symbol];
        let old_qty = position.qty;
        if old_qty == 0.0 {
            position.qty = delta;
            position.avg_entry = price;
            position.entry_fees = fee;
            position.entry_ts = ts;
            position.entry_index = index;
            (position.stop, position.take_profit) = risk_levels(risk, delta.signum(), price);
            return None;
        }
        let old_abs = old_qty.abs();
        let delta_abs = delta.abs();
        let same_direction = old_qty.signum() == delta.signum();
        if same_direction {
            let new_abs = old_abs + delta_abs;
            position.avg_entry = (old_abs * position.avg_entry + delta_abs * price) / new_abs;
            position.entry_fees += fee;
            position.qty += delta;
            return None;
        }
        let closing = old_abs.min(delta_abs);
        let entry_fee_part = position.entry_fees * closing / old_abs;
        let exit_fee_part = fee * closing / delta_abs;
        let gross = closing * (price - position.avg_entry) * old_qty.signum() * multiplier;
        let pnl = gross - entry_fee_part - exit_fee_part;
        let denom = closing * position.avg_entry * multiplier;
        let trade = TradeRecord {
            symbol: symbol as u32,
            side: if old_qty > 0.0 { Side::Buy } else { Side::Sell },
            entry_ts: position.entry_ts,
            exit_ts: Some(ts),
            entry_price: position.avg_entry,
            exit_price: Some(price),
            qty: closing,
            pnl,
            fees: entry_fee_part + exit_fee_part,
            return_pct: if denom > 0.0 {
                pnl / denom * 100.0
            } else {
                0.0
            },
            bars_held: index.saturating_sub(position.entry_index) as u32,
            exit_reason: Some(reason),
        };
        if delta_abs > old_abs {
            let remaining = delta_abs - old_abs;
            position.qty = delta.signum() * remaining;
            position.avg_entry = price;
            position.entry_fees = fee - exit_fee_part;
            position.entry_ts = ts;
            position.entry_index = index;
            (position.stop, position.take_profit) = risk_levels(risk, delta.signum(), price);
        } else if (position.qty + delta).abs() < 1e-12 {
            *position = Position::default();
        } else {
            position.qty += delta;
            position.entry_fees -= entry_fee_part;
        }
        Some(trade)
    }

    fn mark_close(&mut self, index: usize, ts: Ts) {
        let mut equity = self.cash;
        let mut gross = 0.0;
        for (symbol, position) in self.positions.iter().enumerate() {
            let close = self.universe.bar(symbol as u32, index).close;
            let value = position.qty * close * self.universe.specs[symbol].multiplier;
            equity += value;
            gross += value.abs();
            let view = &mut self.position_views[symbol];
            view.qty = position.qty;
            view.avg_entry = position.avg_entry;
            view.mark = close;
            view.stop = position.stop;
            view.take_profit = position.take_profit;
        }
        if !equity.is_finite() {
            self.warnings
                .push(format!("ts={ts} 权益非有限值,后续结果不可信"));
        }
        if equity > self.peak {
            self.peak = equity;
        }
        let drawdown = if self.peak > 0.0 {
            (equity / self.peak - 1.0).min(0.0)
        } else {
            0.0
        };
        self.equity.push(EquityPoint {
            ts,
            equity,
            cash: self.cash,
            gross_exposure: gross,
            drawdown,
        });
    }

    fn run_strategy(&mut self, index: usize, ts: Ts, strategy: &mut dyn Strategy) -> Result<()> {
        let equity = self
            .equity
            .last()
            .map(|point| point.equity)
            .unwrap_or(self.cash);
        let ctx = MarketContext {
            ts,
            index,
            universe: self.universe,
            cash: self.cash,
            equity,
            positions: &self.position_views,
        };
        self.targets.clear();
        strategy.on_bar(&ctx, &mut self.targets);
        self.updates.fill(None);
        for target in &self.targets {
            let symbol = target.symbol as usize;
            if symbol >= self.positions.len() {
                return Err(ExecutionError::InvalidSymbol {
                    symbol_id: target.symbol,
                    symbols: self.positions.len(),
                }
                .into());
            }
            if !target.weight.is_finite() {
                return Err(ExecutionError::InvalidWeight {
                    symbol: self.universe.specs[symbol].symbol.clone(),
                    weight: target.weight,
                }
                .into());
            }
            if self.risk_locked[symbol] {
                continue;
            }
            self.updates[symbol] = Some(target.weight);
        }
        let mut gross = 0.0;
        for (symbol, update) in self.updates.iter().enumerate() {
            let weight = update.unwrap_or(self.target_weights[symbol]);
            gross += weight.abs();
        }
        let margin_rate = self
            .universe
            .specs
            .iter()
            .map(|spec| spec.margin_rate)
            .fold(f64::INFINITY, f64::min);
        let limit = 1.0 / margin_rate;
        if gross > limit + 1e-9 {
            return Err(ExecutionError::GrossLimit {
                gross,
                limit,
                margin_rate,
            }
            .into());
        }
        for (symbol, update) in self.updates.iter().enumerate() {
            let Some(weight) = update else {
                continue;
            };
            if (*weight - self.target_weights[symbol]).abs() <= 1e-12 {
                continue;
            }
            self.target_weights[symbol] = *weight;
            self.pending[symbol] = Some(PendingOrder {
                kind: PendingKind::Target {
                    weight: *weight,
                    signal_equity: equity,
                },
            });
        }
        Ok(())
    }
}

fn risk_levels(risk: RiskConfig, side: f64, entry: f64) -> (Option<f64>, Option<f64>) {
    if side > 0.0 {
        (
            risk.stop_loss.map(|leg| entry * (1.0 - leg.distance)),
            risk.take_profit.map(|leg| entry * (1.0 + leg.distance)),
        )
    } else {
        (
            risk.stop_loss.map(|leg| entry * (1.0 + leg.distance)),
            risk.take_profit.map(|leg| entry * (1.0 - leg.distance)),
        )
    }
}

fn open_trigger(
    position: &Position,
    bar: &crate::model::BarView,
    risk: RiskConfig,
) -> Option<RiskTrigger> {
    let stop = risk
        .stop_loss
        .filter(|leg| leg.trigger == Trigger::Intrabar)
        .and(position.stop);
    let take_profit = risk
        .take_profit
        .filter(|leg| leg.trigger == Trigger::Intrabar)
        .and(position.take_profit);
    if position.qty > 0.0 {
        if stop.is_some_and(|stop| bar.open <= stop) {
            return Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: bar.open,
                ambiguous: false,
            });
        }
        if take_profit.is_some_and(|take_profit| bar.open >= take_profit) {
            return Some(RiskTrigger {
                reason: FillReason::TakeProfit,
                reference: bar.open,
                ambiguous: false,
            });
        }
    } else if position.qty < 0.0 {
        if stop.is_some_and(|stop| bar.open >= stop) {
            return Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: bar.open,
                ambiguous: false,
            });
        }
        if take_profit.is_some_and(|take_profit| bar.open <= take_profit) {
            return Some(RiskTrigger {
                reason: FillReason::TakeProfit,
                reference: bar.open,
                ambiguous: false,
            });
        }
    }
    None
}

fn intrabar_trigger(
    position: &Position,
    bar: &crate::model::BarView,
    risk: RiskConfig,
) -> Option<RiskTrigger> {
    let stop = risk
        .stop_loss
        .filter(|leg| leg.trigger == Trigger::Intrabar)
        .and(position.stop);
    let take_profit = risk
        .take_profit
        .filter(|leg| leg.trigger == Trigger::Intrabar)
        .and(position.take_profit);
    if stop.is_none() && take_profit.is_none() {
        return None;
    }
    if position.qty > 0.0 {
        let stop_hit = stop.is_some_and(|stop| bar.low <= stop);
        let take_profit_hit = take_profit.is_some_and(|take_profit| bar.high >= take_profit);
        match (stop_hit, take_profit_hit) {
            (true, true) => Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: stop?,
                ambiguous: true,
            }),
            (true, false) => Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: stop?,
                ambiguous: false,
            }),
            (false, true) => Some(RiskTrigger {
                reason: FillReason::TakeProfit,
                reference: take_profit?,
                ambiguous: false,
            }),
            (false, false) => None,
        }
    } else if position.qty < 0.0 {
        let stop_hit = stop.is_some_and(|stop| bar.high >= stop);
        let take_profit_hit = take_profit.is_some_and(|take_profit| bar.low <= take_profit);
        match (stop_hit, take_profit_hit) {
            (true, true) => Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: stop?,
                ambiguous: true,
            }),
            (true, false) => Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: stop?,
                ambiguous: false,
            }),
            (false, true) => Some(RiskTrigger {
                reason: FillReason::TakeProfit,
                reference: take_profit?,
                ambiguous: false,
            }),
            (false, false) => None,
        }
    } else {
        None
    }
}

fn close_trigger(
    position: &Position,
    bar: &crate::model::BarView,
    risk: RiskConfig,
) -> Option<RiskTrigger> {
    let stop = risk
        .stop_loss
        .filter(|leg| leg.trigger == Trigger::Close)
        .and(position.stop);
    let take_profit = risk
        .take_profit
        .filter(|leg| leg.trigger == Trigger::Close)
        .and(position.take_profit);
    if position.qty > 0.0 {
        if stop.is_some_and(|stop| bar.close <= stop) {
            return Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: bar.close,
                ambiguous: false,
            });
        }
        if take_profit.is_some_and(|take_profit| bar.close >= take_profit) {
            return Some(RiskTrigger {
                reason: FillReason::TakeProfit,
                reference: bar.close,
                ambiguous: false,
            });
        }
    } else if position.qty < 0.0 {
        if stop.is_some_and(|stop| bar.close >= stop) {
            return Some(RiskTrigger {
                reason: FillReason::StopLoss,
                reference: bar.close,
                ambiguous: false,
            });
        }
        if take_profit.is_some_and(|take_profit| bar.close <= take_profit) {
            return Some(RiskTrigger {
                reason: FillReason::TakeProfit,
                reference: bar.close,
                ambiguous: false,
            });
        }
    }
    None
}

fn round_to_lot(value: f64, lot: f64) -> f64 {
    if lot > 0.0 {
        (value / lot).trunc() * lot
    } else {
        value
    }
}

fn floor_abs_to_lot(value: f64, lot: f64) -> f64 {
    let value = value.abs();
    if lot > 0.0 {
        (value / lot).floor() * lot
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RiskLeg, Trigger};
    use crate::error::{Error, ExecutionError};
    use crate::model::{AssetClass, BarSeries, InstrumentSpec, Side, Timeframe};

    const DAY: i64 = 86_400_000;
    const BASE_TS: i64 = 1_577_836_800_000;

    struct Scripted {
        plan: Vec<(usize, f64)>,
    }

    impl Scripted {
        fn new(plan: &[(usize, f64)]) -> Self {
            Self {
                plan: plan.to_vec(),
            }
        }
    }

    impl Strategy for Scripted {
        fn on_bar(&mut self, ctx: &MarketContext<'_>, out: &mut Vec<TargetPosition>) {
            for (index, weight) in &self.plan {
                if *index == ctx.index {
                    out.push(TargetPosition {
                        symbol: 0,
                        weight: *weight,
                    });
                }
            }
        }
    }

    fn universe_ohlcv(bars: &[(f64, f64, f64, f64, f64)]) -> Universe {
        assert!(!bars.is_empty());
        let ts: Vec<Ts> = (0..bars.len())
            .map(|index| BASE_TS + index as i64 * DAY)
            .collect();
        Universe {
            timeframe: Timeframe::ONE_DAY,
            specs: vec![InstrumentSpec {
                symbol: "TEST".into(),
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
            series: vec![BarSeries {
                symbol: 0,
                ts: ts.clone(),
                open: bars.iter().map(|bar| bar.0).collect(),
                high: bars.iter().map(|bar| bar.1).collect(),
                low: bars.iter().map(|bar| bar.2).collect(),
                close: bars.iter().map(|bar| bar.3).collect(),
                volume: bars.iter().map(|bar| bar.4).collect(),
            }],
            timeline: ts,
            data_hash: "test".into(),
        }
    }

    fn universe(opens: &[f64], closes: &[f64]) -> Universe {
        assert_eq!(opens.len(), closes.len());
        assert!(!closes.is_empty());
        let bars: Vec<(f64, f64, f64, f64, f64)> = opens
            .iter()
            .zip(closes)
            .map(|(open, close)| {
                (
                    *open,
                    open.max(*close) + 1.0,
                    open.min(*close) - 1.0,
                    *close,
                    1_000_000.0,
                )
            })
            .collect();
        universe_ohlcv(&bars)
    }

    fn risk_input(stop_loss: Option<RiskLeg>, take_profit: Option<RiskLeg>) -> EngineInput {
        let mut input = input();
        input.risk = RiskConfig {
            stop_loss,
            take_profit,
        };
        input
    }

    fn leg(trigger: Trigger, distance: f64) -> RiskLeg {
        RiskLeg { trigger, distance }
    }

    fn input() -> EngineInput {
        EngineInput {
            initial_cash: 100_000.0,
            risk_free_rate: 0.0,
            execution: ExecutionConfig::default(),
            risk: RiskConfig::default(),
            strategy_name: "scripted".into(),
            params: BTreeMap::new(),
            config_hash: "test".into(),
            threads: 1,
            requested_start: None,
            requested_end: None,
        }
    }

    fn universe_with_fee(opens: &[f64], closes: &[f64], fee_bps: f64) -> Universe {
        let mut universe = universe(opens, closes);
        universe.specs[0].fee_bps = fee_bps;
        universe
    }

    #[test]
    fn t1_1_hand_calculated_long_position() {
        let opens = [100.0, 99.0, 98.0, 100.0, 102.0, 103.0];
        let closes = [100.0, 99.0, 98.0, 100.0, 102.0, 103.0];
        let universe = universe(&opens, &closes);
        let mut strategy = Scripted::new(&[(4, 0.99)]);
        let result = run_backtest(&universe, &input(), &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 1);
        let fill = &result.fills[0];
        assert_eq!(fill.ts, universe.timeline[5]);
        assert_eq!(fill.side, Side::Buy);
        assert!((fill.price - 103.0).abs() < 1e-12);
        assert!((fill.qty - 99_000.0 / 103.0).abs() < 1e-9);

        let last = result.equity.last().expect("equity");
        assert!((last.cash - 1_000.0).abs() < 1e-9);
        assert!((last.equity - 100_000.0).abs() < 1e-9);
    }

    #[test]
    fn t1_2_fill_uses_next_open_not_close() {
        let opens = [100.0, 99.0, 98.0, 100.0, 102.0, 90.0];
        let closes = [100.0, 99.0, 98.0, 100.0, 102.0, 90.0];
        let universe = universe(&opens, &closes);
        let mut strategy = Scripted::new(&[(4, 0.99)]);
        let result = run_backtest(&universe, &input(), &mut strategy).expect("run");
        assert_eq!(result.fills.len(), 1);
        assert!((result.fills[0].price - 90.0).abs() < 1e-12);
        assert!((result.fills[0].qty - 1_100.0).abs() < 1e-9);
    }

    #[test]
    fn t1_3_zero_volume_bar_defers_to_next_tradable_bar() {
        let opens = [100.0, 100.0, 100.0, 100.0, 100.0, 90.0, 95.0];
        let closes = [100.0, 100.0, 100.0, 100.0, 100.0, 90.0, 95.0];
        let mut universe = universe(&opens, &closes);
        universe.series[0].volume[5] = 0.0;
        let mut strategy = Scripted::new(&[(4, 0.99)]);
        let result = run_backtest(&universe, &input(), &mut strategy).expect("run");
        assert_eq!(result.fills.len(), 1);
        assert_eq!(result.fills[0].ts, universe.timeline[6]);
        assert!((result.fills[0].price - 95.0).abs() < 1e-12);
        assert!((result.fills[0].qty - 99_000.0 / 95.0).abs() < 1e-9);
    }

    #[test]
    fn t1_4_costs_reduce_pnl_to_989_5() {
        let opens = [100.0, 100.0, 100.0, 110.0];
        let closes = [100.0, 100.0, 100.0, 110.0];
        let universe = universe_with_fee(&opens, &closes, 5.0);
        let mut strategy = Scripted::new(&[(1, 0.1), (2, 0.0)]);
        let result = run_backtest(&universe, &input(), &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        assert!((result.fills[0].price - 100.0).abs() < 1e-12);
        assert!((result.fills[0].fee - 5.0).abs() < 1e-9);
        assert!((result.fills[1].price - 110.0).abs() < 1e-12);
        assert!((result.fills[1].fee - 5.5).abs() < 1e-9);

        assert_eq!(result.trades.len(), 1);
        let trade = &result.trades[0];
        assert!((trade.qty - 100.0).abs() < 1e-9);
        assert!((trade.pnl - 989.5).abs() < 1e-9);
        assert!((trade.fees - 10.5).abs() < 1e-9);
    }

    #[test]
    fn t2_1_long_stop_gaps_to_open() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (92.0, 93.0, 90.0, 92.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Intrabar, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        let exit = &result.fills[1];
        assert_eq!(exit.reason, FillReason::StopLoss);
        assert_eq!(exit.ts, universe.timeline[2]);
        assert!((exit.price - 92.0).abs() < 1e-12);
        assert!((exit.qty - 990.0).abs() < 1e-9);
    }

    #[test]
    fn t2_2_long_stop_fills_at_trigger_price() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (98.0, 99.0, 94.0, 96.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Intrabar, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        assert_eq!(result.fills[1].reason, FillReason::StopLoss);
        assert!((result.fills[1].price - 95.0).abs() < 1e-12);
    }

    #[test]
    fn t2_3_short_stop_gaps_to_open() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (108.0, 109.0, 107.0, 108.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Intrabar, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, -0.5)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        let exit = &result.fills[1];
        assert_eq!(exit.reason, FillReason::StopLoss);
        assert_eq!(exit.side, Side::Buy);
        assert!((exit.price - 108.0).abs() < 1e-12);
        assert!((exit.qty - 500.0).abs() < 1e-9);
    }

    #[test]
    fn t2_4_ambiguous_bar_uses_stop_and_warns() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 106.0, 94.0, 100.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(
            Some(leg(Trigger::Intrabar, 0.05)),
            Some(leg(Trigger::Intrabar, 0.05)),
        );
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        let exit = &result.fills[1];
        assert_eq!(exit.reason, FillReason::StopLoss);
        assert!((exit.price - 95.0).abs() < 1e-12);
        assert!(exit.ambiguous_bar);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("ambiguous_bar=true"))
        );
    }

    #[test]
    fn t2_5_close_trigger_fills_at_next_open() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 94.0, 1_000_000.0),
            (93.0, 94.0, 92.0, 93.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Close, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        let exit = &result.fills[1];
        assert_eq!(exit.reason, FillReason::StopLoss);
        assert_eq!(exit.ts, universe.timeline[3]);
        assert!((exit.price - 93.0).abs() < 1e-12);
    }

    #[test]
    fn close_trigger_ignores_intrabar_gap() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (90.0, 101.0, 89.0, 100.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Close, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 1);
    }

    #[test]
    fn t2_6_participation_partially_fills_and_defers() {
        let bars = [(100.0, 101.0, 99.0, 100.0, 1_000.0); 6];
        let universe = universe_ohlcv(&bars);
        let mut input = input();
        input.execution.participation_rate = 0.1;
        let mut strategy = Scripted::new(&[(0, 0.5)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 5);
        for fill in &result.fills {
            assert!((fill.qty - 100.0).abs() < 1e-9);
        }
        let total: f64 = result.fills.iter().map(|fill| fill.qty).sum();
        assert!((total - 500.0).abs() < 1e-9);
    }

    #[test]
    fn t2_9_zero_volume_defers_stop_to_next_bar() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 90.0, 95.0, 0.0),
            (93.0, 94.0, 92.0, 93.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Intrabar, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        let exit = &result.fills[1];
        assert_eq!(exit.ts, universe.timeline[3]);
        assert!((exit.price - 93.0).abs() < 1e-12);
        assert_eq!(exit.reason, FillReason::StopLoss);
    }

    #[test]
    fn t2_10_take_profit_gaps_to_open() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (110.0, 111.0, 109.0, 110.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(None, Some(leg(Trigger::Intrabar, 0.05)));
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        let exit = &result.fills[1];
        assert_eq!(exit.reason, FillReason::TakeProfit);
        assert!((exit.price - 110.0).abs() < 1e-12);
    }

    #[test]
    fn t2_11_fees_clip_quantity_and_keep_cash_non_negative() {
        let universe = universe_with_fee(&[100.0, 100.0], &[100.0, 100.0], 5.0);
        let mut strategy = Scripted::new(&[(0, 1.0)]);
        let result = run_backtest(&universe, &input(), &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 1);
        let fill = &result.fills[0];
        assert!(fill.qty < 1000.0);
        assert!(fill.qty > 999.0);
        let last = result.equity.last().expect("equity");
        assert!(last.cash >= -1e-9);
        assert!(last.cash < 1e-6);
    }

    #[test]
    fn target_quantity_rounds_down_to_lot_size() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
        ];
        let mut universe = universe_ohlcv(&bars);
        universe.specs[0].lot_size = 10.0;
        let mut strategy = Scripted::new(&[(0, 0.995)]);
        let result = run_backtest(&universe, &input(), &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 1);
        assert!((result.fills[0].qty - 990.0).abs() < 1e-9);
    }

    #[test]
    fn intrabar_risk_can_trigger_on_entry_bar() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 94.0, 96.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Intrabar, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, 0.99)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        assert_eq!(result.fills[1].ts, universe.timeline[1]);
        assert_eq!(result.fills[1].reason, FillReason::StopLoss);
        assert!((result.fills[1].price - 95.0).abs() < 1e-12);
        assert_eq!(result.trades.len(), 1);
        assert_eq!(result.trades[0].bars_held, 0);
    }

    #[test]
    fn risk_exit_ignores_participation_but_caps_at_volume() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (94.0, 95.0, 93.0, 94.0, 400.0),
            (94.0, 95.0, 93.0, 94.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let mut input = risk_input(Some(leg(Trigger::Intrabar, 0.05)), None);
        input.execution.participation_rate = 0.1;
        let mut strategy = Scripted::new(&[(0, 1.0)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 3);
        assert!((result.fills[1].qty - 400.0).abs() < 1e-9);
        assert!((result.fills[2].qty - 600.0).abs() < 1e-9);
        assert_eq!(result.fills[1].reason, FillReason::StopLoss);
        assert_eq!(result.fills[2].reason, FillReason::StopLoss);
    }

    #[test]
    fn adding_to_position_keeps_original_stop() {
        let bars = [
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (120.0, 121.0, 119.0, 120.0, 1_000_000.0),
            (100.0, 101.0, 99.0, 100.0, 1_000_000.0),
            (94.0, 95.0, 93.0, 94.0, 1_000_000.0),
        ];
        let universe = universe_ohlcv(&bars);
        let input = risk_input(Some(leg(Trigger::Intrabar, 0.05)), None);
        let mut strategy = Scripted::new(&[(0, 0.5), (1, 1.0)]);
        let result = run_backtest(&universe, &input, &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 3);
        let exit = &result.fills[2];
        assert_eq!(exit.ts, universe.timeline[4]);
        assert!((exit.price - 94.0).abs() < 1e-12);
        assert_eq!(exit.reason, FillReason::StopLoss);
    }

    #[test]
    fn t2_8_short_pnl_and_cash_are_consistent() {
        let opens = [100.0, 100.0, 90.0];
        let closes = [100.0, 100.0, 90.0];
        let universe = universe(&opens, &closes);
        let mut strategy = Scripted::new(&[(0, -0.5), (1, 0.0)]);
        let result = run_backtest(&universe, &input(), &mut strategy).expect("run");

        assert_eq!(result.fills.len(), 2);
        assert_eq!(result.fills[0].side, Side::Sell);
        assert!((result.fills[0].qty - 500.0).abs() < 1e-9);
        assert!((result.equity[1].cash - 150_000.0).abs() < 1e-9);
        assert_eq!(result.trades.len(), 1);
        assert!((result.trades[0].pnl - 5_000.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_non_finite_weight() {
        let universe = universe(&[100.0, 100.0], &[100.0, 100.0]);
        let mut strategy = Scripted::new(&[(0, f64::NAN)]);
        let error = run_backtest(&universe, &input(), &mut strategy).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Execution(ExecutionError::InvalidWeight { .. })
        ));
    }

    #[test]
    fn t2_7_gross_exposure_above_margin_limit_is_rejected() {
        let universe = universe(&[100.0, 100.0], &[100.0, 100.0]);
        let mut strategy = Scripted::new(&[(0, 1.5)]);
        let error = run_backtest(&universe, &input(), &mut strategy).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Execution(ExecutionError::GrossLimit { .. })
        ));
    }

    #[test]
    fn empty_universe_is_rejected() {
        let universe = Universe {
            timeframe: Timeframe::ONE_DAY,
            specs: vec![InstrumentSpec {
                symbol: "TEST".into(),
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
            series: vec![BarSeries::default()],
            timeline: Vec::new(),
            data_hash: "test".into(),
        };
        let mut strategy = Scripted::new(&[]);
        let error = run_backtest(&universe, &input(), &mut strategy).expect_err("must fail");
        assert!(matches!(error, Error::Config(_)));
    }
}
