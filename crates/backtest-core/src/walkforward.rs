//! Walk-forward 窗口规划、OOS 拼接与参数稳定性。
//!
//! 这里只放纯函数;窗口内选参与回测编排位于 `backtest-cli`。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{WalkForwardConfig, WindowMode};
use crate::error::{ConfigError, Result};
use crate::model::{DeflatedSharpe, EquityPoint, Metrics, Ts};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRange {
    pub index: usize,
    pub train_start: usize,
    pub train_end: usize,
    pub test_start: usize,
    pub test_end: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WindowPlan {
    pub windows: Vec<WindowRange>,
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalkForwardWindow {
    pub index: usize,
    pub train_start: usize,
    pub train_end: usize,
    pub test_start: usize,
    pub test_end: usize,
    pub train_start_ts: Ts,
    pub train_end_ts: Ts,
    pub test_start_ts: Ts,
    pub test_end_ts: Ts,
    pub best_params: BTreeMap<String, serde_json::Value>,
    pub is_metrics: Metrics,
    pub oos_metrics: Metrics,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParamSeries {
    pub name: String,
    pub values: Vec<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkForwardGap {
    pub after_window: usize,
    pub start_index: usize,
    pub end_index: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalkForwardResult {
    pub mode: WindowMode,
    pub train_bars: usize,
    pub test_bars: usize,
    pub step_bars: usize,
    pub embargo_bars: usize,
    pub warmup_bars: usize,
    pub objective: String,
    pub windows: Vec<WalkForwardWindow>,
    pub oos_equity: Vec<EquityPoint>,
    pub oos_metrics: Metrics,
    pub param_stability: Vec<ParamSeries>,
    pub skipped_windows: Vec<String>,
    pub gaps: Vec<WalkForwardGap>,
    pub dsr: Option<DeflatedSharpe>,
    pub elapsed_ms: u128,
}

/// 按契约切分窗口。
pub fn plan_windows(total_bars: usize, config: &WalkForwardConfig) -> Result<WindowPlan> {
    if config.test_bars == 0 || config.train_bars == 0 {
        return Err(ConfigError::Invalid {
            field: "walk_forward".into(),
            message: "train_bars 与 test_bars 必须大于 0".into(),
        }
        .into());
    }
    if config.step_bars < config.test_bars {
        return Err(ConfigError::Invalid {
            field: "walk_forward.step_bars".into(),
            message: "必须大于等于 test_bars;重叠窗口不允许".into(),
        }
        .into());
    }
    let mut plan = WindowPlan::default();
    let mut index = 0usize;
    loop {
        let (train_start, train_end) = match config.mode {
            WindowMode::Rolling => (
                index * config.step_bars,
                index * config.step_bars + config.train_bars,
            ),
            WindowMode::Anchored => (0, config.train_bars + index * config.step_bars),
        };
        let test_start = train_end + config.embargo_bars;
        let test_end = test_start + config.test_bars;
        if test_start >= total_bars {
            break;
        }
        if test_end > total_bars {
            plan.skipped.push(format!(
                "窗口 {index} 测试区间 [{test_start}, {test_end}) 超出数据长度 {total_bars},已丢弃"
            ));
            break;
        }
        plan.windows.push(WindowRange {
            index,
            train_start,
            train_end,
            test_start,
            test_end,
        });
        index += 1;
    }
    if plan.windows.is_empty() {
        return Err(ConfigError::Invalid {
            field: "walk_forward".into(),
            message: format!(
                "数据仅 {total_bars} 根 bar,不足以构成一个完整窗口(至少需要 {} 根)",
                config.train_bars + config.embargo_bars + config.test_bars
            ),
        }
        .into());
    }
    Ok(plan)
}

/// 窗口之间因 `step_bars > test_bars` 产生的空档。
pub fn plan_gaps(windows: &[WindowRange]) -> Vec<WalkForwardGap> {
    let mut gaps = Vec::new();
    for pair in windows.windows(2) {
        if pair[1].test_start > pair[0].test_end {
            gaps.push(WalkForwardGap {
                after_window: pair[0].index,
                start_index: pair[0].test_end,
                end_index: pair[1].test_start,
            });
        }
    }
    gaps
}

/// 把各窗口的测试段按时间复利拼接。
///
/// 每个窗口的第一根 bar 相对该窗口 `initial_cash` 的收益会被保留,
/// 因此 OOS 曲线从 `initial_cash` 起算,窗口之间不重置资金基数。
pub fn stitch_oos(initial_cash: f64, segments: &[(&[EquityPoint], f64)]) -> Vec<EquityPoint> {
    let mut level = initial_cash;
    let mut peak = initial_cash;
    let mut out = Vec::new();
    for (equity, segment_cash) in segments {
        let mut previous = *segment_cash;
        for point in *equity {
            if previous > 0.0 {
                level *= point.equity / previous;
            }
            previous = point.equity;
            if level > peak {
                peak = level;
            }
            let drawdown = if peak > 0.0 {
                (level / peak - 1.0).min(0.0)
            } else {
                0.0
            };
            // 窗口内权益已按复利缩放到拼接后的资金基数;现金与名义敞口
            // 同步缩放,保留原窗口的杠杆与持仓比例。
            let scale = if point.equity.abs() > 0.0 {
                level / point.equity
            } else {
                1.0
            };
            out.push(EquityPoint {
                ts: point.ts,
                equity: level,
                cash: point.cash * scale,
                gross_exposure: point.gross_exposure * scale,
                drawdown,
            });
        }
    }
    out
}

pub fn param_stability(windows: &[WalkForwardWindow]) -> Vec<ParamSeries> {
    let mut names: Vec<String> = windows
        .iter()
        .flat_map(|window| window.best_params.keys().cloned())
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| ParamSeries {
            values: windows
                .iter()
                .map(|window| {
                    window
                        .best_params
                        .get(&name)
                        .cloned()
                        .unwrap_or(serde_json::Value::Null)
                })
                .collect(),
            name,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Objective, WalkForwardConfig};

    fn config(mode: WindowMode) -> WalkForwardConfig {
        WalkForwardConfig {
            mode,
            train_bars: 756,
            test_bars: 252,
            step_bars: 252,
            embargo_bars: 1,
            warmup_bars: 60,
            objective: Objective::Sharpe,
        }
    }

    #[test]
    fn t5_9_window_boundaries_follow_contract() {
        let plan = plan_windows(4033, &config(WindowMode::Rolling)).expect("plan");
        assert_eq!(plan.windows[0].train_start, 0);
        assert_eq!(plan.windows[0].train_end, 756);
        assert_eq!(plan.windows[0].test_start, 757);
        assert_eq!(plan.windows[0].test_end, 1009);
        assert_eq!(plan.windows[1].train_start, 252);
        assert_eq!(plan.windows[1].train_end, 1008);
        assert_eq!(plan.windows[1].test_start, 1009);
        assert!(plan.windows[0].test_end <= plan.windows[1].test_start);
        assert!(plan.windows.iter().all(|w| w.train_end <= w.test_start));
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn anchored_training_window_only_grows() {
        let plan = plan_windows(4000, &config(WindowMode::Anchored)).expect("plan");
        assert_eq!(plan.windows[0].train_start, 0);
        assert_eq!(plan.windows[0].train_end, 756);
        assert_eq!(plan.windows[1].train_start, 0);
        assert_eq!(plan.windows[1].train_end, 1008);
        assert!(plan.windows[1].train_end > plan.windows[0].train_end);
    }

    #[test]
    fn overlap_is_rejected_and_partial_tail_is_skipped() {
        let mut cfg = config(WindowMode::Rolling);
        cfg.step_bars = 100;
        assert!(plan_windows(4000, &cfg).is_err());

        cfg.step_bars = 252;
        let plan = plan_windows(1100, &cfg).expect("plan");
        assert_eq!(plan.windows.len(), 1);
        assert_eq!(plan.skipped.len(), 1);
    }

    #[test]
    fn gaps_are_reported_when_step_exceeds_test() {
        let mut cfg = config(WindowMode::Rolling);
        cfg.step_bars = 300;
        let plan = plan_windows(4000, &cfg).expect("plan");
        let gaps = plan_gaps(&plan.windows);
        assert!(!gaps.is_empty());
        assert_eq!(gaps[0].start_index, plan.windows[0].test_end);
    }

    #[test]
    fn t5_11_oos_stitching_compounds_across_windows() {
        let day = 86_400_000;
        let segment = |start: i64, values: &[f64]| {
            values
                .iter()
                .enumerate()
                .map(|(index, value)| EquityPoint {
                    ts: start + index as i64 * day,
                    equity: *value,
                    cash: *value,
                    gross_exposure: 0.0,
                    drawdown: 0.0,
                })
                .collect::<Vec<_>>()
        };
        let first = segment(0, &[100.0, 110.0]);
        let second = segment(2, &[100.0, 90.0]);
        let stitched = stitch_oos(100.0, &[(&first, 100.0), (&second, 100.0)]);
        assert_eq!(stitched.len(), 4);
        assert!((stitched[0].equity - 100.0).abs() < 1e-12);
        assert!((stitched[1].equity - 110.0).abs() < 1e-12);
        assert!((stitched[2].equity - 110.0).abs() < 1e-12);
        assert!((stitched[3].equity - 99.0).abs() < 1e-12);
        assert!((stitched[3].drawdown + 0.1).abs() < 1e-12);
    }

    #[test]
    fn t5_10_oos_stitching_keeps_exposure_and_cash_ratio() {
        let day = 86_400_000;
        let point = |ts: i64, equity: f64, cash: f64, gross: f64| EquityPoint {
            ts,
            equity,
            cash,
            gross_exposure: gross,
            drawdown: 0.0,
        };
        let first = vec![point(0, 100.0, 100.0, 0.0), point(day, 110.0, 10.0, 100.0)];
        let second = vec![
            point(2 * day, 100.0, 50.0, 200.0),
            point(3 * day, 90.0, 45.0, 180.0),
        ];
        let stitched = stitch_oos(100.0, &[(&first, 100.0), (&second, 100.0)]);
        assert!((stitched[1].gross_exposure - 100.0).abs() < 1e-12);
        assert!((stitched[2].equity - 110.0).abs() < 1e-12);
        // 第二个窗口从 110 的基数继续,原权益 100 被放大 1.1 倍。
        assert!((stitched[2].cash - 55.0).abs() < 1e-12);
        assert!((stitched[2].gross_exposure - 220.0).abs() < 1e-12);
        assert!((stitched[3].cash - 49.5).abs() < 1e-12);
    }

    #[test]
    fn param_stability_keeps_window_order() {
        let window = |index: usize, fast: f64| WalkForwardWindow {
            index,
            train_start: 0,
            train_end: 10,
            test_start: 11,
            test_end: 20,
            train_start_ts: 0,
            train_end_ts: 1,
            test_start_ts: 2,
            test_end_ts: 3,
            best_params: BTreeMap::from([("fast".into(), serde_json::json!(fast))]),
            is_metrics: Metrics::empty(252, 0.0),
            oos_metrics: Metrics::empty(252, 0.0),
            warnings: Vec::new(),
        };
        let series = param_stability(&[window(0, 5.0), window(1, 10.0), window(2, 5.0)]);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].name, "fast");
        assert_eq!(series[0].values[0], serde_json::json!(5.0));
        assert_eq!(series[0].values[1], serde_json::json!(10.0));
    }
}
