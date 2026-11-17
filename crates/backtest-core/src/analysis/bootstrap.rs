//! Stationary bootstrap 与百分位置信区间。
//!
//! 只依赖 [`SplitMix64`](super::rng::SplitMix64),不共享可变随机状态。每个
//! 重采样样本由 `seed_for(master, "bootstrap", sample_index)` 独立派生,因
//! 此结果与线程数、执行顺序无关。

use serde::{Deserialize, Serialize};

use super::rng::{SplitMix64, seed_for};

/// 默认置信水平。
pub const DEFAULT_CONFIDENCE: f64 = 0.95;

const BOOTSTRAP_DOMAIN: &str = "bootstrap";

/// bootstrap 统计量。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapStatistic {
    /// 每期算术平均收益。
    Mean,
    /// 每期样本标准差(`ddof=1`)。
    Volatility,
    /// `mean / std * sqrt(periods_per_year)`。
    Sharpe,
    /// 复利路径年化收益。
    Cagr,
    /// 复利路径最大回撤,正数表示回撤幅度。
    MaxDrawdown,
}

impl BootstrapStatistic {
    pub const ALL: [Self; 5] = [
        Self::Mean,
        Self::Volatility,
        Self::Sharpe,
        Self::Cagr,
        Self::MaxDrawdown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::Volatility => "volatility",
            Self::Sharpe => "sharpe",
            Self::Cagr => "cagr",
            Self::MaxDrawdown => "max_drawdown",
        }
    }
}

/// 单个统计量的点估计与百分位区间。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootstrapStat {
    pub name: BootstrapStatistic,
    pub point: Option<f64>,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    /// 参与区间计算的有效重采样样本数。
    pub valid_samples: usize,
}

/// bootstrap 结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootstrapReport {
    pub samples: usize,
    pub block_length: usize,
    pub confidence: f64,
    pub statistics: Vec<BootstrapStat>,
}

/// bootstrap 输入。
#[derive(Clone, Debug)]
pub struct BootstrapInput<'a> {
    /// 按时间升序的每期简单收益。
    pub returns: &'a [f64],
    pub periods_per_year: u32,
    /// 主种子;每个重采样样本再派生独立子种子。
    pub seed: u64,
    pub samples: usize,
    /// 期望块长;`0` 表示自动 `round(T^(1/3))`。
    pub block_length: usize,
    /// 置信水平;非法值回退到 [`DEFAULT_CONFIDENCE`]。
    pub confidence: f64,
}

/// 计算结果与告警。告警由上层汇总到分析报告。
#[derive(Clone, Debug)]
pub struct BootstrapComputation {
    pub report: BootstrapReport,
    pub warnings: Vec<String>,
}

/// 执行 stationary bootstrap。
///
/// 每个样本先按 `1/L` 的概率重新开始一个块,否则延续上一块;块首在
/// `0..T` 均匀抽取,块尾越界后回到 0。区间使用百分位法。
pub fn stationary_bootstrap(input: &BootstrapInput<'_>) -> BootstrapComputation {
    let returns = input.returns;
    let t_len = returns.len();
    let block_length = if input.block_length == 0 {
        auto_block_length(t_len)
    } else {
        input.block_length.max(1)
    };
    let (confidence, confidence_reset) =
        if input.confidence.is_finite() && input.confidence > 0.0 && input.confidence < 1.0 {
            (input.confidence, false)
        } else {
            (DEFAULT_CONFIDENCE, true)
        };
    let mut warnings = Vec::new();
    if confidence_reset {
        warnings.push(format!(
            "confidence={} 非法,已回退到 {DEFAULT_CONFIDENCE}",
            input.confidence
        ));
    }

    let report = BootstrapReport {
        samples: input.samples,
        block_length,
        confidence,
        statistics: Vec::new(),
    };

    if input.samples == 0 {
        warnings.push("bootstrap_samples=0,跳过重采样".into());
        return BootstrapComputation {
            report: BootstrapReport {
                statistics: empty_statistics(),
                ..report
            },
            warnings,
        };
    }
    if t_len < 2 {
        warnings.push(format!("收益序列长度 {t_len} < 2,无法估计 bootstrap 区间"));
        return BootstrapComputation {
            report: BootstrapReport {
                statistics: empty_statistics(),
                ..report
            },
            warnings,
        };
    }
    if let Some(index) = returns.iter().position(|value| !value.is_finite()) {
        warnings.push(format!(
            "收益序列第 {index} 个值非有限,拒绝计算 bootstrap 区间"
        ));
        return BootstrapComputation {
            report: BootstrapReport {
                statistics: empty_statistics(),
                ..report
            },
            warnings,
        };
    }
    if returns.iter().all(|value| *value == 0.0) {
        warnings.push("收益全为零,无法估计 bootstrap 区间".into());
        return BootstrapComputation {
            report: BootstrapReport {
                statistics: empty_statistics(),
                ..report
            },
            warnings,
        };
    }
    if input.periods_per_year == 0 {
        warnings.push("periods_per_year=0,Sharpe 与 CAGR 保持为空".into());
    }

    let point_indices: Vec<usize> = (0..t_len).collect();
    let point = compute_sample(returns, &point_indices, input.periods_per_year);
    let Some(point) = point else {
        warnings.push("原始收益序列的统计量非有限,拒绝计算 bootstrap 区间".into());
        return BootstrapComputation {
            report: BootstrapReport {
                statistics: empty_statistics(),
                ..report
            },
            warnings,
        };
    };

    let mut mean_values = Vec::with_capacity(input.samples);
    let mut volatility_values = Vec::with_capacity(input.samples);
    let mut sharpe_values = Vec::with_capacity(input.samples);
    let mut cagr_values = Vec::with_capacity(input.samples);
    let mut drawdown_values = Vec::with_capacity(input.samples);
    let mut invalid_samples = 0usize;
    let restart = 1.0 / block_length as f64;
    let mut indices = vec![0usize; t_len];
    for sample in 0..input.samples {
        let mut rng = SplitMix64::new(seed_for(input.seed, BOOTSTRAP_DOMAIN, sample as u64));
        let mut cursor = 0usize;
        for (position, slot) in indices.iter_mut().enumerate() {
            if position == 0 || rng.next_f64() < restart {
                cursor = rng.below(t_len);
            } else {
                cursor += 1;
                if cursor == t_len {
                    cursor = 0;
                }
            }
            *slot = cursor;
        }
        let Some(stats) = compute_sample(returns, &indices, input.periods_per_year) else {
            invalid_samples += 1;
            continue;
        };
        mean_values.push(stats.mean);
        volatility_values.push(stats.std);
        if let Some(value) = stats.sharpe {
            sharpe_values.push(value);
        }
        if let Some(value) = stats.cagr {
            cagr_values.push(value);
        }
        if let Some(value) = stats.max_drawdown {
            drawdown_values.push(value);
        }
    }
    if invalid_samples > 0 {
        warnings.push(format!(
            "{invalid_samples}/{} 个重采样样本统计量非有限,已跳过",
            input.samples
        ));
    }

    let statistics = vec![
        build_stat(
            BootstrapStatistic::Mean,
            Some(point.mean),
            &mut mean_values,
            input.samples,
            confidence,
            &mut warnings,
        ),
        build_stat(
            BootstrapStatistic::Volatility,
            Some(point.std),
            &mut volatility_values,
            input.samples,
            confidence,
            &mut warnings,
        ),
        build_stat(
            BootstrapStatistic::Sharpe,
            point.sharpe,
            &mut sharpe_values,
            input.samples,
            confidence,
            &mut warnings,
        ),
        build_stat(
            BootstrapStatistic::Cagr,
            point.cagr,
            &mut cagr_values,
            input.samples,
            confidence,
            &mut warnings,
        ),
        build_stat(
            BootstrapStatistic::MaxDrawdown,
            point.max_drawdown,
            &mut drawdown_values,
            input.samples,
            confidence,
            &mut warnings,
        ),
    ];

    BootstrapComputation {
        report: BootstrapReport {
            statistics,
            ..report
        },
        warnings,
    }
}

fn auto_block_length(t_len: usize) -> usize {
    if t_len == 0 {
        return 1;
    }
    let value = (t_len as f64).cbrt().round() as usize;
    value.max(1)
}

fn empty_statistics() -> Vec<BootstrapStat> {
    BootstrapStatistic::ALL
        .iter()
        .map(|name| BootstrapStat {
            name: *name,
            point: None,
            lower: None,
            upper: None,
            valid_samples: 0,
        })
        .collect()
}

fn build_stat(
    name: BootstrapStatistic,
    point: Option<f64>,
    values: &mut [f64],
    requested_samples: usize,
    confidence: f64,
    warnings: &mut Vec<String>,
) -> BootstrapStat {
    values.sort_by(f64::total_cmp);
    let valid_samples = values.len();
    if point.is_none() {
        warnings.push(format!("{} 点估计为空", name.as_str()));
    }
    if valid_samples != requested_samples {
        warnings.push(format!(
            "{} 有 {valid_samples}/{requested_samples} 个有效重采样样本",
            name.as_str()
        ));
    }
    if valid_samples < 2 {
        if valid_samples != requested_samples {
            return BootstrapStat {
                name,
                point,
                lower: None,
                upper: None,
                valid_samples,
            };
        }
        warnings.push(format!("{} 有效样本不足,置信区间为空", name.as_str()));
        return BootstrapStat {
            name,
            point,
            lower: None,
            upper: None,
            valid_samples,
        };
    }
    let alpha = (1.0 - confidence) / 2.0;
    BootstrapStat {
        name,
        point,
        lower: percentile(values, alpha),
        upper: percentile(values, 1.0 - alpha),
        valid_samples,
    }
}

pub(crate) fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    if sorted.len() == 1 {
        return Some(sorted[0]);
    }
    let position = p * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        return Some(sorted[lower]);
    }
    let fraction = position - lower as f64;
    Some(sorted[lower] * (1.0 - fraction) + sorted[upper] * fraction)
}

struct SampleStats {
    mean: f64,
    std: f64,
    sharpe: Option<f64>,
    cagr: Option<f64>,
    max_drawdown: Option<f64>,
}

fn compute_sample(
    returns: &[f64],
    indices: &[usize],
    periods_per_year: u32,
) -> Option<SampleStats> {
    let count = indices.len();
    if count < 2 {
        return None;
    }
    let n = count as f64;
    let sum = indices.iter().map(|&index| returns[index]).sum::<f64>();
    if !sum.is_finite() {
        return None;
    }
    let mean = sum / n;
    if !mean.is_finite() {
        return None;
    }
    let variance = indices
        .iter()
        .map(|&index| {
            let delta = returns[index] - mean;
            delta * delta
        })
        .sum::<f64>()
        / (n - 1.0);
    if !variance.is_finite() || variance < 0.0 {
        return None;
    }
    let std = variance.sqrt();
    let sharpe = if std > 0.0 && periods_per_year > 0 {
        let value = mean / std * (periods_per_year as f64).sqrt();
        value.is_finite().then_some(value)
    } else {
        None
    };
    let cagr = if periods_per_year > 0 {
        compound_cagr(returns, indices, count, periods_per_year)
    } else {
        None
    };
    let max_drawdown = compound_max_drawdown(returns, indices);
    Some(SampleStats {
        mean,
        std,
        sharpe,
        cagr,
        max_drawdown,
    })
}

fn compound_cagr(
    returns: &[f64],
    indices: &[usize],
    count: usize,
    periods_per_year: u32,
) -> Option<f64> {
    let mut growth = 1.0_f64;
    for &index in indices {
        let step = 1.0 + returns[index];
        if step <= 0.0 {
            return None;
        }
        growth *= step;
        if !growth.is_finite() {
            return None;
        }
    }
    let exponent = periods_per_year as f64 / count as f64;
    let value = growth.powf(exponent) - 1.0;
    value.is_finite().then_some(value)
}

fn compound_max_drawdown(returns: &[f64], indices: &[usize]) -> Option<f64> {
    let mut wealth = 1.0_f64;
    let mut peak = 1.0_f64;
    let mut max_drawdown = 0.0_f64;
    for &index in indices {
        let step = 1.0 + returns[index];
        if step <= 0.0 {
            return Some(1.0);
        }
        wealth *= step;
        if !wealth.is_finite() {
            return None;
        }
        if wealth > peak {
            peak = wealth;
        }
        let drawdown = 1.0 - wealth / peak;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
    }
    Some(max_drawdown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap(returns: &[f64], seed: u64, samples: usize) -> BootstrapComputation {
        stationary_bootstrap(&BootstrapInput {
            returns,
            periods_per_year: 365,
            seed,
            samples,
            block_length: 0,
            confidence: DEFAULT_CONFIDENCE,
        })
    }

    fn stat(report: &BootstrapReport, name: BootstrapStatistic) -> &BootstrapStat {
        report
            .statistics
            .iter()
            .find(|stat| stat.name == name)
            .expect("statistic")
    }

    #[test]
    fn auto_block_length_uses_cube_root() {
        assert_eq!(super::auto_block_length(1), 1);
        assert_eq!(super::auto_block_length(8), 2);
        assert_eq!(super::auto_block_length(27), 3);
        assert_eq!(super::auto_block_length(1_300), 11);
    }

    #[test]
    fn point_estimates_match_hand_calculation() {
        let returns = [0.1, -0.1];
        let computation = stationary_bootstrap(&BootstrapInput {
            returns: &returns,
            periods_per_year: 2,
            seed: 42,
            samples: 100,
            block_length: 0,
            confidence: DEFAULT_CONFIDENCE,
        });
        let report = &computation.report;
        let mean = stat(report, BootstrapStatistic::Mean).point.expect("mean");
        assert!((mean - 0.0).abs() < 1e-15);
        let volatility = stat(report, BootstrapStatistic::Volatility)
            .point
            .expect("volatility");
        assert!((volatility - 0.141_421_356_237_309_5).abs() < 1e-15);
        let cagr = stat(report, BootstrapStatistic::Cagr).point.expect("cagr");
        assert!((cagr - (-0.01)).abs() < 1e-12);
        let drawdown = stat(report, BootstrapStatistic::MaxDrawdown)
            .point
            .expect("max drawdown");
        assert!((drawdown - 0.1).abs() < 1e-12);
    }

    #[test]
    fn bootstrap_is_deterministic_for_seed_and_changes_with_seed() {
        let returns: Vec<f64> = (0..200)
            .map(|index| ((index as f64) * 0.37).sin() * 0.01 + 0.0005)
            .collect();
        let first = bootstrap(&returns, 42, 200);
        let second = bootstrap(&returns, 42, 200);
        assert_eq!(first.report, second.report);
        assert_eq!(first.warnings, second.warnings);
        let third = bootstrap(&returns, 43, 200);
        assert_ne!(first.report, third.report);
    }

    #[test]
    fn confidence_intervals_bracket_point_estimates() {
        let returns: Vec<f64> = (0..120)
            .map(|index| ((index as f64) * 0.21).cos() * 0.012)
            .collect();
        let report = bootstrap(&returns, 7, 300).report;
        for stat in &report.statistics {
            let point = stat.point.expect("point");
            let lower = stat.lower.expect("lower");
            let upper = stat.upper.expect("upper");
            assert!(lower <= upper, "{:?}", stat.name);
            assert!(
                point >= lower - 1e-12 && point <= upper + 1e-12,
                "{:?} point={point} interval=[{lower}, {upper}]",
                stat.name
            );
        }
    }

    #[test]
    fn mean_interval_coverage_is_reasonable() {
        let mut covered = 0usize;
        let trials = 120usize;
        for trial in 0..trials {
            let returns: Vec<f64> = (0..80)
                .map(|index| {
                    let phase = (trial * 80 + index) as f64 * 0.618_033_988_75;
                    (phase.sin() + (phase * 1.7).cos()) * 0.01
                })
                .collect();
            let report = bootstrap(&returns, trial as u64, 200).report;
            let mean = stat(&report, BootstrapStatistic::Mean);
            let (Some(lower), Some(upper)) = (mean.lower, mean.upper) else {
                continue;
            };
            let point = mean.point.expect("point");
            if point >= lower - 1e-12 && point <= upper + 1e-12 {
                covered += 1;
            }
        }
        let coverage = covered as f64 / trials as f64;
        assert!(
            (0.80..=1.0).contains(&coverage),
            "mean CI 覆盖率异常: {coverage}"
        );
    }

    #[test]
    fn handles_short_constant_and_non_finite_inputs() {
        let short = bootstrap(&[0.01], 1, 100);
        assert!(
            short
                .report
                .statistics
                .iter()
                .all(|stat| stat.point.is_none())
        );
        assert!(
            short
                .warnings
                .iter()
                .any(|warning| warning.contains("长度 1"))
        );

        let flat = bootstrap(&[0.0, 0.0, 0.0], 1, 100);
        assert!(
            flat.report
                .statistics
                .iter()
                .all(|stat| stat.point.is_none())
        );
        assert!(
            flat.warnings
                .iter()
                .any(|warning| warning.contains("全为零"))
        );

        let broken = bootstrap(&[0.01, f64::NAN], 1, 100);
        assert!(
            broken
                .report
                .statistics
                .iter()
                .all(|stat| stat.point.is_none())
        );
        assert!(
            broken
                .warnings
                .iter()
                .any(|warning| warning.contains("非有限"))
        );
    }

    #[test]
    fn zero_variance_keeps_sharpe_null_but_mean_valid() {
        let result = bootstrap(&[0.005, 0.005, 0.005, 0.005], 3, 100);
        let mean = stat(&result.report, BootstrapStatistic::Mean);
        assert!((mean.point.expect("mean") - 0.005).abs() < 1e-15);
        let sharpe = stat(&result.report, BootstrapStatistic::Sharpe);
        assert!(sharpe.point.is_none());
        assert!(sharpe.lower.is_none());
        assert!(sharpe.upper.is_none());
    }

    #[test]
    fn zero_samples_is_an_empty_report() {
        let result = bootstrap(&[0.01, -0.01], 1, 0);
        assert_eq!(result.report.statistics.len(), 5);
        assert!(
            result
                .report
                .statistics
                .iter()
                .all(|stat| stat.valid_samples == 0)
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("跳过"))
        );
    }
}
