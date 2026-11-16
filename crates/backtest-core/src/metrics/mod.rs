use std::collections::BTreeMap;

use chrono::{DateTime, Datelike};

use crate::model::{
    DeflatedSharpe, DrawdownEpisode, EquityPoint, FillRecord, Metrics, MonthlyReturn, TradeRecord,
    Ts,
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
        metrics.cost_drag = finite(metrics.total_fees / avg_equity / years);
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

pub fn equity_returns(equity: &[EquityPoint], initial_cash: f64) -> Vec<f64> {
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

/// 每期(非年化)Sharpe。
pub fn period_sharpe(returns: &[f64]) -> Option<f64> {
    if returns.len() < 2 {
        return None;
    }
    let mean = mean(returns);
    let std = sample_std(returns, mean);
    if !std.is_finite() || std <= 0.0 {
        return None;
    }
    finite(mean / std)
}

/// Deflated Sharpe Ratio。
///
/// `returns` 是被选中策略的每期收益,`trial_sharpe` 是全部有效试验的每期
/// Sharpe。`N=1` 时不做多重检验校正,`SR0=0`。
pub fn deflated_sharpe(returns: &[f64], trial_sharpe: &[f64]) -> Option<DeflatedSharpe> {
    let observations = returns.len();
    if observations < 2 || trial_sharpe.is_empty() {
        return None;
    }
    let sharpe = period_sharpe(returns)?;
    let (skewness, kurtosis) = return_moments(returns)?;
    let trials = trial_sharpe.len();
    let trial_variance = if trials >= 2 {
        let trial_mean = mean(trial_sharpe);
        trial_sharpe
            .iter()
            .map(|value| (value - trial_mean) * (value - trial_mean))
            .sum::<f64>()
            / (trials - 1) as f64
    } else {
        0.0
    };
    let expected_max = if trials == 1 || !trial_variance.is_finite() || trial_variance <= 0.0 {
        0.0
    } else {
        let n = trials as f64;
        let gamma = std::f64::consts::EULER_GAMMA;
        let upper = normal_ppf(1.0 - 1.0 / n);
        let upper_e = normal_ppf(1.0 - 1.0 / (n * std::f64::consts::E));
        trial_variance.sqrt() * ((1.0 - gamma) * upper + gamma * upper_e)
    };
    let denominator_sq = 1.0 - skewness * sharpe + (kurtosis - 1.0) / 4.0 * sharpe * sharpe;
    if !denominator_sq.is_finite() || denominator_sq <= 0.0 {
        return None;
    }
    let z = (sharpe - expected_max) * ((observations as f64 - 1.0).sqrt()) / denominator_sq.sqrt();
    let probability = finite(normal_cdf(z))?;
    Some(DeflatedSharpe {
        probability,
        trials,
        sharpe_per_period: sharpe,
        expected_max_sharpe: expected_max,
        trial_sharpe_variance: trial_variance,
        skewness,
        kurtosis,
        observations,
    })
}

fn return_moments(values: &[f64]) -> Option<(f64, f64)> {
    if values.len() < 2 {
        return None;
    }
    let mean = mean(values);
    let m2 = values
        .iter()
        .map(|value| (value - mean) * (value - mean))
        .sum::<f64>()
        / values.len() as f64;
    if !m2.is_finite() || m2 <= 0.0 {
        return None;
    }
    let m3 = values
        .iter()
        .map(|value| (value - mean).powi(3))
        .sum::<f64>()
        / values.len() as f64;
    let m4 = values
        .iter()
        .map(|value| (value - mean).powi(4))
        .sum::<f64>()
        / values.len() as f64;
    Some((m3 / m2.powf(1.5), m4 / (m2 * m2)))
}

/// 标准正态分布函数。
pub fn normal_cdf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    0.5 * erfc(-x / std::f64::consts::SQRT_2)
}

/// 标准正态分位数。Acklam 近似加一步 Halley 修正。
pub fn normal_ppf(p: f64) -> f64 {
    if p.is_nan() || !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if p == 0.0 {
        return f64::NEG_INFINITY;
    }
    if p == 1.0 {
        return f64::INFINITY;
    }
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const P_LOW: f64 = 0.024_25;
    const P_HIGH: f64 = 1.0 - P_LOW;
    let mut x = if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= P_HIGH {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    let error = normal_cdf(x) - p;
    let u = error * (2.0 * std::f64::consts::PI).sqrt() * (0.5 * x * x).exp();
    x -= u / (1.0 + 0.5 * x * u);
    x
}

fn erf(x: f64) -> f64 {
    let ax = x.abs();
    let value = if ax <= 2.0 {
        erf_taylor(ax)
    } else if ax < 6.0 {
        1.0 - erfc_continued_fraction(ax)
    } else {
        1.0
    };
    value.copysign(x)
}

fn erfc(x: f64) -> f64 {
    1.0 - erf(x)
}

fn erf_taylor(x: f64) -> f64 {
    let mut sum = 0.0;
    let mut term = x;
    let mut n = 0usize;
    loop {
        let previous = sum;
        sum += term / (2 * n + 1) as f64;
        n += 1;
        term *= -x * x / n as f64;
        if (sum - previous).abs() <= 1e-17 * sum.abs().max(1.0) || n > 200 {
            break;
        }
    }
    2.0 / std::f64::consts::PI.sqrt() * sum
}

fn erfc_continued_fraction(x: f64) -> f64 {
    let mut fraction = 0.0;
    for n in (1..=64).rev() {
        fraction = 1.0 / (x + (n as f64 * 0.5) * fraction);
    }
    (-x * x).exp() / std::f64::consts::PI.sqrt() * fraction
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

    #[test]
    fn sharpe_matches_independent_calculation() {
        // 首点等于初始资金,收益为 0;随后 1%、2%、-1%、3%。
        let equity = points(&[100.0, 101.0, 103.02, 101.9898, 105.049494]);
        let metrics = compute_metrics(&MetricInput {
            equity: &equity,
            trades: &[],
            fills: &[],
            initial_cash: 100.0,
            periods_per_year: 252,
            risk_free_rate: 0.0,
        });
        let returns = [0.0, 0.01, 0.02, -0.01, 0.03];
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns
            .iter()
            .map(|value| (value - mean) * (value - mean))
            .sum::<f64>()
            / (returns.len() - 1) as f64;
        let expected = mean / variance.sqrt() * 252.0_f64.sqrt();
        assert!((metrics.sharpe.expect("sharpe") - expected).abs() < 1e-9);
    }

    #[test]
    fn empty_trades_leave_optional_metrics_null() {
        let equity = points(&[100.0, 101.0, 100.5]);
        let metrics = compute_metrics(&MetricInput {
            equity: &equity,
            trades: &[],
            fills: &[],
            initial_cash: 100.0,
            periods_per_year: 365,
            risk_free_rate: 0.0,
        });
        assert_eq!(metrics.trades, 0);
        assert!(metrics.win_rate.is_none());
        assert!(metrics.profit_factor.is_none());
        assert!(metrics.expectancy.is_none());
        assert!(metrics.cost_drag.is_some());
    }

    #[test]
    fn normal_distribution_round_trips() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-15);
        assert!((normal_cdf(1.96) - 0.975_002_104_851_78).abs() < 1e-13);
        assert!((normal_cdf(-3.0) - 0.001_349_898_031_63).abs() < 1e-13);
        for p in [
            1e-6,
            0.001,
            0.024_25,
            0.3,
            0.5,
            0.9,
            0.975_75,
            0.999,
            1.0 - 1e-6,
        ] {
            let x = normal_ppf(p);
            assert!((normal_cdf(x) - p).abs() < 1e-12, "p={p} x={x}");
        }
    }

    #[test]
    fn dsr_is_half_for_single_zero_sharpe_trial() {
        let returns = [0.01, -0.01, 0.01, -0.01];
        let dsr = deflated_sharpe(&returns, &[0.0]).expect("dsr");
        assert_eq!(dsr.trials, 1);
        assert!(dsr.sharpe_per_period.abs() < 1e-15);
        assert!((dsr.probability - 0.5).abs() < 1e-12);
    }

    #[test]
    fn dsr_falls_with_more_trials_and_rises_with_sharpe() {
        let returns = [0.02, -0.005, 0.03, -0.01, 0.015, 0.005, -0.002, 0.01];
        let trials_3 = [-0.2, 0.0, 0.2];
        let trials_5 = [-0.2, -0.2, 0.0, 0.2, 0.2];
        let low = deflated_sharpe(&returns, &trials_3).expect("n=3");
        let high = deflated_sharpe(&returns, &trials_5).expect("n=5");
        assert!(
            low.probability > high.probability,
            "{} vs {}",
            low.probability,
            high.probability
        );

        let better: Vec<f64> = returns.iter().map(|value| value + 0.01).collect();
        let better_dsr = deflated_sharpe(&better, &trials_3).expect("better");
        assert!(better_dsr.probability > low.probability);
    }

    #[test]
    fn dsr_matches_reference_implementation() {
        // 参考值由 SciPy 1.16.3 按 docs/03 的公式独立计算。
        let returns = [0.01, -0.02, 0.015, 0.03, -0.005, 0.01];
        let trials = [-0.1, 0.0, 0.1, 0.2];
        let dsr = deflated_sharpe(&returns, &trials).expect("dsr");
        assert!((dsr.sharpe_per_period - 0.387_056_499_858_091_7).abs() < 1e-12);
        assert!((dsr.skewness + 0.308_471_293_170_389_8).abs() < 1e-12);
        assert!((dsr.kurtosis - 2.251_609_645_246_811_7).abs() < 1e-12);
        assert!((dsr.trial_sharpe_variance - 0.016_666_666_666_666_673).abs() < 1e-12);
        assert!((dsr.expected_max_sharpe - 0.135_828_472_541_662_53).abs() < 1e-12);
        assert!((dsr.probability - 0.698_530_690_498_564_3).abs() < 1e-9);
    }

    #[test]
    fn cost_drag_is_fees_over_average_equity_per_year() {
        let equity = points(&[100.0, 100.0, 100.0]);
        let fills = vec![FillRecord {
            ts: equity[0].ts,
            symbol: 0,
            side: crate::model::Side::Buy,
            qty: 1.0,
            price: 100.0,
            notional: 100.0,
            fee: 1.0,
            slippage: 0.0,
            reason: crate::model::FillReason::Entry,
            ambiguous_bar: false,
        }];
        let metrics = compute_metrics(&MetricInput {
            equity: &equity,
            trades: &[],
            fills: &fills,
            initial_cash: 100.0,
            periods_per_year: 365,
            risk_free_rate: 0.0,
        });
        assert!(metrics.cost_drag.expect("cost drag") > 0.0);
    }
}
