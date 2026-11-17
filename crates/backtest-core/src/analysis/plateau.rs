//! 参数平台(plateau)得分。
//!
//! 输入是二维目标值矩阵,算法对每个格点计算其四邻(上下左右)中有多少个
//! 目标值不低于 `当前值 - tolerance`,得分越高说明该格点越处于平坦的高值
//! 区域,而不是孤立尖峰。

use serde::{Deserialize, Serialize};

use crate::error::AnalysisError;

/// 本模块的错误类型;调用方通过 `Error::Analysis` 转换。
type Result<T> = std::result::Result<T, AnalysisError>;

/// 网格点坐标与目标值。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridPoint {
    pub x: usize,
    pub y: usize,
    pub value: f64,
}

/// 平台分析结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlateauReport {
    pub best: GridPoint,
    pub robust: Option<GridPoint>,
    pub isolated: bool,
    /// 每个格点的平台得分,行优先;无效格点为 `None`。
    pub scores: Vec<Option<f64>>,
    pub warnings: Vec<String>,
}

/// 平台输入。
#[derive(Clone, Debug)]
pub struct PlateauInput {
    /// `values[y][x]`,允许缺失/非有限。
    pub values: Vec<Vec<Option<f64>>>,
    pub tolerance: f64,
}

#[derive(Clone, Debug)]
pub struct PlateauComputation {
    pub report: PlateauReport,
    pub warnings: Vec<String>,
}

/// 计算平台得分。
pub fn plateau_score(input: &PlateauInput) -> Result<PlateauComputation> {
    if !input.tolerance.is_finite() || input.tolerance <= 0.0 {
        return Err(AnalysisError::Invalid(
            "plateau tolerance 必须是正有限数".into(),
        ));
    }
    let rows = input.values.len();
    if rows == 0 {
        return Err(AnalysisError::Invalid("平台矩阵为空".into()));
    }
    let cols = input.values[0].len();
    if cols == 0 {
        return Err(AnalysisError::Invalid("平台矩阵列数为 0".into()));
    }
    if input.values.iter().any(|row| row.len() != cols) {
        return Err(AnalysisError::Invalid("平台矩阵行长度不一致".into()));
    }

    let mut warnings = Vec::new();
    let mut scores = vec![None; rows * cols];
    let mut best: Option<(usize, usize, f64)> = None;
    let mut robust_candidate: Option<(usize, usize, f64)> = None;

    for y in 0..rows {
        for x in 0..cols {
            let Some(value) = input.values[y][x].filter(|v| v.is_finite()) else {
                continue;
            };
            // 邻居
            let mut neighbors = Vec::new();
            if y > 0 {
                neighbors.push((x, y - 1));
            }
            if y + 1 < rows {
                neighbors.push((x, y + 1));
            }
            if x > 0 {
                neighbors.push((x - 1, y));
            }
            if x + 1 < cols {
                neighbors.push((x + 1, y));
            }

            let mut count = 0usize;
            let mut total = 0usize;
            for (nx, ny) in &neighbors {
                if let Some(nv) = input.values[*ny][*nx].filter(|v| v.is_finite()) {
                    total += 1;
                    if nv >= value - input.tolerance {
                        count += 1;
                    }
                }
            }
            let score = if total == 0 {
                1.0
            } else {
                count as f64 / total as f64
            };
            scores[y * cols + x] = Some(score);

            // best: max value
            match best {
                None => best = Some((x, y, value)),
                Some((_, _, bv)) if value > bv => best = Some((x, y, value)),
                _ => {}
            }

            // robust: score >= 0.75, max value
            if score >= 0.75 {
                match robust_candidate {
                    None => robust_candidate = Some((x, y, value)),
                    Some((_, _, rv)) if value > rv => robust_candidate = Some((x, y, value)),
                    _ => {}
                }
            }
        }
    }

    let Some((bx, by, bv)) = best else {
        return Err(AnalysisError::Invalid("平台矩阵没有有效格点".into()));
    };
    let best_point = GridPoint {
        x: bx,
        y: by,
        value: bv,
    };
    let best_score = scores[by * cols + bx].unwrap_or(0.0);
    let isolated = best_score < 0.5;
    if isolated {
        warnings.push("best 为孤立尖峰:平台得分低于 0.5".into());
    }
    if robust_candidate.is_none() {
        warnings.push("没有平台得分 >=0.75 的稳健点".into());
    }

    let robust = robust_candidate.map(|(x, y, value)| GridPoint { x, y, value });

    Ok(PlateauComputation {
        report: PlateauReport {
            best: best_point,
            robust,
            isolated,
            scores,
            warnings: warnings.clone(),
        },
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_high_plateau_scores_one() {
        // 3x3 全部相同目标值
        let values = vec![vec![Some(1.0); 3]; 3];
        let comp = plateau_score(&PlateauInput {
            values,
            tolerance: 0.1,
        })
        .expect("plateau");
        assert!(!comp.report.isolated);
        assert!(comp.report.robust.is_some());
        for s in &comp.report.scores {
            assert_eq!(*s, Some(1.0));
        }
    }

    #[test]
    fn isolated_peak_is_flagged() {
        let mut values = vec![vec![Some(0.0); 3]; 3];
        values[1][1] = Some(10.0);
        let comp = plateau_score(&PlateauInput {
            values,
            tolerance: 0.1,
        })
        .expect("plateau");
        assert!(comp.report.isolated);
        assert_eq!(comp.report.best.x, 1);
        assert_eq!(comp.report.best.y, 1);
    }

    #[test]
    fn robust_picks_highest_among_flat() {
        let values = vec![
            vec![Some(0.0), Some(0.5), Some(0.5)],
            vec![Some(0.0), Some(0.5), Some(0.5)],
            vec![Some(0.0), Some(0.0), Some(0.0)],
        ];
        let comp = plateau_score(&PlateauInput {
            values,
            tolerance: 0.1,
        })
        .expect("plateau");
        assert!(!comp.report.isolated);
        let robust = comp.report.robust.expect("robust");
        assert_eq!(robust.value, 0.5);
    }

    #[test]
    fn rejects_invalid_tolerance() {
        let values = vec![vec![Some(1.0)]];
        assert!(matches!(
            plateau_score(&PlateauInput {
                values,
                tolerance: 0.0,
            }),
            Err(AnalysisError::Invalid(_))
        ));
    }
}
