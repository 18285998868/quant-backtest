//! 统计分析与稳健性检验内核。
//!
//! 本模块只处理收益序列与统计量,不读取配置、不依赖引擎或报告。编排层负责
//! 把权益曲线聚合成收益、把结果放进报告。
//!
//! 关闭 `[analysis]` 时不会调用这里的任何函数,因此不改变历史哈希。

mod bootstrap;
mod pbo;
mod plateau;
mod rng;

pub use bootstrap::{
    BootstrapComputation, BootstrapInput, BootstrapReport, BootstrapStat, BootstrapStatistic,
    DEFAULT_CONFIDENCE, stationary_bootstrap,
};
pub use pbo::{PboComputation, PboInput, PboReport, probability_of_backtest_overfitting};
pub use plateau::{GridPoint, PlateauComputation, PlateauInput, PlateauReport, plateau_score};
pub use rng::{SplitMix64, seed_for};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AnalysisError, Result};

/// 分析结果格式版本。
pub const ANALYSIS_VERSION: u32 = 1;

/// 单次 `run` 的分析结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub version: u32,
    /// 本报告的内容哈希;计算时 `hash` 字段本身不参与。
    pub hash: String,
    pub seed: u64,
    /// 输入收益矩阵的位模式哈希。
    pub input_hash: String,
    pub bootstrap: Option<BootstrapReport>,
    /// PBO/CSCV 结果;`run` 不计算,扫描启用分析时才有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pbo: Option<PboReport>,
    /// 参数平台结果;仅 sweep 且至少两个参数轴时计算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plateau: Option<PlateauReport>,
    pub warnings: Vec<String>,
}

/// `run` 的分析输入。
#[derive(Clone, Debug)]
pub struct RunAnalysisInput<'a> {
    pub returns: &'a [f64],
    pub periods_per_year: u32,
    pub seed: u64,
    pub bootstrap_samples: usize,
}

/// 对单条策略收益执行 bootstrap,并计算内容哈希。
pub fn analyze_run(input: &RunAnalysisInput<'_>) -> Result<AnalysisReport> {
    let computation = stationary_bootstrap(&BootstrapInput {
        returns: input.returns,
        periods_per_year: input.periods_per_year,
        seed: input.seed,
        samples: input.bootstrap_samples,
        block_length: 0,
        confidence: DEFAULT_CONFIDENCE,
    });
    let report = AnalysisReport {
        version: ANALYSIS_VERSION,
        hash: String::new(),
        seed: input.seed,
        input_hash: input_hash(input.returns, input.periods_per_year),
        bootstrap: Some(computation.report),
        pbo: None,
        plateau: None,
        warnings: computation.warnings,
    };
    finalize(report)
}

/// 扫描的分析输入。
#[derive(Clone, Debug)]
pub struct SweepAnalysisInput<'a> {
    pub labels: &'a [String],
    /// `returns[trial][observation]`;行长度不一致或含非有限值的试验会被剔除。
    pub returns: &'a [Vec<f64>],
    pub periods_per_year: u32,
    pub seed: u64,
    pub pbo_blocks: usize,
    pub min_trials: usize,
}

/// 对试验收益矩阵执行 CSCV/PBO,并计算内容哈希。
pub fn analyze_sweep(input: &SweepAnalysisInput<'_>) -> Result<AnalysisReport> {
    if input.labels.len() != input.returns.len() {
        return Err(AnalysisError::Invalid(format!(
            "标签数 {} 与试验数 {} 不一致",
            input.labels.len(),
            input.returns.len()
        ))
        .into());
    }
    let Some(expected) = input.returns.first().map(Vec::len) else {
        return Err(AnalysisError::Invalid("收益矩阵为空".into()).into());
    };
    let mut warnings = Vec::new();
    let mut labels = Vec::new();
    let mut rows: Vec<Vec<f64>> = Vec::new();
    let mut mismatched = 0usize;
    let mut non_finite = 0usize;
    let mut empty = 0usize;
    for (label, row) in input.labels.iter().zip(input.returns) {
        if row.len() != expected {
            mismatched += 1;
            continue;
        }
        if row.is_empty() {
            empty += 1;
            continue;
        }
        if row.iter().any(|value| !value.is_finite()) {
            non_finite += 1;
            continue;
        }
        labels.push(label.clone());
        rows.push(row.clone());
    }
    if mismatched > 0 {
        warnings.push(format!(
            "{mismatched} 个试验的收益长度与首行 {expected} 不一致,已剔除"
        ));
    }
    if empty > 0 {
        warnings.push(format!("{empty} 个试验没有收益观测,已剔除"));
    }
    if non_finite > 0 {
        warnings.push(format!("{non_finite} 个试验含非有限收益,已剔除"));
    }
    if rows.len() < 2 {
        warnings.push(format!(
            "有效试验只剩 {},PBO 至少需要 2 个,已跳过",
            rows.len()
        ));
    }
    if (2..input.min_trials).contains(&rows.len()) {
        warnings.push(format!(
            "有效试验 {} 低于 min_trials={},PBO 只作参考",
            rows.len(),
            input.min_trials
        ));
    }

    let pbo = if rows.len() < 2 {
        None
    } else if input.pbo_blocks > expected {
        warnings.push(format!(
            "pbo_blocks={} 超过观测数 {expected},跳过 PBO",
            input.pbo_blocks
        ));
        None
    } else {
        let computation = probability_of_backtest_overfitting(&PboInput {
            labels: &labels,
            returns: &rows,
            blocks: input.pbo_blocks,
            periods_per_year: input.periods_per_year,
            min_trials: input.min_trials,
        })?;
        warnings.extend(computation.warnings);
        Some(computation.report)
    };

    let report = AnalysisReport {
        version: ANALYSIS_VERSION,
        hash: String::new(),
        seed: input.seed,
        input_hash: matrix_input_hash(&labels, &rows, input.periods_per_year),
        bootstrap: None,
        pbo,
        plateau: None,
        warnings,
    };
    finalize(report)
}

fn finalize(mut report: AnalysisReport) -> Result<AnalysisReport> {
    report.hash = content_hash(&report)?;
    Ok(report)
}

fn content_hash(report: &AnalysisReport) -> Result<String> {
    let mut payload = serde_json::Map::new();
    payload.insert("version".into(), serde_json::json!(report.version));
    payload.insert("seed".into(), serde_json::json!(report.seed));
    payload.insert("input_hash".into(), serde_json::json!(report.input_hash));
    payload.insert("bootstrap".into(), serde_json::json!(report.bootstrap));
    payload.insert("warnings".into(), serde_json::json!(report.warnings));
    if let Some(pbo) = &report.pbo {
        payload.insert(
            "pbo".into(),
            serde_json::to_value(pbo)
                .map_err(|error| AnalysisError::Serialize(error.to_string()))?,
        );
    }
    if let Some(plateau) = &report.plateau {
        payload.insert(
            "plateau".into(),
            serde_json::to_value(plateau)
                .map_err(|error| AnalysisError::Serialize(error.to_string()))?,
        );
    }
    let bytes = serde_json::to_vec(&serde_json::Value::Object(payload))
        .map_err(|error| AnalysisError::Serialize(error.to_string()))?;
    Ok(crate::config::sha256_hex(&bytes))
}

fn input_hash(returns: &[f64], periods_per_year: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update((returns.len() as u64).to_le_bytes());
    hasher.update(u64::from(periods_per_year).to_le_bytes());
    for value in returns {
        hasher.update(value.to_bits().to_le_bytes());
    }
    crate::config::sha256_hex(hasher.finalize().as_slice())
}

fn matrix_input_hash(labels: &[String], returns: &[Vec<f64>], periods_per_year: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update((labels.len() as u64).to_le_bytes());
    hasher.update(u64::from(periods_per_year).to_le_bytes());
    for (label, row) in labels.iter().zip(returns) {
        hasher.update((label.len() as u64).to_le_bytes());
        hasher.update(label.as_bytes());
        hasher.update((row.len() as u64).to_le_bytes());
        for value in row {
            hasher.update(value.to_bits().to_le_bytes());
        }
    }
    crate::config::sha256_hex(hasher.finalize().as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn returns() -> Vec<f64> {
        (0..150)
            .map(|index| ((index as f64) * 0.31).sin() * 0.01 + 0.0002)
            .collect()
    }

    fn analyze(seed: u64) -> AnalysisReport {
        let values = returns();
        analyze_run(&RunAnalysisInput {
            returns: &values,
            periods_per_year: 365,
            seed,
            bootstrap_samples: 200,
        })
        .expect("analysis")
    }

    #[test]
    fn report_hash_is_stable_and_seed_sensitive() {
        let first = analyze(42);
        let second = analyze(42);
        assert_eq!(first, second);
        assert_eq!(first.hash.len(), 64);
        assert_eq!(first.input_hash.len(), 64);
        assert!(first.bootstrap.is_some());
        let third = analyze(43);
        assert_ne!(first.hash, third.hash);
    }

    #[test]
    fn input_hash_ignores_seed_but_covers_returns() {
        let values = returns();
        let base = analyze_run(&RunAnalysisInput {
            returns: &values,
            periods_per_year: 365,
            seed: 1,
            bootstrap_samples: 100,
        })
        .expect("base");
        let other_seed = analyze_run(&RunAnalysisInput {
            returns: &values,
            periods_per_year: 365,
            seed: 2,
            bootstrap_samples: 100,
        })
        .expect("seed");
        assert_eq!(base.input_hash, other_seed.input_hash);
        assert_ne!(base.hash, other_seed.hash);

        let mut changed = values.clone();
        changed[0] += 1e-12;
        let changed = analyze_run(&RunAnalysisInput {
            returns: &changed,
            periods_per_year: 365,
            seed: 1,
            bootstrap_samples: 100,
        })
        .expect("changed");
        assert_ne!(base.input_hash, changed.input_hash);
    }

    #[test]
    fn report_keeps_warnings_and_null_statistics_on_short_input() {
        let report = analyze_run(&RunAnalysisInput {
            returns: &[0.01],
            periods_per_year: 365,
            seed: 7,
            bootstrap_samples: 100,
        })
        .expect("analysis");
        assert!(report.hash.len() == 64);
        let bootstrap = report.bootstrap.expect("bootstrap");
        assert!(bootstrap.statistics.iter().all(|stat| stat.point.is_none()));
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("长度 1"))
        );
    }

    #[test]
    fn report_hash_does_not_depend_on_thread_context() {
        let values: Vec<f64> = returns();
        let reports: Vec<AnalysisReport> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        analyze_run(&RunAnalysisInput {
                            returns: &values,
                            periods_per_year: 365,
                            seed: 42,
                            bootstrap_samples: 200,
                        })
                        .expect("analysis")
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("join"))
                .collect()
        });
        for report in &reports[1..] {
            assert_eq!(reports[0], *report);
        }
    }

    fn trial_matrix(trials: usize, observations: usize) -> Vec<Vec<f64>> {
        let mut state = 17u64;
        (0..trials)
            .map(|_| {
                (0..observations)
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
    fn sweep_analysis_drops_bad_rows_and_reports_pbo() {
        let labels: Vec<String> = (0..20).map(|index| format!("trial-{index}")).collect();
        let mut returns = trial_matrix(20, 240);
        let mut short = returns[3].clone();
        short.pop();
        returns[3] = short;
        returns[4][10] = f64::NAN;
        let report = analyze_sweep(&SweepAnalysisInput {
            labels: &labels,
            returns: &returns,
            periods_per_year: 365,
            seed: 42,
            pbo_blocks: 10,
            min_trials: 20,
        })
        .expect("analysis");
        assert!(report.bootstrap.is_none());
        let pbo = report.pbo.as_ref().expect("pbo");
        assert_eq!(pbo.trials, 18);
        assert_eq!(pbo.splits, 252);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("长度"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("非有限"))
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("min_trials"))
        );

        let repeated = analyze_sweep(&SweepAnalysisInput {
            labels: &labels,
            returns: &returns,
            periods_per_year: 365,
            seed: 42,
            pbo_blocks: 10,
            min_trials: 20,
        })
        .expect("repeat");
        assert_eq!(report, repeated);
    }

    #[test]
    fn sweep_analysis_skips_pbo_when_blocks_exceed_observations() {
        let labels: Vec<String> = (0..3).map(|index| format!("trial-{index}")).collect();
        let returns = trial_matrix(3, 4);
        let report = analyze_sweep(&SweepAnalysisInput {
            labels: &labels,
            returns: &returns,
            periods_per_year: 365,
            seed: 1,
            pbo_blocks: 10,
            min_trials: 20,
        })
        .expect("analysis");
        assert!(report.pbo.is_none());
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("跳过 PBO"))
        );
    }
}
