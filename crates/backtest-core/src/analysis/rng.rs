//! 确定性随机源。
//!
//! 使用 SplitMix64:状态只有 64 位,不引入新依赖,跨平台结果一致。它不是
//! 密码学随机数,只用于 bootstrap 重采样。

use sha2::{Digest, Sha256};

/// SplitMix64 状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
    const MIX1: u64 = 0xBF58_476D_1CE4_E5B9;
    const MIX2: u64 = 0x94D0_49BB_1331_11EB;

    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// 返回下一个 64 位输出。
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(Self::GAMMA);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(Self::MIX1);
        value = (value ^ (value >> 27)).wrapping_mul(Self::MIX2);
        value ^ (value >> 31)
    }

    /// 返回 `[0, 1)` 的浮点数,取高 53 位。
    pub fn next_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / 9_007_199_254_740_992.0;
        ((self.next_u64() >> 11) as f64) * SCALE
    }

    /// 无模偏差地返回 `[0, n)`。
    ///
    /// `n` 必须大于 0。
    pub fn below(&mut self, n: usize) -> usize {
        assert!(n > 0, "below(0) 没有定义");
        let n = n as u64;
        // 2^64 % n:落在阈值以下的输出会放大余数频率,直接拒绝。
        let threshold = n.wrapping_neg() % n;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return (value % n) as usize;
            }
        }
    }
}

/// 从主种子派生稳定的子种子。
///
/// `seed = first_u64_be(SHA256(master_le || domain || index_le))`。域字符串
/// 放在固定的 8 字节主种子与 8 字节下标之间,长度由总输入长度唯一确定。
pub fn seed_for(master: u64, domain: &str, index: u64) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(master.to_le_bytes());
    hasher.update(domain.as_bytes());
    hasher.update(index.to_le_bytes());
    let digest = hasher.finalize();
    let mut seed = 0u64;
    for byte in &digest[..8] {
        seed = (seed << 8) | u64::from(*byte);
    }
    seed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix64_matches_reference_vectors() {
        let mut rng = SplitMix64::new(0);
        let expected = [
            0xE220_A839_7B1D_CDAF,
            0x6E78_9E6A_A1B9_65F4,
            0x06C4_5D18_8009_454F,
            0xF88B_B8A8_724C_81EC,
            0x1B39_896A_51A8_749B,
            0x53CB_9F0C_747E_A2EA,
        ];
        for value in expected {
            assert_eq!(rng.next_u64(), value);
        }
    }

    #[test]
    fn next_f64_stays_in_unit_interval() {
        let mut rng = SplitMix64::new(42);
        for _ in 0..10_000 {
            let value = rng.next_f64();
            assert!((0.0..1.0).contains(&value), "越界: {value}");
        }
    }

    #[test]
    fn below_is_in_range_and_roughly_uniform() {
        let mut rng = SplitMix64::new(7);
        let mut counts = [0usize; 7];
        for _ in 0..7_000 {
            let index = rng.below(7);
            assert!(index < 7);
            counts[index] += 1;
        }
        for count in counts {
            assert!((700..=1_300).contains(&count), "分布偏差过大: {counts:?}");
        }
    }

    #[test]
    #[should_panic(expected = "below(0)")]
    fn below_zero_panics() {
        SplitMix64::new(1).below(0);
    }

    #[test]
    fn seed_derivation_is_stable_and_separated() {
        assert_eq!(seed_for(42, "bootstrap", 0), seed_for(42, "bootstrap", 0));
        assert_ne!(seed_for(42, "bootstrap", 0), seed_for(42, "bootstrap", 1));
        assert_ne!(seed_for(42, "bootstrap", 0), seed_for(43, "bootstrap", 0));
        assert_ne!(seed_for(42, "pbo", 0), seed_for(42, "spa", 0));
    }
}
