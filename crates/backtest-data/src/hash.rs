//! 内容哈希 v2:按固定 bar 页做 Merkle 汇总,支持时间分块流式计算。
//!
//! 旧版 v1 按“符号 → 行数 → 逐行”平铺哈希,必须先知道行数且按符号顺序
//! 一次性喂入,无法在时间分块时保持同一结果。v2 改成:
//!
//! ```text
//! data/v2, symbol_count,
//! 每个符号:名称、总行数、每页行数与页摘要(SHA-256(该页行字节))
//! 资金费 v2:每个符号名称、行数、逐行结算
//! ```
//!
//! 页大小固定为 [`PAGE_BARS`] 根 bar,与运行时窗口大小无关。

use sha2::{Digest, Sha256};

use backtest_core::model::{FundingSeries, InstrumentSpec, Ts};

/// 固定页大小。修改它会改变 `data_hash`,必须同步升级哈希版本号。
pub(crate) const PAGE_BARS: usize = 65_536;

const DATA_TAG: &[u8] = b"quant-backtest/data/v2";
const FUNDING_TAG: &[u8] = b"quant-backtest/funding/v2";

struct SymbolHash {
    rows: u64,
    page: Sha256,
    page_rows: usize,
    pages: Vec<(u64, [u8; 32])>,
}

impl SymbolHash {
    fn new() -> Self {
        Self {
            rows: 0,
            page: Sha256::new(),
            page_rows: 0,
            pages: Vec::new(),
        }
    }

    fn push(&mut self, ts: Ts, open: f64, high: f64, low: f64, close: f64, volume: f64) {
        self.page.update(ts.to_le_bytes());
        self.page.update(open.to_le_bytes());
        self.page.update(high.to_le_bytes());
        self.page.update(low.to_le_bytes());
        self.page.update(close.to_le_bytes());
        self.page.update(volume.to_le_bytes());
        self.rows += 1;
        self.page_rows += 1;
        if self.page_rows == PAGE_BARS {
            self.flush_page();
        }
    }

    fn flush_page(&mut self) {
        if self.page_rows == 0 {
            return;
        }
        let digest = std::mem::take(&mut self.page).finalize();
        self.pages.push((self.page_rows as u64, digest.into()));
        self.page = Sha256::new();
        self.page_rows = 0;
    }
}

/// 流式数据哈希器。行可以按符号分批喂入,页摘要与排序无关。
pub(crate) struct DataHasher {
    symbols: Vec<SymbolHash>,
}

impl DataHasher {
    pub(crate) fn new(symbol_count: usize) -> Self {
        Self {
            symbols: (0..symbol_count).map(|_| SymbolHash::new()).collect(),
        }
    }

    pub(crate) fn push(&mut self, symbol: usize, ts: Ts, ohlcv: [f64; 5]) {
        if let Some(state) = self.symbols.get_mut(symbol) {
            state.push(ts, ohlcv[0], ohlcv[1], ohlcv[2], ohlcv[3], ohlcv[4]);
        }
    }

    pub(crate) fn finish(mut self, specs: &[InstrumentSpec], funding: &[FundingSeries]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(DATA_TAG);
        hasher.update((specs.len() as u64).to_le_bytes());
        for (symbol, spec) in specs.iter().enumerate() {
            let state = &mut self.symbols[symbol];
            state.flush_page();
            hasher.update((spec.symbol.len() as u64).to_le_bytes());
            hasher.update(spec.symbol.as_bytes());
            hasher.update(state.rows.to_le_bytes());
            for (rows, digest) in &state.pages {
                hasher.update(rows.to_le_bytes());
                hasher.update(digest);
            }
        }
        if !funding.is_empty() {
            hasher.update(FUNDING_TAG);
            hasher.update((funding.len() as u64).to_le_bytes());
            for (series, spec) in funding.iter().zip(specs) {
                hasher.update((spec.symbol.len() as u64).to_le_bytes());
                hasher.update(spec.symbol.as_bytes());
                hasher.update((series.ts.len() as u64).to_le_bytes());
                for (ts, rate) in series.ts.iter().zip(&series.rate) {
                    hasher.update(ts.to_le_bytes());
                    hasher.update(rate.to_le_bytes());
                }
            }
        }
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::model::AssetClass;

    fn spec(symbol: &str) -> InstrumentSpec {
        InstrumentSpec {
            symbol: symbol.into(),
            asset_class: AssetClass::Crypto,
            currency: "USD".into(),
            multiplier: 1.0,
            lot_size: 0.0,
            margin_rate: 1.0,
            fee_bps: 0.0,
            fee_fixed: 0.0,
            slippage_bps: 0.0,
            periods_per_year: Some(365),
        }
    }

    #[test]
    fn hash_is_independent_of_symbol_feed_order() {
        let specs = [spec("AAA"), spec("BBB")];
        let mut first = DataHasher::new(2);
        first.push(0, 1, [10.0, 11.0, 9.0, 10.5, 100.0]);
        first.push(1, 1, [20.0, 21.0, 19.0, 20.5, 200.0]);
        first.push(0, 2, [10.5, 12.0, 10.0, 11.5, 110.0]);
        first.push(1, 2, [20.5, 22.0, 20.0, 21.5, 210.0]);

        let mut second = DataHasher::new(2);
        second.push(1, 1, [20.0, 21.0, 19.0, 20.5, 200.0]);
        second.push(1, 2, [20.5, 22.0, 20.0, 21.5, 210.0]);
        second.push(0, 1, [10.0, 11.0, 9.0, 10.5, 100.0]);
        second.push(0, 2, [10.5, 12.0, 10.0, 11.5, 110.0]);

        assert_eq!(first.finish(&specs, &[]), second.finish(&specs, &[]));
    }

    #[test]
    fn hash_covers_rows_past_first_page() {
        let specs = [spec("AAA")];
        let rows = PAGE_BARS + 7;
        let bar = |index: usize, marker: f64| {
            let mut values = [1.0, 2.0, 0.5, 1.5, marker];
            values[0] += index as f64 * 1e-9;
            values
        };
        let mut hasher = DataHasher::new(1);
        for index in 0..rows {
            hasher.push(0, index as Ts, bar(index, 1.0));
        }
        let first = hasher.finish(&specs, &[]);

        // 修改第一页之后的行必须改变摘要,页摘要覆盖所有页。
        let mut changed = DataHasher::new(1);
        for index in 0..rows {
            let marker = if index + 1 == rows { 2.0 } else { 1.0 };
            changed.push(0, index as Ts, bar(index, marker));
        }
        assert_ne!(first, changed.finish(&specs, &[]));
    }
}
