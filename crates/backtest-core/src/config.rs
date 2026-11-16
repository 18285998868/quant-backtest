use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{ConfigError, Result};
use crate::model::{AssetClass, InstrumentSpec, Timeframe, Ts};

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

#[derive(Clone, Debug)]
pub enum StrategyConfig {
    SmaCross {
        fast: usize,
        slow: usize,
        weight: f64,
        allow_short: bool,
    },
}

impl StrategyConfig {
    pub fn name(&self) -> &'static str {
        match self {
            StrategyConfig::SmaCross { .. } => "sma_cross",
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
        }
        map
    }

    pub fn warmup_bars(&self) -> usize {
        match self {
            StrategyConfig::SmaCross { slow, .. } => *slow,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub data_path: PathBuf,
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
    execution: RawExecution,
    #[serde(default)]
    risk: RawRisk,
    strategy: RawStrategy,
    #[serde(default)]
    instruments: Vec<RawInstrument>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRun {
    data: String,
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

fn validate(raw: RawConfig) -> Result<RunConfig> {
    if raw.run.data.trim().is_empty() {
        return Err(field("run.data", "不能为空"));
    }
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
    let timeframe = Timeframe::parse(&raw.run.timeframe)?;
    if timeframe != Timeframe::ONE_DAY {
        return Err(ConfigError::Unsupported {
            what: "周期",
            value: timeframe.to_string(),
            supported: "1d (分钟/小时/周线在 M1 阶段实现)",
        }
        .into());
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
    if risk.stop_loss.is_some() || risk.take_profit.is_some() {
        return Err(ConfigError::Unsupported {
            what: "风控配置",
            value: "止损/止盈".into(),
            supported: "S1 暂不支持,将在 S2 实现",
        }
        .into());
    }

    let strategy = parse_strategy(raw.strategy)?;

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
    let StrategyConfig::SmaCross { weight, .. } = &strategy;
    if *weight > 1.0 / min_margin + 1e-12 {
        return Err(field(
            "strategy.weight",
            &format!(
                "{} 超过保证金允许的单标的权重上限 {:.6}",
                weight,
                1.0 / min_margin
            ),
        ));
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
        data_path: PathBuf::from(raw.run.data),
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
        config_hash,
    })
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
        other => Err(ConfigError::Unsupported {
            what: "策略",
            value: other.to_string(),
            supported: "sma_cross",
        }
        .into()),
    }
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
    fn rejects_unsupported_timeframe() {
        let error = load_text(&config_text("1h", ONE_INSTRUMENT)).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Config(ConfigError::Unsupported { .. })
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
}
