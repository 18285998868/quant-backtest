use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

pub type Ts = i64;
pub type SymbolId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeUnit {
    Minute,
    Hour,
    Day,
    Week,
}

impl TimeUnit {
    pub fn millis(self) -> i64 {
        match self {
            TimeUnit::Minute => 60_000,
            TimeUnit::Hour => 3_600_000,
            TimeUnit::Day => 86_400_000,
            TimeUnit::Week => 604_800_000,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            TimeUnit::Minute => "m",
            TimeUnit::Hour => "h",
            TimeUnit::Day => "d",
            TimeUnit::Week => "w",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeframe {
    pub unit: TimeUnit,
    pub count: u32,
}

impl Timeframe {
    pub const ONE_DAY: Timeframe = Timeframe {
        unit: TimeUnit::Day,
        count: 1,
    };

    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let text = raw.trim();
        let (digits, suffix) = text.split_at(text.len().saturating_sub(1));
        if digits.is_empty() || suffix.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ConfigError::Invalid {
                field: "timeframe".into(),
                message: format!("'{raw}' 不是合法周期,示例: 1m, 5m, 1h, 4h, 1d, 1w"),
            });
        }
        let count: u32 = digits.parse().map_err(|_| ConfigError::Invalid {
            field: "timeframe".into(),
            message: format!("'{raw}' 的数量部分超出范围"),
        })?;
        if count == 0 {
            return Err(ConfigError::Invalid {
                field: "timeframe".into(),
                message: format!("'{raw}' 的周期数量必须大于 0"),
            });
        }
        let unit = match suffix {
            "m" => TimeUnit::Minute,
            "h" => TimeUnit::Hour,
            "d" => TimeUnit::Day,
            "w" => TimeUnit::Week,
            _ => {
                return Err(ConfigError::Invalid {
                    field: "timeframe".into(),
                    message: format!("未知周期单位 '{suffix}',支持 m/h/d/w"),
                });
            }
        };
        Ok(Timeframe { unit, count })
    }

    pub fn duration_ms(&self) -> i64 {
        self.count as i64 * self.unit.millis()
    }

    pub fn bars_per_year(&self, asset_class: AssetClass) -> u32 {
        let base = match self.unit {
            TimeUnit::Week => 52.0,
            TimeUnit::Day => match asset_class {
                AssetClass::Crypto => 365.0,
                AssetClass::UsEquity | AssetClass::Fx => 252.0,
            },
            TimeUnit::Hour => match asset_class {
                AssetClass::Crypto => 365.0 * 24.0,
                AssetClass::UsEquity => 252.0 * 6.5,
                AssetClass::Fx => 252.0 * 24.0,
            },
            TimeUnit::Minute => match asset_class {
                AssetClass::Crypto => 365.0 * 24.0 * 60.0,
                AssetClass::UsEquity => 252.0 * 6.5 * 60.0,
                AssetClass::Fx => 252.0 * 24.0 * 60.0,
            },
        };
        ((base / self.count as f64).round() as u32).max(1)
    }
}

impl std::fmt::Display for Timeframe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.count, self.unit.suffix())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetClass {
    Crypto,
    UsEquity,
    Fx,
}

impl AssetClass {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "crypto" => Ok(AssetClass::Crypto),
            "us_equity" | "us-equity" | "equity" | "us_stock" | "stock" => Ok(AssetClass::UsEquity),
            "fx" | "forex" => Ok(AssetClass::Fx),
            other => Err(ConfigError::Unsupported {
                what: "资产类别",
                value: other.to_string(),
                supported: "crypto, us_equity, fx",
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AssetClass::Crypto => "crypto",
            AssetClass::UsEquity => "us_equity",
            AssetClass::Fx => "fx",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstrumentSpec {
    pub symbol: String,
    pub asset_class: AssetClass,
    pub currency: String,
    pub multiplier: f64,
    pub lot_size: f64,
    pub margin_rate: f64,
    pub fee_bps: f64,
    pub fee_fixed: f64,
    pub slippage_bps: f64,
    pub periods_per_year: Option<u32>,
}

impl InstrumentSpec {
    pub fn effective_periods_per_year(&self, timeframe: Timeframe) -> u32 {
        self.periods_per_year
            .unwrap_or_else(|| timeframe.bars_per_year(self.asset_class))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BarView {
    pub symbol: SymbolId,
    pub ts: Ts,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Clone, Debug, Default)]
pub struct BarSeries {
    pub symbol: SymbolId,
    pub ts: Vec<Ts>,
    pub open: Vec<f64>,
    pub high: Vec<f64>,
    pub low: Vec<f64>,
    pub close: Vec<f64>,
    pub volume: Vec<f64>,
}

impl BarSeries {
    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }
}

/// 单个标的的资金费结算序列。
///
/// `ts` 是结算时刻(UTC 毫秒),`rate` 是该次结算的费率。序列按 `ts` 严格
/// 递增;费率带符号,正数表示多头向空头支付。
#[derive(Clone, Debug, Default)]
pub struct FundingSeries {
    pub symbol: SymbolId,
    pub ts: Vec<Ts>,
    pub rate: Vec<f64>,
}

impl FundingSeries {
    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct Universe {
    pub timeframe: Timeframe,
    pub specs: Vec<InstrumentSpec>,
    /// 当前窗口的 OHLCV;分块回放时只含窗口范围,配合 `base_index` 定位。
    pub series: Vec<BarSeries>,
    /// 与 `series` 等长;未启用资金费时为空。
    pub funding: Vec<FundingSeries>,
    pub timeline: Vec<Ts>,
    pub data_hash: String,
    /// `series` 第一根 bar 在完整时间线中的绝对下标。
    pub base_index: usize,
}

impl Universe {
    pub fn symbol_id(&self, symbol: &str) -> Option<SymbolId> {
        self.specs
            .iter()
            .position(|spec| spec.symbol == symbol)
            .map(|index| index as SymbolId)
    }

    /// 受信任索引。调用方必须保证 symbol 与 index 已在 `Universe` 内。
    /// 数据区常驻内存的粗略估算,不含 `Vec`/分配器开销与配置。
    pub fn estimated_data_bytes(&self) -> u64 {
        let timeline = self.timeline.len() as u64 * std::mem::size_of::<Ts>() as u64;
        let series = self
            .series
            .iter()
            .map(|item| item.len() as u64 * 5 * std::mem::size_of::<f64>() as u64)
            .sum::<u64>();
        let funding = self
            .funding
            .iter()
            .map(|item| {
                item.len() as u64 * (std::mem::size_of::<Ts>() + std::mem::size_of::<f64>()) as u64
            })
            .sum::<u64>();
        timeline + series + funding
    }

    pub fn has_funding(&self) -> bool {
        self.funding.iter().any(|item| !item.is_empty())
    }

    pub fn bar(&self, symbol: SymbolId, index: usize) -> BarView {
        let local = index - self.base_index;
        let series = &self.series[symbol as usize];
        BarView {
            symbol,
            ts: series.ts[local],
            open: series.open[local],
            high: series.high[local],
            low: series.low[local],
            close: series.close[local],
            volume: series.volume[local],
        }
    }

    pub fn try_bar(&self, symbol: SymbolId, index: usize) -> Option<BarView> {
        let local = index.checked_sub(self.base_index)?;
        let series = self.series.get(symbol as usize)?;
        Some(BarView {
            symbol,
            ts: *series.ts.get(local)?,
            open: *series.open.get(local)?,
            high: *series.high.get(local)?,
            low: *series.low.get(local)?,
            close: *series.close.get(local)?,
            volume: *series.volume.get(local)?,
        })
    }

    /// 当前窗口覆盖的绝对下标范围 `[start, end)`。
    pub fn window_start(&self) -> usize {
        self.base_index
    }

    pub fn window_end(&self) -> usize {
        self.base_index + self.series.first().map_or(0, BarSeries::len)
    }

    pub fn len(&self) -> usize {
        self.timeline.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timeline.is_empty()
    }

    pub fn symbol_count(&self) -> usize {
        self.specs.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillReason {
    Entry,
    Rebalance,
    StopLoss,
    TakeProfit,
    Close,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquityPoint {
    pub ts: Ts,
    pub equity: f64,
    pub cash: f64,
    pub gross_exposure: f64,
    pub drawdown: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FillRecord {
    pub ts: Ts,
    pub symbol: SymbolId,
    pub side: Side,
    pub qty: f64,
    pub price: f64,
    pub notional: f64,
    pub fee: f64,
    pub slippage: f64,
    pub reason: FillReason,
    pub ambiguous_bar: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TradeRecord {
    pub symbol: SymbolId,
    pub side: Side,
    pub entry_ts: Ts,
    pub exit_ts: Option<Ts>,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub qty: f64,
    pub pnl: f64,
    pub fees: f64,
    pub return_pct: f64,
    pub bars_held: u32,
    pub exit_reason: Option<FillReason>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrawdownEpisode {
    pub peak_ts: Ts,
    pub trough_ts: Ts,
    pub recovery_ts: Option<Ts>,
    pub depth: f64,
    pub duration_days: f64,
    pub recovered: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonthlyReturn {
    pub year: i32,
    pub month: u32,
    pub return_pct: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metrics {
    pub total_return: Option<f64>,
    pub cagr: Option<f64>,
    pub annual_volatility: Option<f64>,
    pub sharpe: Option<f64>,
    pub sortino: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub max_drawdown_duration_days: Option<f64>,
    pub calmar: Option<f64>,
    pub ulcer: Option<f64>,
    pub trades: usize,
    pub win_rate: Option<f64>,
    pub profit_factor: Option<f64>,
    pub expectancy: Option<f64>,
    pub total_fees: f64,
    pub turnover: Option<f64>,
    pub cost_drag: Option<f64>,
    pub exposure: Option<f64>,
    pub periods_per_year: u32,
    pub risk_free_rate: f64,
}

impl Metrics {
    pub fn empty(periods_per_year: u32, risk_free_rate: f64) -> Self {
        Self {
            total_return: None,
            cagr: None,
            annual_volatility: None,
            sharpe: None,
            sortino: None,
            max_drawdown: None,
            max_drawdown_duration_days: None,
            calmar: None,
            ulcer: None,
            trades: 0,
            win_rate: None,
            profit_factor: None,
            expectancy: None,
            total_fees: 0.0,
            turnover: None,
            cost_drag: None,
            exposure: None,
            periods_per_year,
            risk_free_rate,
        }
    }
}

/// 搜参校正后的 Deflated Sharpe Ratio。
///
/// `probability` 即 DSR;其余字段用于在报告中复核输入。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeflatedSharpe {
    pub probability: f64,
    pub trials: usize,
    pub sharpe_per_period: f64,
    pub expected_max_sharpe: f64,
    pub trial_sharpe_variance: f64,
    pub skewness: f64,
    pub kurtosis: f64,
    pub observations: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunMeta {
    pub strategy: String,
    pub params: BTreeMap<String, serde_json::Value>,
    pub symbols: Vec<String>,
    pub requested_start: Option<Ts>,
    pub requested_end: Option<Ts>,
    pub first_bar_ts: Option<Ts>,
    pub last_bar_ts: Option<Ts>,
    pub timeframe: String,
    pub config_hash: String,
    pub data_hash: String,
    pub threads: usize,
    pub walk_forward_window: Option<usize>,
    pub engine_version: String,
    pub generated_at: String,
    pub warnings: Vec<String>,
}

/// 单个标的的资金费净额(正数表示净支出)。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SymbolFunding {
    pub symbol: SymbolId,
    pub cost: f64,
    pub events: usize,
}

/// 资金费汇总。`total_cost` 为正表示账户净支出。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundingSummary {
    pub total_cost: f64,
    pub paid: f64,
    pub received: f64,
    pub events: usize,
    pub by_symbol: Vec<SymbolFunding>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BacktestResult {
    pub meta: RunMeta,
    pub equity: Vec<EquityPoint>,
    pub fills: Vec<FillRecord>,
    pub trades: Vec<TradeRecord>,
    pub metrics: Metrics,
    pub drawdowns: Vec<DrawdownEpisode>,
    pub monthly: Vec<MonthlyReturn>,
    /// 未启用资金费时为 `None`,序列化时省略,保持旧结果哈希不变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding: Option<FundingSummary>,
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t1_8_timeframe_parsing_round_trips() {
        for (raw, unit, count) in [
            ("1d", TimeUnit::Day, 1),
            ("4h", TimeUnit::Hour, 4),
            ("1w", TimeUnit::Week, 1),
            ("5m", TimeUnit::Minute, 5),
            ("12h", TimeUnit::Hour, 12),
        ] {
            let timeframe = Timeframe::parse(raw).expect("parse");
            assert_eq!(timeframe.unit, unit);
            assert_eq!(timeframe.count, count);
            assert_eq!(timeframe.to_string(), raw);
        }
        assert_eq!(
            Timeframe::parse("1d").expect("1d").duration_ms(),
            86_400_000
        );
        assert_eq!(
            Timeframe::parse("4h").expect("4h").duration_ms(),
            14_400_000
        );
    }

    #[test]
    fn t1_8_timeframe_rejects_invalid_inputs() {
        for raw in ["", "d", "0d", "1x", "-1d", "1.5d", "1D"] {
            assert!(Timeframe::parse(raw).is_err(), "{raw} 应被拒绝");
        }
    }

    #[test]
    fn periods_per_year_follows_asset_class_and_timeframe() {
        assert_eq!(Timeframe::ONE_DAY.bars_per_year(AssetClass::Crypto), 365);
        assert_eq!(Timeframe::ONE_DAY.bars_per_year(AssetClass::UsEquity), 252);
        assert_eq!(Timeframe::ONE_DAY.bars_per_year(AssetClass::Fx), 252);
        let cases = [
            ("1m", 525_600, 98_280, 362_880),
            ("5m", 105_120, 19_656, 72_576),
            ("15m", 35_040, 6_552, 24_192),
            ("30m", 17_520, 3_276, 12_096),
            ("1h", 8_760, 1_638, 6_048),
            ("4h", 2_190, 410, 1_512),
            ("1w", 52, 52, 52),
        ];
        for (raw, crypto, equity, fx) in cases {
            let timeframe = Timeframe::parse(raw).expect(raw);
            assert_eq!(timeframe.bars_per_year(AssetClass::Crypto), crypto, "{raw}");
            assert_eq!(
                timeframe.bars_per_year(AssetClass::UsEquity),
                equity,
                "{raw}"
            );
            assert_eq!(timeframe.bars_per_year(AssetClass::Fx), fx, "{raw}");
        }
    }

    #[test]
    fn estimated_data_bytes_counts_timeline_and_ohlcv() {
        let universe = Universe {
            timeframe: Timeframe::ONE_DAY,
            specs: Vec::new(),
            series: vec![
                BarSeries {
                    symbol: 0,
                    ts: vec![0; 3],
                    open: vec![0.0; 3],
                    high: vec![0.0; 3],
                    low: vec![0.0; 3],
                    close: vec![0.0; 3],
                    volume: vec![0.0; 3],
                },
                BarSeries {
                    symbol: 1,
                    ts: vec![0; 3],
                    open: vec![0.0; 3],
                    high: vec![0.0; 3],
                    low: vec![0.0; 3],
                    close: vec![0.0; 3],
                    volume: vec![0.0; 3],
                },
            ],
            funding: Vec::new(),
            timeline: vec![0; 3],
            data_hash: String::new(),
            base_index: 0,
        };
        assert_eq!(universe.estimated_data_bytes(), 3 * 8 + 2 * 3 * 5 * 8);
    }
}
