//! CSCV / 回测过拟合概率(PBO)。
//!
//! 输入是试验收益矩阵(外层试验、内层时间),算法把时间切成 `S` 个连续块,
//! 枚举 `S/2` 个块作为样本内,其余为样本外,统计样本内最优试验在样本外的
//! 相对排名。实现只做确定性算术,不使用随机数。

use serde::{Deserialize, Serialize};

use super::bootstrap::percentile;
use crate::error::AnalysisError;

/// 本模块的错误类型;调用方通过 `Error::Analysis` 转换。
type Result<T> = std::result::Result<T, AnalysisError>;

/// PBO 结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PboReport {
    pub pbo: f64,
    /// 实际参与统计的组合数。
    pub splits: usize,
    pub blocks: usize,
    pub trials: usize,
    pub observations: usize,
    pub logit_mean: f64,
    pub logit_p05: f64,
    pub logit_p50: f64,
    pub logit_p95: f64,
    /// 被选为样本内最优次数最多的试验标签。
    pub best_label: Option<String>,
    pub warnings: Vec<String>,
}

/// PBO 输入。
#[derive(Clone, Debug)]
pub struct PboInput<'a> {
    pub labels: &'a [String],
    /// `returns[trial][observation]`,所有行等长。
    pub returns: &'a [Vec<f64>],
    pub blocks: usize,
    pub periods_per_year: u32,
    pub min_trials: usize,
}

#[derive(Clone, Debug)]
pub struct PboComputation {
    pub report: PboReport,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Moment {
    sum: f64,
    sumsq: f64,
    count: usize,
}

impl Moment {
    fn add(self, other: Self) -> Self {
        Self {
            sum: self.sum + other.sum,
            sumsq: self.sumsq + other.sumsq,
            count: self.count + other.count,
        }
    }

    fn sharpe(self, periods_per_year: u32) -> Option<f64> {
        if self.count < 2 || !self.sum.is_finite() || !self.sumsq.is_finite() {
            return None;
        }
        let n = self.count as f64;
        let mean = self.sum / n;
        let variance = (self.sumsq - self.sum * self.sum / n) / (n - 1.0);
        if !variance.is_finite() || variance <= 0.0 {
            return None;
        }
        let scale = (periods_per_year.max(1) as f64).sqrt();
        let value = mean / variance.sqrt() * scale;
        value.is_finite().then_some(value)
    }
}

/// 计算 CSCV / PBO。
pub fn probability_of_backtest_overfitting(input: &PboInput<'_>) -> Result<PboComputation> {
    let trials = input.returns.len();
    if trials < 2 {
        return Err(AnalysisError::Invalid("PBO 至少需要 2 个试验".into()));
    }
    if input.labels.len() != trials {
        return Err(AnalysisError::Invalid(format!(
            "PBO 标签数 {} 与试验数 {trials} 不一致",
            input.labels.len()
        )));
    }
    let observations = input.returns[0].len();
    if observations < 2 {
        return Err(AnalysisError::Invalid(
            "PBO 每个试验至少需要 2 个收益观测".into(),
        ));
    }
    if input.returns.iter().any(|row| row.len() != observations) {
        return Err(AnalysisError::Invalid("PBO 收益矩阵行长度不一致".into()));
    }
    let blocks = input.blocks;
    if blocks < 4 || !blocks.is_multiple_of(2) {
        return Err(AnalysisError::Invalid(
            "PBO blocks 必须是 >= 4 的偶数".into(),
        ));
    }
    if blocks > observations {
        return Err(AnalysisError::Invalid(format!(
            "PBO blocks={blocks} 超过观测数 {observations}"
        )));
    }

    let mut warnings = Vec::new();
    if trials < input.min_trials {
        warnings.push(format!(
            "试验数 {trials} 低于 min_trials={},PBO 噪声较大",
            input.min_trials
        ));
    }

    let block_shapes = block_ranges(observations, blocks)?;
    let moments = block_moments(input.returns, &block_shapes);
    let masks = combinations(blocks, blocks / 2);
    if masks.len() > 10_000 {
        warnings.push(format!(
            "块数 {blocks} 产生 {} 个组合,计算时间较长",
            masks.len()
        ));
    }

    let full_mask = if blocks == 32 {
        u32::MAX
    } else {
        (1u32 << blocks) - 1
    };
    let mut logits = Vec::with_capacity(masks.len());
    let mut best_counts = vec![0usize; trials];
    let mut skipped = 0usize;
    for mask in &masks {
        let is_sharpe: Vec<Option<f64>> = moments
            .iter()
            .map(|row| {
                let selected = row
                    .iter()
                    .enumerate()
                    .filter(|(block, _)| mask & (1 << block) != 0)
                    .fold(Moment::default(), |acc, (_, moment)| acc.add(*moment));
                selected.sharpe(input.periods_per_year)
            })
            .collect();
        let Some(best_index) = best_index(&is_sharpe) else {
            skipped += 1;
            continue;
        };
        best_counts[best_index] += 1;

        let complement = full_mask ^ mask;
        let oos_sharpe: Vec<Option<f64>> = moments
            .iter()
            .map(|row| {
                let selected = row
                    .iter()
                    .enumerate()
                    .filter(|(block, _)| complement & (1 << block) != 0)
                    .fold(Moment::default(), |acc, (_, moment)| acc.add(*moment));
                selected.sharpe(input.periods_per_year)
            })
            .collect();
        let best_value = oos_sharpe[best_index];
        let worse = oos_sharpe
            .iter()
            .enumerate()
            .filter(|(index, value)| is_worse(**value, *index, best_value, best_index))
            .count();
        let rank = worse + 1;
        let omega = rank as f64 / (trials + 1) as f64;
        logits.push((omega / (1.0 - omega)).ln());
    }

    let evaluated = logits.len();
    if evaluated == 0 {
        return Err(AnalysisError::Invalid(
            "PBO 没有任何组合能计算出有效 Sharpe".into(),
        ));
    }
    if skipped > 0 {
        warnings.push(format!(
            "{skipped}/{} 个组合的样本内 Sharpe 无效,已跳过",
            masks.len()
        ));
    }
    logits.sort_by(f64::total_cmp);
    let overfit = logits.iter().filter(|value| **value <= 0.0).count();
    let pbo = overfit as f64 / evaluated as f64;
    let logit_mean = logits.iter().sum::<f64>() / evaluated as f64;
    let logit_p05 = percentile(&logits, 0.05).unwrap_or(logit_mean);
    let logit_p50 = percentile(&logits, 0.50).unwrap_or(logit_mean);
    let logit_p95 = percentile(&logits, 0.95).unwrap_or(logit_mean);
    let best = best_counts
        .iter()
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            left.cmp(right).then_with(|| right_index.cmp(left_index))
        })
        .filter(|(_, count)| **count > 0)
        .map(|(index, _)| index);

    Ok(PboComputation {
        report: PboReport {
            pbo,
            splits: evaluated,
            blocks,
            trials,
            observations,
            logit_mean,
            logit_p05,
            logit_p50,
            logit_p95,
            best_label: best.map(|index| input.labels[index].clone()),
            warnings: warnings.clone(),
        },
        warnings,
    })
}

/// 把 `observations` 切成 `blocks` 段;前 `observations % blocks` 段多一个点。
fn block_ranges(observations: usize, blocks: usize) -> Result<Vec<(usize, usize)>> {
    if blocks == 0 {
        return Err(AnalysisError::Invalid("PBO blocks 不能为 0".into()));
    }
    let base = observations / blocks;
    let extra = observations % blocks;
    let mut ranges = Vec::with_capacity(blocks);
    let mut start = 0usize;
    for block in 0..blocks {
        let len = base + usize::from(block < extra);
        if len == 0 {
            return Err(AnalysisError::Invalid(
                "PBO 每个时间块至少需要 1 个观测".into(),
            ));
        }
        ranges.push((start, start + len));
        start += len;
    }
    Ok(ranges)
}

fn block_moments(returns: &[Vec<f64>], ranges: &[(usize, usize)]) -> Vec<Vec<Moment>> {
    returns
        .iter()
        .map(|row| {
            ranges
                .iter()
                .map(|(start, end)| {
                    row[*start..*end]
                        .iter()
                        .fold(Moment::default(), |mut moment, value| {
                            moment.sum += value;
                            moment.sumsq += value * value;
                            moment.count += 1;
                            moment
                        })
                })
                .collect()
        })
        .collect()
}

/// 按字典序生成 `blocks` 选 `choose` 的位掩码。
fn combinations(blocks: usize, choose: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let mut combo: Vec<usize> = (0..choose).collect();
    loop {
        out.push(combo.iter().fold(0u32, |mask, block| mask | (1 << block)));
        if choose == 0 {
            break;
        }
        let mut index = choose;
        loop {
            if index == 0 {
                return out;
            }
            index -= 1;
            if combo[index] < blocks - choose + index {
                combo[index] += 1;
                for next in index + 1..choose {
                    combo[next] = combo[next - 1] + 1;
                }
                break;
            }
        }
    }
    out
}

fn best_index(values: &[Option<f64>]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (index, value) in values.iter().enumerate() {
        let Some(value) = value else {
            continue;
        };
        best = match best {
            None => Some((index, *value)),
            Some((best_index, best_value)) => {
                if value.total_cmp(&best_value).is_gt() {
                    Some((index, *value))
                } else {
                    Some((best_index, best_value))
                }
            }
        };
    }
    best.map(|(index, _)| index)
}

/// `candidate` 是否严格差于 `best`;`None` 视为最差,并列按索引小者优先。
fn is_worse(
    candidate: Option<f64>,
    candidate_index: usize,
    best: Option<f64>,
    best_index: usize,
) -> bool {
    match (candidate, best) {
        (None, None) => candidate_index < best_index,
        (None, Some(_)) => true,
        (Some(_), None) => false,
        (Some(candidate), Some(best)) => {
            candidate.total_cmp(&best).is_lt()
                || (candidate == best && candidate_index < best_index)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("trial-{index}")).collect()
    }

    fn pseudo_returns(trials: usize, observations: usize, seed: u64) -> Vec<Vec<f64>> {
        let mut state = seed;
        (0..trials)
            .map(|_| {
                (0..observations)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        let unit = ((state >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0;
                        unit * 0.01
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn random_family_has_pbo_near_half() {
        let returns = pseudo_returns(40, 600, 7);
        let labels = labels(40);
        let computation = probability_of_backtest_overfitting(&PboInput {
            labels: &labels,
            returns: &returns,
            blocks: 10,
            periods_per_year: 365,
            min_trials: 20,
        })
        .expect("pbo");
        assert_eq!(computation.report.splits, 252);
        assert_eq!(computation.report.trials, 40);
        assert!(
            (0.2..=0.8).contains(&computation.report.pbo),
            "pbo={}",
            computation.report.pbo
        );
    }

    #[test]
    fn strong_trial_lowers_pbo() {
        let mut returns = pseudo_returns(20, 500, 11);
        for value in &mut returns[0] {
            *value += 0.002;
        }
        let labels = labels(20);
        let computation = probability_of_backtest_overfitting(&PboInput {
            labels: &labels,
            returns: &returns,
            blocks: 10,
            periods_per_year: 365,
            min_trials: 20,
        })
        .expect("pbo");
        assert!(
            computation.report.pbo < 0.25,
            "pbo={}",
            computation.report.pbo
        );
        assert_eq!(computation.report.best_label.as_deref(), Some("trial-0"));
    }

    #[test]
    fn is_deterministic_and_independent_of_label_order() {
        let returns = pseudo_returns(12, 300, 3);
        let labels = labels(12);
        let first = probability_of_backtest_overfitting(&PboInput {
            labels: &labels,
            returns: &returns,
            blocks: 8,
            periods_per_year: 252,
            min_trials: 10,
        })
        .expect("first");
        let second = probability_of_backtest_overfitting(&PboInput {
            labels: &labels,
            returns: &returns,
            blocks: 8,
            periods_per_year: 252,
            min_trials: 10,
        })
        .expect("second");
        assert_eq!(first.report, second.report);
        assert_eq!(first.warnings, second.warnings);
    }

    #[test]
    fn rejects_invalid_shapes() {
        let labels = labels(2);
        let returns = vec![vec![0.01; 10]];
        assert!(matches!(
            probability_of_backtest_overfitting(&PboInput {
                labels: &labels,
                returns: &returns,
                blocks: 4,
                periods_per_year: 365,
                min_trials: 2,
            }),
            Err(AnalysisError::Invalid(_))
        ));
        let returns = vec![vec![0.01; 3], vec![0.02; 3]];
        assert!(matches!(
            probability_of_backtest_overfitting(&PboInput {
                labels: &labels,
                returns: &returns,
                blocks: 4,
                periods_per_year: 365,
                min_trials: 2,
            }),
            Err(AnalysisError::Invalid(_))
        ));
    }

    #[test]
    fn combinations_count_matches_binomial() {
        assert_eq!(combinations(10, 5).len(), 252);
        assert_eq!(combinations(4, 2).len(), 6);
        assert_eq!(combinations(2, 1).len(), 2);
    }
}
