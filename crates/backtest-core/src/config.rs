use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ConfigError, Result};
use crate::model::{AssetClass, InstrumentSpec, Timeframe, Ts};
use crate::strategy::DEFAULT_VOL_FLOOR;

#[derive(Clone, Copy, Debug)]
pub enum ThreadSetting {
    Auto,
    Fixed(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Intrabar,
    Close,
}

#[derive(Clone, Copy, Debug)]
pub struct RiskLeg {
    pub trigger: Trigger,
    pub distance: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RiskConfig {
    pub stop_loss: Option<RiskLeg>,
    pub take_profit: Option<RiskLeg>,
}

#[derive(Clone, Copy, Debug)]
pub struct ExecutionConfig {
    pub participation_rate: f64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            participation_rate: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Weighting {
    Equal,
    InverseVol { lookback: usize, floor: f64 },
}

impl Weighting {
    pub fn name(&self) -> &'static str {
        match self {
            Weighting::Equal => "equal",
            Weighting::InverseVol { .. } => "inverse_vol",
        }
    }
}

#[derive(Clone, Debug)]
pub enum StrategyConfig {
    SmaCross {
        fast: usize,
        slow: usize,
        weight: f64,
        allow_short: bool,
    },
    XsMomentum {
        lookback: usize,
        top_k: usize,
        rebalance_bars: usize,
        weighting: Weighting,
        gross: f64,
    },
}

impl StrategyConfig {
    pub fn name(&self) -> &'static str {
        match self {
            StrategyConfig::SmaCross { .. } => "sma_cross",
            StrategyConfig::XsMomentum { .. } => "xs_momentum",
        }
    }

    pub fn params(&self) -> BTreeMap<String, serde_json::Value> {
        let mut map = BTreeMap::new();
        match self {
            StrategyConfig::SmaCross {
                fast,
                slow,
                weight,
                allow_short,
            } => {
                map.insert("fast".into(), serde_json::json!(fast));
                map.insert("slow".into(), serde_json::json!(slow));
                map.insert("weight".into(), serde_json::json!(weight));
                map.insert("allow_short".into(), serde_json::json!(allow_short));
            }
            StrategyConfig::XsMomentum {
                lookback,
                top_k,
                rebalance_bars,
                weighting,
                gross,
            } => {
                map.insert("lookback".into(), serde_json::json!(lookback));
                map.insert("top_k".into(), serde_json::json!(top_k));
                map.insert("rebalance_bars".into(), serde_json::json!(rebalance_bars));
                map.insert("weighting".into(), serde_json::json!(weighting.name()));
                if let Weighting::InverseVol { lookback, floor } = weighting {
                    map.insert("vol_lookback".into(), serde_json::json!(lookback));
                    map.insert("vol_floor".into(), serde_json::json!(floor));
                }
                map.insert("gross".into(), serde_json::json!(gross));
            }
        }
        map
    }

    pub fn warmup_bars(&self) -> usize {
        match self {
            StrategyConfig::SmaCross { slow, .. } => *slow,
            StrategyConfig::XsMomentum {
                lookback,
                weighting,
                ..
            } => match weighting {
                Weighting::Equal => *lookback,
                Weighting::InverseVol { lookback: vol, .. } => (*lookback).max(*vol),
            },
        }
    }

    pub fn accepts_param(&self, name: &str) -> bool {
        match self {
            StrategyConfig::SmaCross { .. } => {
                matches!(name, "fast" | "slow" | "weight" | "allow_short")
            }
            StrategyConfig::XsMomentum { .. } => matches!(
                name,
                "lookback"
                    | "top_k"
                    | "rebalance_bars"
                    | "weighting"
                    | "vol_lookback"
                    | "vol_floor"
                    | "gross"
            ),
        }
    }

    /// 用扫描网格中的一个标量覆盖策略参数。
    pub fn apply_param(&mut self, name: &str, value: &serde_json::Value) -> Result<()> {
        let field_name = format!("sweep.grid.{name}");
        match self {
            StrategyConfig::SmaCross {
                fast,
                slow,
                weight,
                allow_short,
            } => match name {
                "fast" => *fast = json_usize(&field_name, value)?,
                "slow" => *slow = json_usize(&field_name, value)?,
                "weight" => *weight = json_f64(&field_name, value)?,
                "allow_short" => {
                    *allow_short = value.as_bool().ok_or_else(|| ConfigError::Invalid {
                        field: field_name,
                        message: format!("期望布尔值,实际 {value}"),
                    })?
                }
                _ => return Err(unknown_param(name)),
            },
            StrategyConfig::XsMomentum {
                lookback,
                top_k,
                rebalance_bars,
                weighting,
                gross,
            } => match name {
                "lookback" => *lookback = json_usize(&field_name, value)?,
                "top_k" => *top_k = json_usize(&field_name, value)?,
                "rebalance_bars" => *rebalance_bars = json_usize(&field_name, value)?,
                "weighting" => {
                    let raw = value.as_str().ok_or_else(|| ConfigError::Invalid {
                        field: field_name,
                        message: format!("期望字符串,实际 {value}"),
                    })?;
                    *weighting = parse_weighting_value(raw, *lookback, *weighting)?;
                }
                "vol_lookback" => {
                    let vol = json_usize(&field_name, value)?;
                    let floor = match weighting {
                        Weighting::InverseVol { floor, .. } => *floor,
                        Weighting::Equal => DEFAULT_VOL_FLOOR,
                    };
                    *weighting = Weighting::InverseVol {
                        lookback: vol,
                        floor,
                    };
                }
                "vol_floor" => {
                    let floor = json_f64(&field_name, value)?;
                    let vol = match weighting {
                        Weighting::InverseVol { lookback, .. } => *lookback,
                        Weighting::Equal => *lookback,
                    };
                    *weighting = Weighting::InverseVol {
                        lookback: vol,
                        floor,
                    };
                }
                "gross" => *gross = json_f64(&field_name, value)?,
                _ => return Err(unknown_param(name)),
            },
        }
        Ok(())
    }
}

fn parse_weighting_value(raw: &str, lookback: usize, current: Weighting) -> Result<Weighting> {
    match raw.to_ascii_lowercase().as_str() {
        "equal" => Ok(Weighting::Equal),
        "inverse_vol" => Ok(match current {
            Weighting::InverseVol { lookback, floor } => Weighting::InverseVol { lookback, floor },
            Weighting::Equal => Weighting::InverseVol {
                lookback,
                floor: DEFAULT_VOL_FLOOR,
            },
        }),
        other => Err(ConfigError::Unsupported {
            what: "加权方式",
            value: other.to_string(),
            supported: "equal, inverse_vol",
        }
        .into()),
    }
}

fn json_usize(field_name: &str, value: &serde_json::Value) -> Result<usize> {
    let raw = value.as_u64().ok_or_else(|| ConfigError::Invalid {
        field: field_name.into(),
        message: format!("期望非负整数,实际 {value}"),
    })?;
    usize::try_from(raw).map_err(|_| {
        ConfigError::Invalid {
            field: field_name.into(),
            message: format!("{raw} 超出 usize 范围"),
        }
        .into()
    })
}

fn json_f64(field_name: &str, value: &serde_json::Value) -> Result<f64> {
    value.as_f64().ok_or_else(|| {
        ConfigError::Invalid {
            field: field_name.into(),
            message: format!("期望数值,实际 {value}"),
        }
        .into()
    })
}

fn unknown_param(name: &str) -> crate::error::Error {
    ConfigError::Invalid {
        field: format!("sweep.grid.{name}"),
        message: format!("策略不支持参数 '{name}'"),
    }
    .into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Objective {
    Sharpe,
    Cagr,
    Calmar,
}

impl Objective {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "sharpe" => Ok(Self::Sharpe),
            "cagr" => Ok(Self::Cagr),
            "calmar" => Ok(Self::Calmar),
            other => Err(ConfigError::Unsupported {
                what: "扫描目标",
                value: other.to_string(),
                supported: "sharpe, cagr, calmar",
            }
            .into()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sharpe => "sharpe",
            Self::Cagr => "cagr",
            Self::Calmar => "calmar",
        }
    }

    pub fn value(self, metrics: &crate::model::Metrics) -> Option<f64> {
        match self {
            Self::Sharpe => metrics.sharpe,
            Self::Cagr => metrics.cagr,
            Self::Calmar => metrics.calmar,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SweepConfig {
    pub objective: Objective,
    pub top_k: usize,
    pub oos_split: f64,
    pub grid: BTreeMap<String, Vec<serde_json::Value>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    Rolling,
    Anchored,
}

impl WindowMode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "rolling" => Ok(Self::Rolling),
            "anchored" => Ok(Self::Anchored),
            other => Err(ConfigError::Unsupported {
                what: "walk-forward 窗口模式",
                value: other.to_string(),
                supported: "rolling, anchored",
            }
            .into()),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Rolling => "rolling",
            Self::Anchored => "anchored",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WalkForwardConfig {
    pub mode: WindowMode,
    pub train_bars: usize,
    pub test_bars: usize,
    pub step_bars: usize,
    pub embargo_bars: usize,
    pub warmup_bars: usize,
    pub objective: Objective,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinanceMarket {
    Spot,
    Um,
    Cm,
}

impl BinanceMarket {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "spot" => Ok(Self::Spot),
            "futures/um" | "um" => Ok(Self::Um),
            "futures/cm" | "cm" => Ok(Self::Cm),
            other => Err(ConfigError::Unsupported {
                what: "binance 市场",
                value: other.to_string(),
                supported: "spot, futures/um, futures/cm",
            }
            .into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Um => "futures/um",
            Self::Cm => "futures/cm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DukascopyPrice {
    Bid,
    Ask,
}

impl DukascopyPrice {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bid" => Ok(Self::Bid),
            "ask" => Ok(Self::Ask),
            other => Err(ConfigError::Unsupported {
                what: "dukascopy 报价侧",
                value: other.to_string(),
                supported: "bid, ask",
            }
            .into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bid => "BID",
            Self::Ask => "ASK",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinanceSource {
    pub root: PathBuf,
    pub market: BinanceMarket,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DukascopySource {
    pub root: PathBuf,
    pub price_type: DukascopyPrice,
}

/// `data-reader` 的归档数据源。归档格式与字段由上游模块负责。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceConfig {
    Binance(BinanceSource),
    Dukascopy(DukascopySource),
}

impl SourceConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Binance(_) => "binance",
            Self::Dukascopy(_) => "dukascopy",
        }
    }

    pub fn root(&self) -> &Path {
        match self {
            Self::Binance(source) => &source.root,
            Self::Dukascopy(source) => &source.root,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Binance(source) => format!(
                "binance {}: {}",
                source.market.as_str(),
                source.root.display()
            ),
            Self::Dukascopy(source) => format!(
                "dukascopy {}: {}",
                source.price_type.as_str(),
                source.root.display()
            ),
        }
    }
}

/// 一次回测的数据入口:规范 CSV 或归档模块。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataInput {
    Csv(PathBuf),
    Archive(SourceConfig),
}

impl DataInput {
    pub fn describe(&self) -> String {
        match self {
            Self::Csv(path) => format!("csv: {}", path.display()),
            Self::Archive(source) => source.describe(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub data: DataInput,
    pub initial_cash: f64,
    pub start: Option<Ts>,
    pub end: Option<Ts>,
    pub output_dir: PathBuf,
    pub threads: ThreadSetting,
    pub memory_limit_gb: f64,
    pub timeframe: Timeframe,
    pub risk_free_rate: f64,
    pub execution: ExecutionConfig,
    pub risk: RiskConfig,
    pub strategy: StrategyConfig,
    pub instruments: Vec<InstrumentSpec>,
    pub sweep: Option<SweepConfig>,
    pub walk_forward: Option<WalkForwardConfig>,
    pub config_hash: String,
}

impl RunConfig {
    pub fn apply_overrides(
        &mut self,
        threads: Option<usize>,
        memory_limit_gb: Option<f64>,
        output_dir: Option<PathBuf>,
    ) -> Result<()> {
        if let Some(value) = threads {
            if value == 0 {
                return Err(ConfigError::Invalid {
                    field: "threads".into(),
                    message: "必须大于 0".into(),
                }
                .into());
            }
            self.threads = ThreadSetting::Fixed(value);
        }
        if let Some(value) = memory_limit_gb {
            if !value.is_finite() || value < 0.0 {
                return Err(ConfigError::Invalid {
                    field: "memory_limit_gb".into(),
                    message: "必须是有限且 >= 0 的数值".into(),
                }
                .into());
            }
            self.memory_limit_gb = value;
        }
        if let Some(value) = output_dir {
            if value.as_os_str().is_empty() {
                return Err(ConfigError::Invalid {
                    field: "output_dir".into(),
                    message: "不能为空".into(),
                }
                .into());
            }
            self.output_dir = value;
        }
        Ok(())
    }

    pub fn with_strategy(&self, strategy: StrategyConfig) -> Result<Self> {
        validate_strategy(&strategy, &self.instruments)?;
        let config_hash = hash_config(&ConfigHashInput {
            initial_cash: self.initial_cash,
            start: self.start,
            end: self.end,
            timeframe: self.timeframe,
            risk_free_rate: self.risk_free_rate,
            execution: self.execution,
            risk: self.risk,
            strategy: &strategy,
            instruments: &self.instruments,
        })?;
        Ok(Self {
            strategy,
            config_hash,
            ..self.clone()
        })
    }

    pub fn min_margin_rate(&self) -> f64 {
        self.instruments
            .iter()
            .map(|spec| spec.margin_rate)
            .fold(f64::INFINITY, f64::min)
    }

    pub fn periods_per_year(&self) -> u32 {
        self.instruments
            .first()
            .map(|spec| spec.effective_periods_per_year(self.timeframe))
            .unwrap_or_else(|| self.timeframe.bars_per_year(AssetClass::Crypto))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    run: RawRun,
    #[serde(default)]
    source: Option<RawSource>,
    #[serde(default)]
    execution: RawExecution,
    #[serde(default)]
    risk: RawRisk,
    strategy: RawStrategy,
    #[serde(default)]
    sweep: Option<RawSweep>,
    #[serde(default)]
    walk_forward: Option<RawWalkForward>,
    #[serde(default)]
    instruments: Vec<RawInstrument>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRun {
    #[serde(default)]
    data: Option<String>,
    initial_cash: f64,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default = "default_output_dir")]
    output_dir: String,
    #[serde(default)]
    threads: Option<RawThreads>,
    #[serde(default)]
    memory_limit_gb: Option<f64>,
    timeframe: String,
    #[serde(default)]
    risk_free_rate: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    kind: String,
    root: String,
    #[serde(default)]
    market: Option<String>,
    #[serde(default)]
    price_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawThreads {
    Auto(String),
    Count(usize),
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawExecution {
    #[serde(default)]
    participation_rate: f64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawRisk {
    #[serde(default)]
    stop_loss: Option<RawRiskLeg>,
    #[serde(default)]
    take_profit: Option<RawRiskLeg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRiskLeg {
    trigger: String,
    distance: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStrategy {
    name: String,
    #[serde(default)]
    fast: Option<usize>,
    #[serde(default)]
    slow: Option<usize>,
    #[serde(default)]
    weight: Option<f64>,
    #[serde(default)]
    allow_short: Option<bool>,
    #[serde(default)]
    lookback: Option<usize>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    rebalance_bars: Option<usize>,
    #[serde(default)]
    weighting: Option<String>,
    #[serde(default)]
    vol_lookback: Option<usize>,
    #[serde(default)]
    vol_floor: Option<f64>,
    #[serde(default)]
    gross: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSweep {
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    oos_split: Option<f64>,
    #[serde(default)]
    grid: BTreeMap<String, Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWalkForward {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    train_bars: Option<usize>,
    #[serde(default)]
    test_bars: Option<usize>,
    #[serde(default)]
    step_bars: Option<usize>,
    #[serde(default)]
    embargo_bars: Option<usize>,
    #[serde(default)]
    warmup_bars: Option<usize>,
    #[serde(default)]
    objective: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInstrument {
    symbol: String,
    asset_class: String,
    currency: String,
    #[serde(default = "one")]
    multiplier: f64,
    #[serde(default)]
    lot_size: f64,
    #[serde(default = "one")]
    margin_rate: f64,
    #[serde(default = "five")]
    fee_bps: f64,
    #[serde(default)]
    fee_fixed: f64,
    #[serde(default = "two")]
    slippage_bps: f64,
    #[serde(default)]
    periods_per_year: Option<u32>,
}

fn default_output_dir() -> String {
    "out".into()
}

fn one() -> f64 {
    1.0
}

fn two() -> f64 {
    2.0
}

fn five() -> f64 {
    5.0
}

pub fn load_config(path: &Path) -> Result<RunConfig> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let raw: RawConfig = toml::from_str(&text).map_err(|source| ConfigError::ParseToml {
        path: path.to_path_buf(),
        source,
    })?;
    validate(raw)
}

fn validate(mut raw: RawConfig) -> Result<RunConfig> {
    let timeframe = Timeframe::parse(&raw.run.timeframe)?;
    let data = parse_data_input(raw.run.data.take(), raw.source.take(), timeframe)?;
    if !raw.run.initial_cash.is_finite() || raw.run.initial_cash <= 0.0 {
        return Err(field("run.initial_cash", "必须是有限且 > 0 的数值"));
    }
    if !raw.run.risk_free_rate.is_finite() || raw.run.risk_free_rate <= -1.0 {
        return Err(field("run.risk_free_rate", "必须是有限且 > -1 的数值"));
    }
    if raw.run.output_dir.trim().is_empty() {
        return Err(field("run.output_dir", "不能为空"));
    }
    let memory_limit_gb = raw.run.memory_limit_gb.unwrap_or(0.0);
    if !memory_limit_gb.is_finite() || memory_limit_gb < 0.0 {
        return Err(field("run.memory_limit_gb", "必须是有限且 >= 0 的数值"));
    }
    let start = raw
        .run
        .start
        .as_deref()
        .map(|s| parse_ts(s, "run.start"))
        .transpose()?;
    let end = raw
        .run
        .end
        .as_deref()
        .map(|s| parse_ts(s, "run.end"))
        .transpose()?;
    if let (Some(start), Some(end)) = (start, end)
        && start >= end
    {
        return Err(field("run.start", "必须早于 run.end"));
    }
    let threads = match raw.run.threads {
        None => ThreadSetting::Auto,
        Some(RawThreads::Auto(text)) if text.eq_ignore_ascii_case("auto") => ThreadSetting::Auto,
        Some(RawThreads::Auto(text)) => {
            return Err(ConfigError::Unsupported {
                what: "线程设置",
                value: text,
                supported: "auto 或正整数",
            }
            .into());
        }
        Some(RawThreads::Count(0)) => {
            return Err(field("run.threads", "必须大于 0"));
        }
        Some(RawThreads::Count(value)) => ThreadSetting::Fixed(value),
    };

    if !raw.execution.participation_rate.is_finite()
        || !(0.0..=1.0).contains(&raw.execution.participation_rate)
    {
        return Err(field(
            "execution.participation_rate",
            "必须在 [0, 1] 区间内",
        ));
    }
    let execution = ExecutionConfig {
        participation_rate: raw.execution.participation_rate,
    };

    let risk = RiskConfig {
        stop_loss: parse_risk_leg(raw.risk.stop_loss, "risk.stop_loss")?,
        take_profit: parse_risk_leg(raw.risk.take_profit, "risk.take_profit")?,
    };

    let strategy = parse_strategy(raw.strategy)?;
    let sweep = parse_sweep(raw.sweep, &strategy)?;
    let walk_forward = parse_walk_forward(raw.walk_forward, &strategy)?;

    if raw.instruments.is_empty() {
        return Err(field("instruments", "至少需要一个标的"));
    }
    let mut instruments = Vec::with_capacity(raw.instruments.len());
    let mut seen = std::collections::BTreeSet::new();
    for (index, item) in raw.instruments.into_iter().enumerate() {
        let prefix = format!("instruments[{index}]");
        if item.symbol.trim().is_empty() {
            return Err(field(&format!("{prefix}.symbol"), "不能为空"));
        }
        if !seen.insert(item.symbol.clone()) {
            return Err(field(
                &format!("{prefix}.symbol"),
                &format!("标的 '{}' 重复", item.symbol),
            ));
        }
        if item.currency.trim().is_empty() {
            return Err(field(&format!("{prefix}.currency"), "不能为空"));
        }
        validate_positive(&format!("{prefix}.multiplier"), item.multiplier)?;
        validate_non_negative(&format!("{prefix}.lot_size"), item.lot_size)?;
        validate_positive(&format!("{prefix}.margin_rate"), item.margin_rate)?;
        validate_non_negative(&format!("{prefix}.fee_bps"), item.fee_bps)?;
        validate_non_negative(&format!("{prefix}.fee_fixed"), item.fee_fixed)?;
        validate_non_negative(&format!("{prefix}.slippage_bps"), item.slippage_bps)?;
        if matches!(item.periods_per_year, Some(0)) {
            return Err(field(&format!("{prefix}.periods_per_year"), "必须大于 0"));
        }
        instruments.push(InstrumentSpec {
            symbol: item.symbol,
            asset_class: AssetClass::parse(&item.asset_class)?,
            currency: item.currency,
            multiplier: item.multiplier,
            lot_size: item.lot_size,
            margin_rate: item.margin_rate,
            fee_bps: item.fee_bps,
            fee_fixed: item.fee_fixed,
            slippage_bps: item.slippage_bps,
            periods_per_year: item.periods_per_year,
        });
    }
    let currency = &instruments[0].currency;
    for spec in &instruments[1..] {
        if &spec.currency != currency {
            return Err(field(
                "instruments.currency",
                &format!(
                    "一次回测只允许一种计价货币:{} 与 {} 不一致",
                    currency, spec.currency
                ),
            ));
        }
    }
    let ppy = instruments[0].effective_periods_per_year(timeframe);
    for spec in &instruments[1..] {
        let other = spec.effective_periods_per_year(timeframe);
        if other != ppy {
            return Err(field(
                "instruments.periods_per_year",
                &format!(
                    "组合内年化因子必须一致:{} 为 {},{} 为 {}",
                    instruments[0].symbol, ppy, spec.symbol, other
                ),
            ));
        }
    }
    let min_margin = instruments
        .iter()
        .map(|spec| spec.margin_rate)
        .fold(f64::INFINITY, f64::min);
    let limit = 1.0 / min_margin;
    match &strategy {
        StrategyConfig::SmaCross { weight, .. } if *weight > limit + 1e-12 => {
            return Err(field(
                "strategy.weight",
                &format!("{} 超过保证金允许的单标的权重上限 {:.6}", weight, limit),
            ));
        }
        StrategyConfig::XsMomentum { gross, .. } if *gross > limit + 1e-12 => {
            return Err(field(
                "strategy.gross",
                &format!("{} 超过保证金允许的总敞口上限 {:.6}", gross, limit),
            ));
        }
        _ => {}
    }

    let config_hash = hash_config(&ConfigHashInput {
        initial_cash: raw.run.initial_cash,
        start,
        end,
        timeframe,
        risk_free_rate: raw.run.risk_free_rate,
        execution,
        risk,
        strategy: &strategy,
        instruments: &instruments,
    })?;

    Ok(RunConfig {
        data,
        initial_cash: raw.run.initial_cash,
        start,
        end,
        output_dir: PathBuf::from(raw.run.output_dir),
        threads,
        memory_limit_gb,
        timeframe,
        risk_free_rate: raw.run.risk_free_rate,
        execution,
        risk,
        strategy,
        instruments,
        sweep,
        walk_forward,
        config_hash,
    })
}

fn parse_data_input(
    csv: Option<String>,
    source: Option<RawSource>,
    timeframe: Timeframe,
) -> Result<DataInput> {
    match (csv, source) {
        (Some(path), None) => {
            if path.trim().is_empty() {
                return Err(field("run.data", "不能为空"));
            }
            Ok(DataInput::Csv(PathBuf::from(path)))
        }
        (None, Some(raw)) => Ok(DataInput::Archive(parse_source(raw, timeframe)?)),
        (Some(_), Some(_)) => Err(field("source", "与 run.data 只能二选一")),
        (None, None) => Err(field(
            "run.data",
            "缺少数据源:填写 run.data 或增加 [source] 段",
        )),
    }
}

fn parse_source(raw: RawSource, timeframe: Timeframe) -> Result<SourceConfig> {
    if raw.root.trim().is_empty() {
        return Err(field("source.root", "不能为空"));
    }
    let root = PathBuf::from(raw.root);
    match raw.kind.trim().to_ascii_lowercase().as_str() {
        "binance" => {
            if raw.price_type.is_some() {
                return Err(field("source.price_type", "仅适用于 dukascopy 数据源"));
            }
            let market = raw
                .market
                .ok_or_else(|| field("source.market", "binance 数据源必须指定 market"))?;
            ensure_source_timeframe(
                "binance",
                timeframe,
                &BINANCE_TIMEFRAMES,
                "binance: 1m/3m/5m/15m/30m/1h/2h/4h/6h/8h/12h/1d/3d/1w",
            )?;
            Ok(SourceConfig::Binance(BinanceSource {
                root,
                market: BinanceMarket::parse(&market)?,
            }))
        }
        "dukascopy" => {
            if raw.market.is_some() {
                return Err(field("source.market", "仅适用于 binance 数据源"));
            }
            let price_type = match raw.price_type {
                Some(text) => DukascopyPrice::parse(&text)?,
                None => DukascopyPrice::Bid,
            };
            ensure_source_timeframe(
                "dukascopy",
                timeframe,
                &DUKASCOPY_TIMEFRAMES,
                "dukascopy: 1m/1h/1d",
            )?;
            Ok(SourceConfig::Dukascopy(DukascopySource {
                root,
                price_type,
            }))
        }
        other => Err(ConfigError::Unsupported {
            what: "数据源",
            value: other.to_string(),
            supported: "binance, dukascopy",
        }
        .into()),
    }
}

const BINANCE_TIMEFRAMES: [&str; 14] = [
    "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "6h", "8h", "12h", "1d", "3d", "1w",
];
const DUKASCOPY_TIMEFRAMES: [&str; 3] = ["1m", "1h", "1d"];

fn ensure_source_timeframe(
    provider: &str,
    timeframe: Timeframe,
    allowed: &[&str],
    supported: &'static str,
) -> Result<()> {
    let text = timeframe.to_string();
    if allowed.contains(&text.as_str()) {
        return Ok(());
    }
    Err(ConfigError::Unsupported {
        what: "数据源周期",
        value: format!("{provider} {text}"),
        supported,
    }
    .into())
}

fn parse_strategy(raw: RawStrategy) -> Result<StrategyConfig> {
    match raw.name.as_str() {
        "sma_cross" => {
            let fast = raw
                .fast
                .ok_or_else(|| field("strategy.fast", "缺少该字段"))?;
            let slow = raw
                .slow
                .ok_or_else(|| field("strategy.slow", "缺少该字段"))?;
            if fast == 0 {
                return Err(field("strategy.fast", "必须大于 0"));
            }
            if slow <= fast {
                return Err(field("strategy.slow", "必须大于 strategy.fast"));
            }
            let weight = raw.weight.unwrap_or(1.0);
            if !weight.is_finite() || weight <= 0.0 {
                return Err(field("strategy.weight", "必须是有限且 > 0 的数值"));
            }
            Ok(StrategyConfig::SmaCross {
                fast,
                slow,
                weight,
                allow_short: raw.allow_short.unwrap_or(false),
            })
        }
        "xs_momentum" => {
            let lookback = raw
                .lookback
                .ok_or_else(|| field("strategy.lookback", "缺少该字段"))?;
            let top_k = raw
                .top_k
                .ok_or_else(|| field("strategy.top_k", "缺少该字段"))?;
            let rebalance_bars = raw
                .rebalance_bars
                .ok_or_else(|| field("strategy.rebalance_bars", "缺少该字段"))?;
            if lookback == 0 {
                return Err(field("strategy.lookback", "必须大于 0"));
            }
            if top_k == 0 {
                return Err(field("strategy.top_k", "必须大于 0"));
            }
            if rebalance_bars == 0 {
                return Err(field("strategy.rebalance_bars", "必须大于 0"));
            }
            let gross = raw.gross.unwrap_or(1.0);
            if !gross.is_finite() || gross <= 0.0 {
                return Err(field("strategy.gross", "必须是有限且 > 0 的数值"));
            }
            let weighting = match raw
                .weighting
                .as_deref()
                .unwrap_or("inverse_vol")
                .to_ascii_lowercase()
                .as_str()
            {
                "equal" => Weighting::Equal,
                "inverse_vol" => {
                    let vol_lookback = raw.vol_lookback.unwrap_or(lookback);
                    if vol_lookback < 2 {
                        return Err(field("strategy.vol_lookback", "必须大于等于 2"));
                    }
                    let floor = raw.vol_floor.unwrap_or(DEFAULT_VOL_FLOOR);
                    if !floor.is_finite() || floor <= 0.0 {
                        return Err(field("strategy.vol_floor", "必须是有限且 > 0 的数值"));
                    }
                    Weighting::InverseVol {
                        lookback: vol_lookback,
                        floor,
                    }
                }
                other => {
                    return Err(ConfigError::Unsupported {
                        what: "加权方式",
                        value: other.to_string(),
                        supported: "equal, inverse_vol",
                    }
                    .into());
                }
            };
            Ok(StrategyConfig::XsMomentum {
                lookback,
                top_k,
                rebalance_bars,
                weighting,
                gross,
            })
        }
        other => Err(ConfigError::Unsupported {
            what: "策略",
            value: other.to_string(),
            supported: "sma_cross, xs_momentum",
        }
        .into()),
    }
}

fn parse_sweep(raw: Option<RawSweep>, strategy: &StrategyConfig) -> Result<Option<SweepConfig>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let objective = match raw.objective {
        Some(text) => Objective::parse(&text)?,
        None => Objective::Sharpe,
    };
    let top_k = raw.top_k.unwrap_or(20);
    if top_k == 0 {
        return Err(field("sweep.top_k", "必须大于 0"));
    }
    let oos_split = raw.oos_split.unwrap_or(0.7);
    if !oos_split.is_finite() || oos_split <= 0.0 || oos_split >= 1.0 {
        return Err(field("sweep.oos_split", "必须在 (0, 1) 区间内"));
    }
    if raw.grid.is_empty() {
        return Err(field("sweep.grid", "至少需要一个参数"));
    }
    for (name, values) in &raw.grid {
        if name.trim().is_empty() {
            return Err(field("sweep.grid", "参数名不能为空"));
        }
        if !strategy.accepts_param(name) {
            return Err(field(
                &format!("sweep.grid.{name}"),
                &format!("策略 '{}' 不支持该参数", strategy.name()),
            ));
        }
        if values.is_empty() {
            return Err(field(&format!("sweep.grid.{name}"), "参数值列表不能为空"));
        }
        for value in values {
            if !(value.is_number() || value.is_string() || value.is_boolean()) {
                return Err(field(
                    &format!("sweep.grid.{name}"),
                    &format!("只支持标量值,实际 {value}"),
                ));
            }
        }
    }
    Ok(Some(SweepConfig {
        objective,
        top_k,
        oos_split,
        grid: raw.grid,
    }))
}

fn parse_walk_forward(
    raw: Option<RawWalkForward>,
    strategy: &StrategyConfig,
) -> Result<Option<WalkForwardConfig>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mode = match raw.mode {
        Some(text) => WindowMode::parse(&text)?,
        None => WindowMode::Rolling,
    };
    let train_bars = raw.train_bars.unwrap_or(756);
    let test_bars = raw.test_bars.unwrap_or(252);
    let step_bars = raw.step_bars.unwrap_or(test_bars);
    let embargo_bars = raw.embargo_bars.unwrap_or(1);
    let warmup_bars = raw.warmup_bars.unwrap_or(60);
    let objective = match raw.objective {
        Some(text) => Objective::parse(&text)?,
        None => Objective::Sharpe,
    };
    if train_bars == 0 {
        return Err(field("walk_forward.train_bars", "必须大于 0"));
    }
    if test_bars == 0 {
        return Err(field("walk_forward.test_bars", "必须大于 0"));
    }
    if step_bars < test_bars {
        return Err(field(
            "walk_forward.step_bars",
            "必须大于等于 test_bars;重叠窗口不允许",
        ));
    }
    if warmup_bars == 0 {
        return Err(field("walk_forward.warmup_bars", "必须大于 0"));
    }
    let max_warmup = train_bars.checked_add(embargo_bars).ok_or_else(|| {
        field(
            "walk_forward.train_bars",
            "train_bars + embargo_bars 超出可表示范围",
        )
    })?;
    if warmup_bars > max_warmup {
        return Err(field(
            "walk_forward.warmup_bars",
            &format!("不能超过 train_bars + embargo_bars = {max_warmup}"),
        ));
    }
    let strategy_warmup = strategy.warmup_bars();
    if warmup_bars < strategy_warmup {
        return Err(field(
            "walk_forward.warmup_bars",
            &format!(
                "策略 '{}' 需要至少 {strategy_warmup} 根预热 bar",
                strategy.name()
            ),
        ));
    }
    Ok(Some(WalkForwardConfig {
        mode,
        train_bars,
        test_bars,
        step_bars,
        embargo_bars,
        warmup_bars,
        objective,
    }))
}

fn parse_risk_leg(raw: Option<RawRiskLeg>, field_name: &str) -> Result<Option<RiskLeg>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if !raw.distance.is_finite() || raw.distance <= 0.0 || raw.distance >= 1.0 {
        return Err(field(
            &format!("{field_name}.distance"),
            "必须在 (0, 1) 区间内",
        ));
    }
    let trigger = match raw.trigger.to_ascii_lowercase().as_str() {
        "intrabar" => Trigger::Intrabar,
        "close" => Trigger::Close,
        other => {
            return Err(ConfigError::Unsupported {
                what: "触发方式",
                value: other.to_string(),
                supported: "intrabar, close",
            }
            .into());
        }
    };
    Ok(Some(RiskLeg {
        trigger,
        distance: raw.distance,
    }))
}

fn parse_ts(raw: &str, field_name: &str) -> Result<Ts> {
    let text = raw.trim();
    if let Ok(value) = text.parse::<i64>() {
        return Ok(value);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(text) {
        return Ok(dt.with_timezone(&Utc).timestamp_millis());
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt.and_utc().timestamp_millis());
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        let dt = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| field(field_name, "日期时间非法"))?;
        return Ok(dt.and_utc().timestamp_millis());
    }
    Err(field(
        field_name,
        &format!("'{raw}' 不是 RFC3339、epoch 毫秒或 YYYY-MM-DD"),
    ))
}

fn validate_strategy(strategy: &StrategyConfig, instruments: &[InstrumentSpec]) -> Result<()> {
    let min_margin = instruments
        .iter()
        .map(|spec| spec.margin_rate)
        .fold(f64::INFINITY, f64::min);
    let limit = 1.0 / min_margin;
    match strategy {
        StrategyConfig::SmaCross {
            fast, slow, weight, ..
        } => {
            if *fast == 0 {
                return Err(field("strategy.fast", "必须大于 0"));
            }
            if *slow <= *fast {
                return Err(field("strategy.slow", "必须大于 strategy.fast"));
            }
            if !weight.is_finite() || *weight <= 0.0 {
                return Err(field("strategy.weight", "必须是有限且 > 0 的数值"));
            }
            if *weight > limit + 1e-12 {
                return Err(field(
                    "strategy.weight",
                    &format!("{} 超过保证金允许的单标的权重上限 {:.6}", weight, limit),
                ));
            }
        }
        StrategyConfig::XsMomentum {
            lookback,
            top_k,
            rebalance_bars,
            weighting,
            gross,
        } => {
            if *lookback == 0 {
                return Err(field("strategy.lookback", "必须大于 0"));
            }
            if *top_k == 0 {
                return Err(field("strategy.top_k", "必须大于 0"));
            }
            if *rebalance_bars == 0 {
                return Err(field("strategy.rebalance_bars", "必须大于 0"));
            }
            if let Weighting::InverseVol { lookback, floor } = weighting {
                if *lookback < 2 {
                    return Err(field("strategy.vol_lookback", "必须大于等于 2"));
                }
                if !floor.is_finite() || *floor <= 0.0 {
                    return Err(field("strategy.vol_floor", "必须是有限且 > 0 的数值"));
                }
            }
            if !gross.is_finite() || *gross <= 0.0 {
                return Err(field("strategy.gross", "必须是有限且 > 0 的数值"));
            }
            if *gross > limit + 1e-12 {
                return Err(field(
                    "strategy.gross",
                    &format!("{} 超过保证金允许的总敞口上限 {:.6}", gross, limit),
                ));
            }
        }
    }
    Ok(())
}

fn validate_positive(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value <= 0.0 {
        return Err(field(name, "必须是有限且 > 0 的数值"));
    }
    Ok(())
}

fn validate_non_negative(name: &str, value: f64) -> Result<()> {
    if !value.is_finite() || value < 0.0 {
        return Err(field(name, "必须是有限且 >= 0 的数值"));
    }
    Ok(())
}

fn field(name: &str, message: &str) -> crate::error::Error {
    ConfigError::Invalid {
        field: name.to_string(),
        message: message.to_string(),
    }
    .into()
}

struct ConfigHashInput<'a> {
    initial_cash: f64,
    start: Option<Ts>,
    end: Option<Ts>,
    timeframe: Timeframe,
    risk_free_rate: f64,
    execution: ExecutionConfig,
    risk: RiskConfig,
    strategy: &'a StrategyConfig,
    instruments: &'a [InstrumentSpec],
}

fn hash_config(input: &ConfigHashInput<'_>) -> Result<String> {
    let strategy = input.strategy;
    let instruments = input.instruments;
    let value = serde_json::json!({
        "run": {
            "initial_cash": input.initial_cash,
            "start": input.start,
            "end": input.end,
            "timeframe": input.timeframe.to_string(),
            "risk_free_rate": input.risk_free_rate,
        },
        "execution": {
            "participation_rate": input.execution.participation_rate,
        },
        "risk": {
            "stop_loss": input.risk.stop_loss.map(|leg| serde_json::json!({
                "trigger": if leg.trigger == Trigger::Intrabar { "intrabar" } else { "close" },
                "distance": leg.distance,
            })),
            "take_profit": input.risk.take_profit.map(|leg| serde_json::json!({
                "trigger": if leg.trigger == Trigger::Intrabar { "intrabar" } else { "close" },
                "distance": leg.distance,
            })),
        },
        "strategy": {
            "name": strategy.name(),
            "params": strategy.params(),
        },
        "instruments": instruments,
    });
    let bytes = serde_json::to_vec(&value).map_err(|err| ConfigError::Invalid {
        field: "config_hash".into(),
        message: err.to_string(),
    })?;
    Ok(sha256_hex(&bytes))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    const ONE_INSTRUMENT: &str = r#"[[instruments]]
symbol = "AAA"
asset_class = "crypto"
currency = "USD"
multiplier = 1.0
lot_size = 0.0
margin_rate = 1.0
fee_bps = 5.0
fee_fixed = 0.0
slippage_bps = 2.0
periods_per_year = 365
"#;

    fn config_text(timeframe: &str, instruments: &str) -> String {
        format!(
            r#"[run]
data = "bars.csv"
initial_cash = 100000.0
start = "2020-01-01"
end = "2020-02-01"
output_dir = "out"
threads = "auto"
timeframe = "{timeframe}"

[strategy]
name = "sma_cross"
fast = 2
slow = 3
weight = 0.99
allow_short = false

{instruments}"#
        )
    }

    fn load_text(text: &str) -> Result<RunConfig> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run.toml");
        std::fs::write(&path, text).expect("write config");
        load_config(&path)
    }

    #[test]
    fn loads_valid_config_and_hash_is_stable() {
        let text = config_text("1d", ONE_INSTRUMENT);
        let first = load_text(&text).expect("first");
        let second = load_text(&text).expect("second");
        assert_eq!(first.config_hash, second.config_hash);
        assert_eq!(first.instruments.len(), 1);
        assert_eq!(first.timeframe, Timeframe::ONE_DAY);
        assert_eq!(first.periods_per_year(), 365);
        assert_eq!(
            first.start,
            Some(
                NaiveDate::from_ymd_opt(2020, 1, 1)
                    .expect("date")
                    .and_hms_opt(0, 0, 0)
                    .expect("time")
                    .and_utc()
                    .timestamp_millis()
            )
        );
    }

    #[test]
    fn hash_changes_when_parameters_change() {
        let first = load_text(&config_text("1d", ONE_INSTRUMENT)).expect("first");
        let changed = config_text("1d", ONE_INSTRUMENT).replace("weight = 0.99", "weight = 0.98");
        let second = load_text(&changed).expect("second");
        assert_ne!(first.config_hash, second.config_hash);
    }

    #[test]
    fn rejects_currency_mismatch() {
        let other = ONE_INSTRUMENT
            .replace("symbol = \"AAA\"", "symbol = \"BBB\"")
            .replace("currency = \"USD\"", "currency = \"EUR\"");
        let instruments = format!("{ONE_INSTRUMENT}{other}");
        let error = load_text(&config_text("1d", &instruments)).expect_err("must fail");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
    }

    #[test]
    fn accepts_supported_timeframes() {
        for raw in ["1m", "5m", "15m", "30m", "1h", "4h", "1d", "1w"] {
            let config = load_text(&config_text(raw, ONE_INSTRUMENT)).expect(raw);
            assert_eq!(config.timeframe.to_string(), raw);
        }
    }

    fn source_text(timeframe: &str, source: &str) -> String {
        let base = config_text(timeframe, ONE_INSTRUMENT).replace("data = \"bars.csv\"\n", "");
        format!("{base}\n{source}\n")
    }

    #[test]
    fn parses_binance_source_without_csv_path() {
        let text = source_text(
            "1h",
            r#"[source]
kind = "binance"
root = "../data/binance"
market = "spot"
"#,
        );
        let config = load_text(&text).expect("config");
        match &config.data {
            DataInput::Archive(SourceConfig::Binance(source)) => {
                assert_eq!(source.root, PathBuf::from("../data/binance"));
                assert_eq!(source.market, BinanceMarket::Spot);
            }
            other => panic!("unexpected data input: {other:?}"),
        }
    }

    #[test]
    fn parses_dukascopy_source_and_defaults_to_bid() {
        let text = source_text(
            "1d",
            r#"[source]
kind = "dukascopy"
root = "../data"
"#,
        );
        let config = load_text(&text).expect("config");
        match &config.data {
            DataInput::Archive(SourceConfig::Dukascopy(source)) => {
                assert_eq!(source.price_type, DukascopyPrice::Bid);
            }
            other => panic!("unexpected data input: {other:?}"),
        }
    }

    #[test]
    fn rejects_csv_and_source_together() {
        let text = format!(
            "{}\n[source]\nkind = \"binance\"\nroot = \"archive\"\nmarket = \"spot\"\n",
            config_text("1d", ONE_INSTRUMENT)
        );
        let error = load_text(&text).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Config(ConfigError::Invalid { ref field, .. }) if field == "source"
        ));
    }

    #[test]
    fn rejects_missing_data_source() {
        let text = config_text("1d", ONE_INSTRUMENT).replace("data = \"bars.csv\"\n", "");
        let error = load_text(&text).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Config(ConfigError::Invalid { ref field, .. }) if field == "run.data"
        ));
    }

    #[test]
    fn rejects_source_provider_field_mismatch() {
        for source in [
            "[source]\nkind = \"binance\"\nroot = \"archive\"\nmarket = \"spot\"\nprice_type = \"BID\"\n",
            "[source]\nkind = \"dukascopy\"\nroot = \"archive\"\nmarket = \"spot\"\n",
        ] {
            let text = source_text("1d", source);
            let error = load_text(&text).expect_err("must fail");
            assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
        }
    }

    #[test]
    fn rejects_source_timeframe_not_published() {
        let binance = source_text(
            "2d",
            "[source]\nkind = \"binance\"\nroot = \"archive\"\nmarket = \"spot\"\n",
        );
        assert!(matches!(
            load_text(&binance).expect_err("must fail"),
            Error::Config(ConfigError::Unsupported { what, .. }) if what == "数据源周期"
        ));
        let dukascopy = source_text("4h", "[source]\nkind = \"dukascopy\"\nroot = \"archive\"\n");
        assert!(matches!(
            load_text(&dukascopy).expect_err("must fail"),
            Error::Config(ConfigError::Unsupported { what, .. }) if what == "数据源周期"
        ));
    }

    #[test]
    fn rejects_unknown_source_kind_or_field() {
        let kind = source_text(
            "1d",
            "[source]\nkind = \"kraken\"\nroot = \"archive\"\nmarket = \"spot\"\n",
        );
        assert!(matches!(
            load_text(&kind).expect_err("must fail"),
            Error::Config(ConfigError::Unsupported { what, .. }) if what == "数据源"
        ));
        let field = source_text(
            "1d",
            "[source]\nkind = \"binance\"\nroot = \"archive\"\nmarket = \"spot\"\npath = \"x\"\n",
        );
        assert!(matches!(
            load_text(&field).expect_err("must fail"),
            Error::Config(ConfigError::ParseToml { .. })
        ));
    }

    #[test]
    fn periods_per_year_override_wins_over_timeframe() {
        let instruments =
            ONE_INSTRUMENT.replace("periods_per_year = 365", "periods_per_year = 100");
        let config = load_text(&config_text("1h", &instruments)).expect("config");
        assert_eq!(config.timeframe.to_string(), "1h");
        assert_eq!(config.periods_per_year(), 100);
    }

    #[test]
    fn rejects_unsupported_timeframe() {
        let error = load_text(&config_text("5x", ONE_INSTRUMENT)).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Config(ConfigError::Invalid { ref field, .. }) if field == "timeframe"
        ));
    }

    #[test]
    fn rejects_weight_above_margin_limit() {
        let text = config_text("1d", ONE_INSTRUMENT).replace("weight = 0.99", "weight = 1.5");
        let error = load_text(&text).expect_err("must fail");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
    }

    #[test]
    fn rejects_unknown_strategy_and_unknown_field() {
        let unknown =
            config_text("1d", ONE_INSTRUMENT).replace("name = \"sma_cross\"", "name = \"nope\"");
        let error = load_text(&unknown).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Config(ConfigError::Unsupported { .. })
        ));

        let extra = config_text("1d", ONE_INSTRUMENT).replace("fast = 2", "fast = 2\nmystery = 1");
        let error = load_text(&extra).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Config(ConfigError::ParseToml { .. })
        ));
    }

    #[test]
    fn rejects_non_positive_initial_cash() {
        let text = config_text("1d", ONE_INSTRUMENT)
            .replace("initial_cash = 100000.0", "initial_cash = 0.0");
        let error = load_text(&text).expect_err("must fail");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
    }

    #[test]
    fn loads_risk_legs_and_hashes_them() {
        let text = format!(
            "{}\n[risk.stop_loss]\ntrigger = \"Intrabar\"\ndistance = 0.05\n\
             [risk.take_profit]\ntrigger = \"close\"\ndistance = 0.10\n",
            config_text("1d", ONE_INSTRUMENT)
        );
        let first = load_text(&text).expect("risk config");
        let stop = first.risk.stop_loss.expect("stop");
        let take_profit = first.risk.take_profit.expect("take profit");
        assert_eq!(stop.trigger, Trigger::Intrabar);
        assert!((stop.distance - 0.05).abs() < 1e-12);
        assert_eq!(take_profit.trigger, Trigger::Close);
        assert!((take_profit.distance - 0.10).abs() < 1e-12);

        let changed = text.replace("distance = 0.05", "distance = 0.06");
        let second = load_text(&changed).expect("changed risk config");
        assert_ne!(first.config_hash, second.config_hash);
    }

    #[test]
    fn rejects_invalid_risk_legs() {
        for distance in ["0.0", "1.0", "-0.1"] {
            let text = format!(
                "{}\n[risk.stop_loss]\ntrigger = \"intrabar\"\ndistance = {distance}\n",
                config_text("1d", ONE_INSTRUMENT)
            );
            let error = load_text(&text).expect_err("distance must fail");
            assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
        }
        let text = format!(
            "{}\n[risk.stop_loss]\ntrigger = \"daily\"\ndistance = 0.05\n",
            config_text("1d", ONE_INSTRUMENT)
        );
        let error = load_text(&text).expect_err("trigger must fail");
        assert!(matches!(
            error,
            Error::Config(ConfigError::Unsupported { .. })
        ));
    }

    fn xs_config_text() -> String {
        format!(
            r#"[run]
data = "bars.csv"
initial_cash = 100000.0
start = "2020-01-01"
end = "2020-02-01"
output_dir = "out"
threads = "auto"
timeframe = "1d"

[strategy]
name = "xs_momentum"
lookback = 20
top_k = 2
rebalance_bars = 5
weighting = "inverse_vol"
vol_lookback = 10
vol_floor = 0.0001
gross = 0.9

{ONE_INSTRUMENT}"#
        )
    }

    #[test]
    fn loads_xs_momentum_config_and_hashes_parameters() {
        let text = xs_config_text();
        let first = load_text(&text).expect("xs config");
        assert_eq!(first.strategy.name(), "xs_momentum");
        assert_eq!(first.strategy.warmup_bars(), 20);
        let params = first.strategy.params();
        assert_eq!(params.get("lookback"), Some(&serde_json::json!(20)));
        assert_eq!(params.get("top_k"), Some(&serde_json::json!(2)));
        assert_eq!(params.get("rebalance_bars"), Some(&serde_json::json!(5)));
        assert_eq!(
            params.get("weighting"),
            Some(&serde_json::json!("inverse_vol"))
        );
        assert_eq!(params.get("vol_lookback"), Some(&serde_json::json!(10)));
        assert_eq!(params.get("vol_floor"), Some(&serde_json::json!(0.0001)));
        assert_eq!(params.get("gross"), Some(&serde_json::json!(0.9)));

        let second = load_text(&text).expect("xs config");
        assert_eq!(first.config_hash, second.config_hash);

        let changed = text.replace("vol_floor = 0.0001", "vol_floor = 0.0002");
        let third = load_text(&changed).expect("changed xs config");
        assert_ne!(first.config_hash, third.config_hash);
    }

    #[test]
    fn xs_momentum_equal_weighting_omits_vol_parameters() {
        let text = xs_config_text().replace("weighting = \"inverse_vol\"", "weighting = \"equal\"");
        let config = load_text(&text).expect("equal xs config");
        let params = config.strategy.params();
        assert_eq!(params.get("weighting"), Some(&serde_json::json!("equal")));
        assert!(!params.contains_key("vol_lookback"));
        assert!(!params.contains_key("vol_floor"));
        assert_eq!(config.strategy.warmup_bars(), 20);
    }

    #[test]
    fn rejects_invalid_xs_momentum_fields() {
        for (old, new) in [
            ("lookback = 20", "lookback = 0"),
            ("top_k = 2", "top_k = 0"),
            ("rebalance_bars = 5", "rebalance_bars = 0"),
            ("gross = 0.9", "gross = 0.0"),
            ("vol_lookback = 10", "vol_lookback = 1"),
            ("vol_floor = 0.0001", "vol_floor = 0.0"),
        ] {
            let text = xs_config_text().replace(old, new);
            let error = load_text(&text).expect_err("invalid xs field");
            assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
        }
        let unsupported =
            xs_config_text().replace("weighting = \"inverse_vol\"", "weighting = \"nope\"");
        let error = load_text(&unsupported).expect_err("unknown weighting");
        assert!(matches!(
            error,
            Error::Config(ConfigError::Unsupported { .. })
        ));
        let missing = xs_config_text().replace("lookback = 20\n", "");
        let error = load_text(&missing).expect_err("missing lookback");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
    }

    #[test]
    fn rejects_xs_momentum_gross_above_margin_limit() {
        let text = xs_config_text().replace("gross = 0.9", "gross = 1.5");
        let error = load_text(&text).expect_err("gross above limit");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
    }

    fn with_sweep(body: &str) -> String {
        format!(
            "{}\n[sweep]\nobjective = \"sharpe\"\ntop_k = 3\noos_split = 0.7\n\n[sweep.grid]\n{body}\n",
            config_text("1d", ONE_INSTRUMENT)
        )
    }

    #[test]
    fn loads_sweep_grid_and_applies_overrides() {
        let text = with_sweep("fast = [2, 4]\nslow = [5]");
        let config = load_text(&text).expect("sweep config");
        let sweep = config.sweep.as_ref().expect("sweep");
        assert_eq!(sweep.objective, Objective::Sharpe);
        assert_eq!(sweep.top_k, 3);
        assert_eq!(sweep.grid["fast"].len(), 2);

        let mut strategy = config.strategy.clone();
        strategy
            .apply_param("fast", &serde_json::json!(4))
            .expect("fast");
        strategy
            .apply_param("slow", &serde_json::json!(5))
            .expect("slow");
        let changed = config.with_strategy(strategy).expect("valid strategy");
        assert_ne!(changed.config_hash, config.config_hash);
        assert_eq!(changed.strategy.params()["fast"], 4);
    }

    #[test]
    fn with_strategy_rejects_invalid_combination() {
        let config = load_text(&with_sweep("fast = [2]\nslow = [5]")).expect("config");
        let mut strategy = config.strategy.clone();
        strategy
            .apply_param("fast", &serde_json::json!(60))
            .expect("fast");
        let error = config.with_strategy(strategy).expect_err("must fail");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
    }

    #[test]
    fn rejects_invalid_sweep_sections() {
        for (old, new) in [
            ("top_k = 3", "top_k = 0"),
            ("oos_split = 0.7", "oos_split = 1.5"),
        ] {
            let text = with_sweep("fast = [2]").replace(old, new);
            let error = load_text(&text).expect_err("invalid sweep");
            assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
        }
        let unknown = with_sweep("mystery = [1]");
        let error = load_text(&unknown).expect_err("unknown grid key");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
        let nested = with_sweep("fast = [[1, 2]]");
        let error = load_text(&nested).expect_err("non-scalar grid value");
        assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
    }

    fn with_walk_forward(body: &str) -> String {
        format!(
            "{}\n[walk_forward]\n{body}\n",
            config_text("1d", ONE_INSTRUMENT)
        )
    }

    #[test]
    fn rejects_invalid_walk_forward_sections() {
        for (body, field_name) in [
            (
                "train_bars = 0\ntest_bars = 2\nstep_bars = 2\nwarmup_bars = 1",
                "walk_forward.train_bars",
            ),
            (
                "train_bars = 2\ntest_bars = 0\nstep_bars = 2\nwarmup_bars = 1",
                "walk_forward.test_bars",
            ),
            (
                "train_bars = 2\ntest_bars = 2\nstep_bars = 1\nwarmup_bars = 1",
                "walk_forward.step_bars",
            ),
            (
                "train_bars = 2\ntest_bars = 2\nstep_bars = 2\nwarmup_bars = 0",
                "walk_forward.warmup_bars",
            ),
            (
                "train_bars = 2\ntest_bars = 2\nstep_bars = 2\nembargo_bars = 1\nwarmup_bars = 5",
                "walk_forward.warmup_bars",
            ),
        ] {
            let error = load_text(&with_walk_forward(body)).expect_err("invalid walk_forward");
            let message = error.to_string();
            assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
            assert!(message.contains(field_name), "{message}");
        }
    }

    #[test]
    fn applies_documented_run_defaults() {
        let text = config_text("1d", ONE_INSTRUMENT)
            .replace("start = \"2020-01-01\"\n", "")
            .replace("end = \"2020-02-01\"\n", "")
            .replace("output_dir = \"out\"\n", "")
            .replace("threads = \"auto\"\n", "")
            .replace("memory_limit_gb = 0\n", "");
        let config = load_text(&text).expect("defaults");
        assert_eq!(config.output_dir, PathBuf::from("out"));
        assert!(matches!(config.threads, ThreadSetting::Auto));
        assert_eq!(config.memory_limit_gb, 0.0);
        assert_eq!(config.risk_free_rate, 0.0);
        assert_eq!(config.execution.participation_rate, 0.0);
        assert!(config.risk.stop_loss.is_none());
        assert!(config.risk.take_profit.is_none());
        assert!(config.sweep.is_none());
        assert!(config.walk_forward.is_none());
    }
}
