//! 策略契约。具体策略实现放在 `backtest-strategy`。

use crate::model::{SymbolId, Ts, Universe};

/// 逆波动率回退时使用的默认波动率下限。
pub const DEFAULT_VOL_FLOOR: f64 = 1e-4;

#[derive(Clone, Copy, Debug)]
pub struct TargetPosition {
    pub symbol: SymbolId,
    pub weight: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct PositionView {
    pub symbol: SymbolId,
    pub qty: f64,
    pub avg_entry: f64,
    pub mark: f64,
    pub stop: Option<f64>,
    pub take_profit: Option<f64>,
}

#[derive(Debug)]
pub struct MarketContext<'a> {
    pub ts: Ts,
    pub index: usize,
    pub universe: &'a Universe,
    pub cash: f64,
    pub equity: f64,
    pub positions: &'a [PositionView],
}

pub trait Strategy: Send {
    fn on_bar(&mut self, ctx: &MarketContext<'_>, out: &mut Vec<TargetPosition>);

    /// 策略声明的最长指标窗口。walk-forward 预热与配置校验会使用。
    fn warmup_bars(&self) -> usize {
        0
    }

    /// 取走策略在本根 bar 产生的告警。引擎会追加到结果 `warnings`。
    fn take_warnings(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// 逆波动率回退原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeightFallback {
    TooFewActive,
    TooFewValidVol,
    ZeroRaw,
}

impl WeightFallback {
    pub fn message(self) -> &'static str {
        match self {
            WeightFallback::TooFewActive => "有效标的少于 2 个",
            WeightFallback::TooFewValidVol => "存在无效波动率",
            WeightFallback::ZeroRaw => "逆波动率分母为 0",
        }
    }
}

#[derive(Clone, Debug)]
pub struct WeightResult {
    pub weights: Vec<f64>,
    pub fallback: Option<WeightFallback>,
}

/// 按方向与逆波动率分配目标权重。
///
/// `vols` 与 `directions` 长度应一致;方向为 0 的标的不参与分配。
/// σ 非有限或 <= 0 视为无效。有效标的少于 2 个、存在无效 σ 或分母为 0 时
/// 回退到按方向等权,并通过 [`WeightResult::fallback`] 返回原因。
pub fn inverse_vol_weights_checked(
    vols: &[f64],
    directions: &[i8],
    gross: f64,
    floor: f64,
) -> WeightResult {
    let n = vols.len().min(directions.len());
    let mut weights = vec![0.0; n];
    let gross = if gross.is_finite() { gross } else { 0.0 };
    let floor = if floor.is_finite() && floor > 0.0 {
        floor
    } else {
        DEFAULT_VOL_FLOOR
    };
    let active: Vec<usize> = (0..n).filter(|&index| directions[index] != 0).collect();
    if active.is_empty() {
        return WeightResult {
            weights,
            fallback: Some(WeightFallback::TooFewActive),
        };
    }

    let mut raw = vec![0.0; n];
    let mut valid = 0;
    for &index in &active {
        let vol = vols[index];
        if vol.is_finite() && vol > 0.0 {
            raw[index] = 1.0 / vol.max(floor);
            valid += 1;
        }
    }
    let raw_sum: f64 = active.iter().map(|&index| raw[index]).sum();
    let fallback = if active.len() < 2 {
        Some(WeightFallback::TooFewActive)
    } else if valid < active.len() {
        Some(WeightFallback::TooFewValidVol)
    } else if !raw_sum.is_finite() || raw_sum <= 0.0 {
        Some(WeightFallback::ZeroRaw)
    } else {
        None
    };

    if let Some(reason) = fallback {
        let equal = gross / active.len() as f64;
        for &index in &active {
            weights[index] = directions[index] as f64 * equal;
        }
        return WeightResult {
            weights,
            fallback: Some(reason),
        };
    }

    for &index in &active {
        weights[index] = directions[index] as f64 * raw[index] / raw_sum * gross;
    }
    WeightResult {
        weights,
        fallback: None,
    }
}

/// 逆波动率权重;无法计算时静默回退等权。
///
/// 需要回退原因时使用 [`inverse_vol_weights_checked`]。
pub fn inverse_vol_weights(vols: &[f64], directions: &[i8], gross: f64, floor: f64) -> Vec<f64> {
    inverse_vol_weights_checked(vols, directions, gross, floor).weights
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "actual={actual}, expected={expected}"
        );
    }

    #[test]
    fn inverse_vol_weights_match_hand_calculation() {
        let weights = inverse_vol_weights(&[0.01, 0.02], &[1, 1], 1.0, DEFAULT_VOL_FLOOR);
        assert_close(weights[0], 2.0 / 3.0);
        assert_close(weights[1], 1.0 / 3.0);
        let gross: f64 = weights.iter().map(|weight| weight.abs()).sum();
        assert_close(gross, 1.0);
    }

    #[test]
    fn inverse_vol_weights_respect_short_direction() {
        let weights = inverse_vol_weights(&[0.01, 0.02], &[1, -1], 1.0, DEFAULT_VOL_FLOOR);
        assert_close(weights[0], 2.0 / 3.0);
        assert_close(weights[1], -1.0 / 3.0);
        let gross: f64 = weights.iter().map(|weight| weight.abs()).sum();
        assert_close(gross, 1.0);
    }

    #[test]
    fn invalid_vol_falls_back_to_equal_weights() {
        let result = inverse_vol_weights_checked(&[0.0, 0.02], &[1, 1], 1.0, DEFAULT_VOL_FLOOR);
        assert_close(result.weights[0], 0.5);
        assert_close(result.weights[1], 0.5);
        assert_eq!(result.fallback, Some(WeightFallback::TooFewValidVol));
    }

    #[test]
    fn single_active_symbol_falls_back_with_warning_reason() {
        let result = inverse_vol_weights_checked(&[0.01], &[1], 1.0, DEFAULT_VOL_FLOOR);
        assert_close(result.weights[0], 1.0);
        assert_eq!(result.fallback, Some(WeightFallback::TooFewActive));
    }

    #[test]
    fn zero_direction_does_not_consume_weight() {
        let result =
            inverse_vol_weights_checked(&[0.01, 0.02, 0.005], &[1, 1, 0], 1.0, DEFAULT_VOL_FLOOR);
        assert_close(result.weights[0], 2.0 / 3.0);
        assert_close(result.weights[1], 1.0 / 3.0);
        assert_close(result.weights[2], 0.0);
        assert_eq!(result.fallback, None);
    }
}
