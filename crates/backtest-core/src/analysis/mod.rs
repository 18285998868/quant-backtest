//! 统计分析与稳健性检验内核。
//!
//! 本模块只处理收益序列与统计量,不读取配置、不依赖引擎或报告。编排层负责
//! 把权益曲线聚合成收益、把结果放进报告。
//!
//! 关闭 `[analysis]` 时不会调用这里的任何函数,因此不改变历史哈希。

mod bootstrap;
mod rng;

pub use bootstrap::{
    BootstrapComputation, BootstrapInput, BootstrapReport, BootstrapStat, BootstrapStatistic,
    DEFAULT_CONFIDENCE, stationary_bootstrap,
};
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
        warnings: computation.warnings,
    };
    finalize(report)
}

fn finalize(mut report: AnalysisReport) -> Result<AnalysisReport> {
    report.hash = content_hash(&report)?;
    Ok(report)
}

fn content_hash(report: &AnalysisReport) -> Result<String> {
    let payload = serde_json::json!({
        "version": report.version,
        "seed": report.seed,
        "input_hash": report.input_hash,
        "bootstrap": &report.bootstrap,
        "warnings": &report.warnings,
    });
    let bytes = serde_json::to_vec(&payload)
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
}
