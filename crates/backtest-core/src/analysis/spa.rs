//! SPA / Reality Check(Stationary bootstrap 近似)。

use serde::{Deserialize, Serialize};

use super::rng::{SplitMix64, seed_for};
use crate::error::AnalysisError;

/// 本模块的错误类型;调用方通过 `Error::Analysis` 转换。
type Result<T> = std::result::Result<T, AnalysisError>;

/// SPA 结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpaReport {
    pub p_value: f64,
    pub statistic: f64,
    pub trials: usize,
    pub samples: usize,
    pub block_length: usize,
    pub best_label: Option<String>,
    pub benchmark: String,
    pub variant: String,
    pub warnings: Vec<String>,
}

/// SPA 输入。
#[derive(Clone, Debug)]
pub struct SpaInput<'a> {
    pub labels: &'a [String],
    /// `returns[trial][observation]`。
    pub returns: &'a [Vec<f64>],
    pub benchmark: &'a [f64],
    pub samples: usize,
    pub block_length: usize,
    pub seed: u64,
    pub min_trials: usize,
}

#[derive(Clone, Debug)]
pub struct SpaComputation {
    pub report: SpaReport,
    pub warnings: Vec<String>,
}

/// 计算 SPA。
pub fn spa_test(input: &SpaInput<'_>) -> Result<SpaComputation> {
    if input.labels.len() != input.returns.len() {
        return Err(AnalysisError::Invalid(format!(
            "SPA 标签数 {} 与试验数 {} 不一致",
            input.labels.len(),
            input.returns.len()
        )));
    }
    if input.returns.is_empty() {
        return Err(AnalysisError::Invalid("SPA 收益矩阵为空".into()));
    }
    if input.samples < 1 {
        return Err(AnalysisError::Invalid("SPA samples 必须 >=1".into()));
    }
    // 对齐长度
    let t_full = input.returns[0].len().min(input.benchmark.len());
    if t_full < 2 {
        return Err(AnalysisError::Invalid("SPA 至少需要 2 个观测".into()));
    }
    for row in input.returns {
        if row.len() < t_full {
            return Err(AnalysisError::Invalid("SPA 试验收益长度不足".into()));
        }
    }

    let mut warnings = Vec::new();
    if input.returns.len() < input.min_trials {
        warnings.push(format!(
            "试验数 {} 低于 min_trials={},SPA 检验功效较低",
            input.returns.len(),
            input.min_trials
        ));
    }

    // d_i,t
    let mut d: Vec<Vec<f64>> = Vec::with_capacity(input.returns.len());
    for row in input.returns {
        let mut di = Vec::with_capacity(t_full);
        for t in 0..t_full {
            di.push(row[t] - input.benchmark[t]);
        }
        d.push(di);
    }

    // observed stats
    let mut trial_t: Vec<Option<f64>> = Vec::with_capacity(d.len());
    let mut trial_sd: Vec<f64> = Vec::with_capacity(d.len());
    let mut best_index: Option<usize> = None;
    let mut best_t = f64::NEG_INFINITY;
    for (i, di) in d.iter().enumerate() {
        let mean = di.iter().sum::<f64>() / t_full as f64;
        let var = di
            .iter()
            .map(|v| (v - mean) * (v - mean))
            .sum::<f64>()
            / (t_full as f64 - 1.0);
        let sd = var.sqrt();
        if !sd.is_finite() || sd <= 0.0 {
            trial_t.push(None);
            trial_sd.push(0.0);
            continue;
        }
        let t_stat = (t_full as f64).sqrt() * mean / sd;
        trial_t.push(Some(t_stat));
        trial_sd.push(sd);
        if t_stat > best_t {
            best_t = t_stat;
            best_index = Some(i);
        }
    }
    let observed = if best_index.is_none() {
        0.0
    } else {
        best_t
    };
    if best_index.is_none() {
        warnings.push("所有试验与基准无差异,SPA p 值为 1".into());
    }

    // centered d for null
    let centered: Vec<Vec<f64>> = d
        .iter()
        .map(|di| {
            let mean = di.iter().sum::<f64>() / t_full as f64;
            di.iter().map(|v| v - mean).collect()
        })
        .collect();

    let l = if input.block_length == 0 {
        (t_full as f64).powf(1.0 / 3.0).round().max(1.0) as usize
    } else {
        input.block_length
    };

    // bootstrap
    let mut count = 0usize;
    if best_index.is_some() {
    for s in 0..input.samples {
        let seed = seed_for(input.seed, "spa", s as u64);
        let mut rng = SplitMix64::new(seed);
        // generate stationary bootstrap indices length T
        let mut indices = Vec::with_capacity(t_full);
        let mut current = (rng.next_u64() as usize) % t_full;
        for _ in 0..t_full {
            indices.push(current);
            if rng.next_f64() < 1.0 / l as f64 {
                current = (rng.next_u64() as usize) % t_full;
            } else {
                current = (current + 1) % t_full;
            }
        }
        // compute max bootstrap stat
        let mut boot_best = f64::NEG_INFINITY;
        for i in 0..d.len() {
            if trial_sd[i] <= 0.0 {
                continue;
            }
            let mean_boot = indices.iter().map(|&idx| centered[i][idx]).sum::<f64>()
                / t_full as f64;
            let t_boot = (t_full as f64).sqrt() * mean_boot / trial_sd[i];
            if t_boot > boot_best {
                boot_best = t_boot;
            }
        }
        if boot_best >= observed {
            count += 1;
        }
    }
    }

    let p_value = if best_index.is_none() {
        1.0
    } else {
        (1 + count) as f64 / (input.samples + 1) as f64
    };
    let best_label = best_index.map(|i| input.labels[i].clone());

    Ok(SpaComputation {
        report: SpaReport {
            p_value,
            statistic: observed,
            trials: d.len(),
            samples: input.samples,
            block_length: l,
            best_label,
            benchmark: "user".into(),
            variant: "studentized stationary bootstrap".into(),
            warnings: warnings.clone(),
        },
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn returns(trials: usize, obs: usize, seed: u64) -> Vec<Vec<f64>> {
        let mut state = seed;
        (0..trials)
            .map(|_| {
                (0..obs)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.02
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn all_equal_to_benchmark_gives_high_p() {
        let trials = 5;
        let obs = 200;
        let benchmark = vec![0.0; obs];
        // make all trials equal to benchmark
        let r0 = vec![vec![0.0; obs]; trials];
        let labels: Vec<String> = (0..trials).map(|i| format!("t{i}")).collect();
        let comp = spa_test(&SpaInput {
            labels: &labels,
            returns: &r0,
            benchmark: &benchmark,
            samples: 200,
            block_length: 0,
            seed: 1,
            min_trials: 2,
        })
        .expect("spa");
        assert!(comp.report.p_value > 0.5, "p={}", comp.report.p_value);
    }

    #[test]
    fn strong_trial_gives_low_p() {
        let trials = 5;
        let obs = 200;
        let mut r = returns(trials, obs, 2);
        for v in &mut r[0] {
            *v += 0.01;
        }
        let benchmark = vec![0.0; obs];
        let labels: Vec<String> = (0..trials).map(|i| format!("t{i}")).collect();
        let comp = spa_test(&SpaInput {
            labels: &labels,
            returns: &r,
            benchmark: &benchmark,
            samples: 200,
            block_length: 0,
            seed: 2,
            min_trials: 2,
        })
        .expect("spa");
        assert!(comp.report.p_value < 0.5, "p={}", comp.report.p_value);
        assert_eq!(comp.report.best_label.as_deref(), Some("t0"));
    }

    #[test]
    fn rejects_empty() {
        let labels: Vec<String> = vec![];
        let returns: Vec<Vec<f64>> = vec![];
        let benchmark = vec![0.0; 10];
        assert!(matches!(
            spa_test(&SpaInput {
                labels: &labels,
                returns: &returns,
                benchmark: &benchmark,
                samples: 10,
                block_length: 0,
                seed: 1,
                min_trials: 2,
            }),
            Err(AnalysisError::Invalid(_))
        ));
    }
}
