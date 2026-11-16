use std::collections::BTreeMap;

use chrono::{DateTime, Datelike};

use crate::model::{
    DrawdownEpisode, EquityPoint, FillRecord, Metrics, MonthlyReturn, TradeRecord, Ts,
};

#[derive(Debug)]
pub struct MetricInput<'a> {
    pub equity: &'a [EquityPoint],
    pub trades: &'a [TradeRecord],
    pub fills: &'a [FillRecord],
    pub initial_cash: f64,
    pub periods_per_year: u32,
    pub risk_free_rate: f64,
}

pub fn compute_metrics(input: &MetricInput<'_>) -> Metrics {
    let mut metrics = Metrics::empty(input.periods_per_year, input.risk_free_rate);
    if input.equity.is_empty() || !input.initial_cash.is_finite() || input.initial_cash <= 0.0 {
        return metrics;
    }
    let n = input.equity.len();
    let first = &input.equity[0];
    let last = &input.equity[n - 1];
    metrics.total_return = finite(last.equity / input.initial_cash - 1.0);

    let returns = equity_returns(input.equity, input.initial_cash);
    let ppy = input.periods_per_year as f64;
    let rf_per_period = input.risk_free_rate / ppy;

    let years = calendar_years(first.ts, last.ts);
    if let Some(years) = years
        && years > 0.0
        && last.equity > 0.0
    {
        metrics.cagr = finite((last.equity / input.initial_cash).powf(1.0 / years) - 1.0);
    }

    if returns.len() >= 2 {
        let mean = mean(&returns);
        let std = sample_std(&returns, mean);
        if std > 0.0 {
            metrics.annual_volatility = finite(std * ppy.sqrt());
            metrics.sharpe = finite((mean - rf_per_period) / std * ppy.sqrt());
        }
        let downside: f64 = returns
            .iter()
            .map(|r| {
                let diff = (r - rf_per_period).min(0.0);
                diff * diff
            })
            .sum::<f64>()
            / returns.len() as f64;
        let downside_dev = downside.sqrt();
        if downside_dev > 0.0 {
            metrics.sortino = finite((mean - rf_per_period) / downside_dev * ppy.sqrt());
        }
    }

    let drawdowns = series_drawdowns(input.equity, input.initial_cash);
    let max_dd = drawdowns.iter().fold(0.0_f64, |acc, dd| acc.min(*dd));
    metrics.max_drawdown = finite(if max_dd < 0.0 { -max_dd } else { 0.0 });
    metrics.ulcer =
        finite((drawdowns.iter().map(|dd| dd * dd).sum::<f64>() / drawdowns.len() as f64).sqrt());

    let episodes = all_drawdown_episodes(input.equity, input.initial_cash);
    metrics.max_drawdown_duration_days = finite(
        episodes
            .iter()
            .map(|episode| episode.duration_days)
            .fold(0.0_f64, f64::max),
    );
    if let Some(max_dd) = metrics.max_drawdown {
        if max_dd > 0.0 {
            if let Some(cagr) = metrics.cagr {
                metrics.calmar = finite(cagr / max_dd);
            }
        } else {
            metrics.calmar = None;
        }
    }

    let closed: Vec<&TradeRecord> = input
        .trades
        .iter()
        .filter(|trade| trade.exit_ts.is_some())
        .collect();
    metrics.trades = closed.len();
    if !closed.is_empty() {
        let wins = closed.iter().filter(|trade| trade.pnl > 0.0).count();
        metrics.win_rate = finite(wins as f64 / closed.len() as f64);
        let gross_profit: f64 = closed
            .iter()
            .filter(|trade| trade.pnl > 0.0)
            .map(|trade| trade.pnl)
            .sum();
        let gross_loss: f64 = closed
            .iter()
            .filter(|trade| trade.pnl < 0.0)
            .map(|trade| trade.pnl)
            .sum();
        if gross_loss < 0.0 {
            metrics.profit_factor = finite(gross_profit / gross_loss.abs());
        }
        metrics.expectancy =
            finite(closed.iter().map(|trade| trade.pnl).sum::<f64>() / closed.len() as f64);
    }

    metrics.total_fees = input.fills.iter().map(|fill| fill.fee).sum();
    let avg_equity = input.equity.iter().map(|point| point.equity).sum::<f64>() / n as f64;
    if avg_equity > 0.0
        && let Some(years) = years
        && years > 0.0
    {
        let traded: f64 = input.fills.iter().map(|fill| fill.notional.abs()).sum();
        metrics.turnover = finite(traded / avg_equity / years);
    }
    let exposed = input
        .equity
        .iter()
        .filter(|point| point.gross_exposure > 0.0)
        .count();
    metrics.exposure = finite(exposed as f64 / n as f64);
    metrics
}

pub fn drawdown_episodes(
    equity: &[EquityPoint],
    initial_cash: f64,
    top_n: usize,
) -> Vec<DrawdownEpisode> {
    let mut episodes = all_drawdown_episodes(equity, initial_cash);
    episodes.sort_by(|a, b| {
        b.depth
            .partial_cmp(&a.depth)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.peak_ts.cmp(&b.peak_ts))
    });
    episodes.truncate(top_n);
    episodes
}

pub fn all_drawdown_episodes(equity: &[EquityPoint], initial_cash: f64) -> Vec<DrawdownEpisode> {
    if equity.is_empty() {
        return Vec::new();
    }
    let mut episodes = Vec::new();
    let mut peak_value = initial_cash;
    let mut peak_ts = equity[0].ts;
    let mut in_episode = false;
    let mut trough_value = initial_cash;
    let mut trough_ts = equity[0].ts;

    for point in equity {
        if point.equity >= peak_value {
            if in_episode {
                episodes.push(DrawdownEpisode {
                    peak_ts,
                    trough_ts,
                    recovery_ts: Some(point.ts),
                    depth: 1.0 - trough_value / peak_value,
                    duration_days: days_between(peak_ts, point.ts),
                    recovered: true,
                });
                in_episode = false;
            }
            peak_value = point.equity;
            peak_ts = point.ts;
        } else if !in_episode {
            in_episode = true;
            trough_value = point.equity;
            trough_ts = point.ts;
        } else if point.equity < trough_value {
            trough_value = point.equity;
            trough_ts = point.ts;
        }
    }
    if in_episode {
        episodes.push(DrawdownEpisode {
            peak_ts,
            trough_ts,
            recovery_ts: None,
            depth: 1.0 - trough_value / peak_value,
            duration_days: days_between(peak_ts, equity[equity.len() - 1].ts),
            recovered: false,
        });
    }
    episodes
}

pub fn monthly_returns(equity: &[EquityPoint], initial_cash: f64) -> Vec<MonthlyReturn> {
    if equity.is_empty() {
        return Vec::new();
    }
    let mut buckets: BTreeMap<i32, f64> = BTreeMap::new();
    let mut previous = initial_cash;
    for point in equity {
        let Some(dt) = DateTime::from_timestamp_millis(point.ts) else {
            continue;
        };
        let key = dt.year() * 100 + dt.month() as i32;
        let r = if previous > 0.0 {
            point.equity / previous - 1.0
        } else {
            0.0
        };
        *buckets.entry(key).or_insert(1.0) *= 1.0 + r;
        previous = point.equity;
    }
    buckets
        .into_iter()
        .filter_map(|(key, growth)| {
            let year = key / 100;
            let month = (key % 100) as u32;
            finite((growth - 1.0) * 100.0).map(|return_pct| MonthlyReturn {
                year,
                month,
                return_pct,
            })
        })
        .collect()
}

fn series_drawdowns(equity: &[EquityPoint], initial_cash: f64) -> Vec<f64> {
    let mut peak = initial_cash;
    let mut out = Vec::with_capacity(equity.len());
    for point in equity {
        if point.equity > peak {
            peak = point.equity;
        }
        out.push(if peak > 0.0 {
            (point.equity / peak - 1.0).min(0.0)
        } else {
            0.0
        });
    }
    out
}

fn equity_returns(equity: &[EquityPoint], initial_cash: f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(equity.len());
    let mut previous = initial_cash;
    for point in equity {
        if previous > 0.0 {
            out.push(point.equity / previous - 1.0);
        }
        previous = point.equity;
    }
    out
}

fn calendar_years(first: Ts, last: Ts) -> Option<f64> {
    let days = days_between(first, last);
    if days > 0.0 {
        Some(days / 365.25)
    } else {
        None
    }
}

fn days_between(first: Ts, last: Ts) -> f64 {
    (last - first) as f64 / 86_400_000.0
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn sample_std(values: &[f64], mean: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let variance = values
        .iter()
        .map(|value| (value - mean) * (value - mean))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    variance.sqrt()
}

fn finite(value: f64) -> Option<f64> {
    if value.is_finite() { Some(value) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(values: &[f64]) -> Vec<EquityPoint> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| EquityPoint {
                ts: 1_700_000_000_000 + index as i64 * 86_400_000,
                equity: *value,
                cash: *value,
                gross_exposure: 0.0,
                drawdown: 0.0,
            })
            .collect()
    }

    #[test]
    fn max_drawdown_and_ulcer_match_hand_calculation() {
        let equity = points(&[100.0, 110.0, 99.0, 120.0]);
        let metrics = compute_metrics(&MetricInput {
            equity: &equity,
            trades: &[],
            fills: &[],
            initial_cash: 100.0,
            periods_per_year: 252,
            risk_free_rate: 0.0,
        });
        assert!((metrics.max_drawdown.unwrap() - 0.10).abs() < 1e-12);
        assert!((metrics.ulcer.unwrap() - 0.05).abs() < 1e-12);
        let episodes = drawdown_episodes(&equity, 100.0, 5);
        assert_eq!(episodes.len(), 1);
        assert!(episodes[0].recovered);
        assert_eq!(episodes[0].recovery_ts, Some(equity[3].ts));
    }

    #[test]
    fn zero_drawdown_is_not_negative_zero() {
        let equity = points(&[100.0, 100.0, 100.0]);
        let metrics = compute_metrics(&MetricInput {
            equity: &equity,
            trades: &[],
            fills: &[],
            initial_cash: 100.0,
            periods_per_year: 365,
            risk_free_rate: 0.0,
        });
        let max_drawdown = metrics.max_drawdown.expect("max drawdown");
        assert_eq!(max_drawdown, 0.0);
        assert!(!max_drawdown.is_sign_negative());
    }
}
