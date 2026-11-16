//! `data-reader` 归档装载。
//!
//! 归档里的时间统一是 bar 开盘时刻;本模块转换成回测契约要求的收盘时刻:
//!
//! - Binance:`ts = close_time + 1`;
//! - Dukascopy:`ts = open_time + timeframe`。
//!
//! 日期范围按开盘时刻向外放宽一个 bar 周期,再交由 `finalize` 做精确的
//! `[start, end)` 过滤,避免跨日/跨周 bar 被归档日期窗口切掉。
//!
//! 规划阶段可按 UTC 月调用 [`load_bars_window`] 只取单月数据;`load_funding`
//! 则一次性装载完整区间的资金费,避免月边界重复或遗漏结算。

use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};

use backtest_core::config::{BinanceMarket, DukascopyPrice, SourceConfig};
use backtest_core::error::{ConfigError, DataError, Error, Result};
use backtest_core::model::{FundingSeries, InstrumentSpec, TimeUnit, Timeframe, Ts};

use crate::{LoadDiagnostics, LoadOptions, RawBar, validate_ohlc};

pub(crate) fn load(
    options: &LoadOptions<'_>,
    source: &SourceConfig,
) -> Result<(Vec<RawBar>, Vec<FundingSeries>, LoadDiagnostics)> {
    match source {
        SourceConfig::Binance(source) => load_binance(options, source),
        SourceConfig::Dukascopy(source) => load_dukascopy(options, source),
    }
}

/// 单月规划窗口的 bar 装载。`options.instruments` 必须恰好包含一个标的。
///
/// Binance 在该月没有任何归档文件时返回空 bar 与零统计,由规划层决定跳过;
/// 其他读取错误仍然返回,避免把损坏归档误当成空月份。
pub(crate) fn load_bars_window(
    options: &LoadOptions<'_>,
    source: &SourceConfig,
) -> Result<(Vec<RawBar>, BarStats)> {
    debug_assert_eq!(options.instruments.len(), 1);
    let spec = &options.instruments[0];
    let duration = options.timeframe.duration_ms();
    let window = date_window(options.start, options.end, duration)?;
    match source {
        SourceConfig::Binance(source) => {
            let interval = binance_interval_checked(options.timeframe)?;
            let (bars, stats) =
                load_binance_bars(source, spec, interval, &window, options.timeframe, true)?;
            Ok((bars, BarStats::Binance(stats)))
        }
        SourceConfig::Dukascopy(source) => {
            let timeframe = dukascopy_timeframe_checked(options.timeframe)?;
            let (bars, stats) = load_dukascopy_bars(source, spec, timeframe, &window, duration)?;
            Ok((bars, BarStats::Dukascopy(stats)))
        }
    }
}

/// 一次性装载完整区间的资金费。Dukascopy 与关闭资金费的 Binance 返回空。
pub(crate) fn load_funding(
    options: &LoadOptions<'_>,
    source: &SourceConfig,
) -> Result<(Vec<FundingSeries>, LoadDiagnostics)> {
    let SourceConfig::Binance(source) = source else {
        return Ok((Vec::new(), LoadDiagnostics::default()));
    };
    if !source.funding {
        return Ok((Vec::new(), LoadDiagnostics::default()));
    }
    let window = date_window(options.start, options.end, options.timeframe.duration_ms())?;
    let mut funding = Vec::with_capacity(options.instruments.len());
    let mut notes = Vec::with_capacity(options.instruments.len());
    for (symbol_id, spec) in options.instruments.iter().enumerate() {
        let (series, stats) = load_binance_funding(source, spec, symbol_id as u32, &window)?;
        funding.push(series);
        notes.push(stats.note(source.market.as_str(), &spec.symbol));
    }
    Ok((funding, LoadDiagnostics { notes }))
}

struct Context<'a> {
    provider: &'static str,
    root: &'a Path,
    symbol: &'a str,
}

impl Context<'_> {
    fn error(&self, message: impl Into<String>) -> Error {
        DataError::Archive {
            provider: self.provider,
            root: self.root.to_path_buf(),
            symbol: self.symbol.to_string(),
            message: message.into(),
        }
        .into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DateWindow {
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
}

fn date_window(start: Option<Ts>, end: Option<Ts>, duration: i64) -> Result<DateWindow> {
    let start = match start {
        Some(value) => {
            let open = value
                .checked_sub(duration)
                .ok_or(DataError::TimestampRange { value })?;
            Some(ms_to_date(open)?)
        }
        None => None,
    };
    let end = match end {
        Some(value) => {
            let last = value
                .checked_sub(1)
                .ok_or(DataError::TimestampRange { value })?;
            Some(ms_to_date(last)?)
        }
        None => None,
    };
    Ok(DateWindow { start, end })
}

fn ms_to_date(value: Ts) -> Result<NaiveDate> {
    DateTime::<Utc>::from_timestamp_millis(value)
        .map(|datetime| datetime.date_naive())
        .ok_or_else(|| DataError::TimestampRange { value }.into())
}

fn binance_interval(timeframe: Timeframe) -> Option<binance_core::Interval> {
    use binance_core::Interval;
    Some(match (timeframe.unit, timeframe.count) {
        (TimeUnit::Minute, 1) => Interval::M1,
        (TimeUnit::Minute, 3) => Interval::M3,
        (TimeUnit::Minute, 5) => Interval::M5,
        (TimeUnit::Minute, 15) => Interval::M15,
        (TimeUnit::Minute, 30) => Interval::M30,
        (TimeUnit::Hour, 1) => Interval::H1,
        (TimeUnit::Hour, 2) => Interval::H2,
        (TimeUnit::Hour, 4) => Interval::H4,
        (TimeUnit::Hour, 6) => Interval::H6,
        (TimeUnit::Hour, 8) => Interval::H8,
        (TimeUnit::Hour, 12) => Interval::H12,
        (TimeUnit::Day, 1) => Interval::D1,
        (TimeUnit::Day, 3) => Interval::D3,
        (TimeUnit::Week, 1) => Interval::W1,
        _ => return None,
    })
}

fn binance_interval_checked(timeframe: Timeframe) -> Result<binance_core::Interval> {
    // 配置入口已经校验过周期;这里的失败只代表内部映射遗漏。
    binance_interval(timeframe).ok_or_else(|| {
        ConfigError::Unsupported {
            what: "数据源周期",
            value: format!("binance {timeframe}"),
            supported: "binance: 1m/3m/5m/15m/30m/1h/2h/4h/6h/8h/12h/1d/3d/1w",
        }
        .into()
    })
}

fn binance_market(market: BinanceMarket) -> binance_core::Market {
    match market {
        BinanceMarket::Spot => binance_core::Market::Spot,
        BinanceMarket::Um => binance_core::Market::Um,
        BinanceMarket::Cm => binance_core::Market::Cm,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BinanceBarStats {
    pub bars: usize,
    pub files_daily: usize,
    pub files_monthly: usize,
    pub rows_skipped: usize,
    pub gaps: usize,
}

impl BinanceBarStats {
    fn add(&mut self, other: Self) {
        self.bars += other.bars;
        self.files_daily += other.files_daily;
        self.files_monthly += other.files_monthly;
        self.rows_skipped += other.rows_skipped;
        self.gaps += other.gaps;
    }

    fn note(self, market: &str, symbol: &str, timeframe: Timeframe) -> String {
        format!(
            "binance {market} {symbol} {timeframe}: {} 根 bar(daily={}, monthly={}, 跳过={}, 缺口={})",
            self.bars, self.files_daily, self.files_monthly, self.rows_skipped, self.gaps
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BinanceFundingStats {
    pub rows: usize,
    pub files_monthly: usize,
    pub rows_skipped: usize,
    pub gaps: usize,
    pub adjusted: usize,
}

impl BinanceFundingStats {
    fn note(self, market: &str, symbol: &str) -> String {
        format!(
            "binance {market} {symbol} 资金费: {} 次结算(monthly={}, 跳过={}, 缺口={}, 亚秒修正={})",
            self.rows, self.files_monthly, self.rows_skipped, self.gaps, self.adjusted
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DukascopyBarStats {
    pub bars: usize,
    pub requested_files: usize,
    pub loaded_files: usize,
    pub missing_files: usize,
    pub rows_invalid: usize,
    pub gap_segments: usize,
    pub missing_bars: i64,
}

impl DukascopyBarStats {
    fn add(&mut self, other: Self) {
        self.bars += other.bars;
        self.requested_files += other.requested_files;
        self.loaded_files += other.loaded_files;
        self.missing_files += other.missing_files;
        self.rows_invalid += other.rows_invalid;
        self.gap_segments += other.gap_segments;
        self.missing_bars = self.missing_bars.saturating_add(other.missing_bars);
    }

    fn note(self, price_type: &str, symbol: &str, timeframe: Timeframe) -> String {
        format!(
            "dukascopy {price_type} {symbol} {timeframe}: {} 根 bar(文件 {}/{}, 缺失={}, 跳过={}, 缺口={} 段/{} 根)",
            self.bars,
            self.loaded_files,
            self.requested_files,
            self.missing_files,
            self.rows_invalid,
            self.gap_segments,
            self.missing_bars
        )
    }
}

/// 多个月窗口累计后的归档 bar 统计。
#[derive(Clone, Copy, Debug)]
pub(crate) enum BarStats {
    Binance(BinanceBarStats),
    Dukascopy(DukascopyBarStats),
}

impl BarStats {
    pub(crate) fn zero(source: &SourceConfig) -> Self {
        match source {
            SourceConfig::Binance(_) => Self::Binance(BinanceBarStats::default()),
            SourceConfig::Dukascopy(_) => Self::Dukascopy(DukascopyBarStats::default()),
        }
    }

    pub(crate) fn add(&mut self, other: Self) {
        match (self, other) {
            (Self::Binance(left), Self::Binance(right)) => left.add(right),
            (Self::Dukascopy(left), Self::Dukascopy(right)) => left.add(right),
            _ => {}
        }
    }

    pub(crate) fn note(self, source: &SourceConfig, symbol: &str, timeframe: Timeframe) -> String {
        match (self, source) {
            (Self::Binance(stats), SourceConfig::Binance(source)) => {
                stats.note(source.market.as_str(), symbol, timeframe)
            }
            (Self::Dukascopy(stats), SourceConfig::Dukascopy(source)) => {
                stats.note(source.price_type.as_str(), symbol, timeframe)
            }
            _ => "归档统计与数据源不匹配".into(),
        }
    }
}

fn load_binance(
    options: &LoadOptions<'_>,
    source: &backtest_core::config::BinanceSource,
) -> Result<(Vec<RawBar>, Vec<FundingSeries>, LoadDiagnostics)> {
    let interval = binance_interval_checked(options.timeframe)?;
    let window = date_window(options.start, options.end, options.timeframe.duration_ms())?;

    let mut bars = Vec::new();
    let mut notes = Vec::new();
    let mut funding = Vec::new();
    for (symbol_id, spec) in options.instruments.iter().enumerate() {
        let (mut symbol_bars, stats) =
            load_binance_bars(source, spec, interval, &window, options.timeframe, false)?;
        bars.append(&mut symbol_bars);
        notes.push(stats.note(source.market.as_str(), &spec.symbol, options.timeframe));
        if source.funding {
            let (series, funding_stats) =
                load_binance_funding(source, spec, symbol_id as u32, &window)?;
            funding.push(series);
            notes.push(funding_stats.note(source.market.as_str(), &spec.symbol));
        }
    }
    Ok((bars, funding, LoadDiagnostics { notes }))
}

fn load_binance_bars(
    source: &backtest_core::config::BinanceSource,
    spec: &InstrumentSpec,
    interval: binance_core::Interval,
    window: &DateWindow,
    timeframe: Timeframe,
    allow_no_files: bool,
) -> Result<(Vec<RawBar>, BinanceBarStats)> {
    let context = Context {
        provider: "binance",
        root: &source.root,
        symbol: &spec.symbol,
    };
    let mut builder =
        binance_core::Config::builder(&source.root, binance_core::Dataset::Klines, &spec.symbol)
            .market(binance_market(source.market))
            .interval(interval);
    if window.start.is_some() || window.end.is_some() {
        builder = builder.date_range(window.start, window.end);
    }
    let config = builder
        .build()
        .map_err(|error| context.error(error.to_string()))?;
    let data = match binance_core::read(&config) {
        Ok(data) => data,
        Err(binance_core::Error::NoFiles { .. }) if allow_no_files => {
            return Ok((Vec::new(), BinanceBarStats::default()));
        }
        Err(error) => return Err(context.error(error.to_string())),
    };

    let table = &data.table;
    let open_time = i64_column(table, "open_time", &context)?;
    let close_time = i64_column(table, "close_time", &context)?;
    let open = f64_column(table, "open", &context)?;
    let high = f64_column(table, "high", &context)?;
    let low = f64_column(table, "low", &context)?;
    let close = f64_column(table, "close", &context)?;
    let volume = f64_column(table, "volume", &context)?;
    let mut bars = Vec::with_capacity(table.len());
    for index in 0..table.len() {
        // 归档 close_time 是开盘时刻 + 周期 - 1ms;回测契约用收盘时刻。
        let ts = close_time[index]
            .checked_add(1)
            .ok_or(DataError::TimestampRange {
                value: close_time[index],
            })?;
        validate_ohlc(
            &spec.symbol,
            ts,
            open[index],
            high[index],
            low[index],
            close[index],
        )?;
        debug_assert_eq!(ts - open_time[index], timeframe.duration_ms());
        bars.push(RawBar {
            symbol: spec.symbol.clone(),
            ts,
            open: open[index],
            high: high[index],
            low: low[index],
            close: close[index],
            volume: volume[index],
        });
    }
    let report = &data.report;
    let stats = BinanceBarStats {
        bars: table.len(),
        files_daily: report.files_daily,
        files_monthly: report.files_monthly,
        rows_skipped: report.rows_skipped,
        gaps: report.gaps,
    };
    Ok((bars, stats))
}

fn load_binance_funding(
    source: &backtest_core::config::BinanceSource,
    spec: &InstrumentSpec,
    symbol_id: u32,
    window: &DateWindow,
) -> Result<(FundingSeries, BinanceFundingStats)> {
    let context = Context {
        provider: "binance",
        root: &source.root,
        symbol: &spec.symbol,
    };
    let mut builder = binance_core::Config::builder(
        &source.root,
        binance_core::Dataset::FundingRate,
        &spec.symbol,
    )
    .market(binance_market(source.market));
    if window.start.is_some() || window.end.is_some() {
        builder = builder.date_range(window.start, window.end);
    }
    let config = builder
        .build()
        .map_err(|error| context.error(error.to_string()))?;
    let data = binance_core::read(&config).map_err(|error| context.error(error.to_string()))?;
    let table = &data.table;
    let calc_time = i64_column(table, "calc_time", &context)?;
    let rate = f64_column(table, "last_funding_rate", &context)?;
    let mut series = FundingSeries {
        symbol: symbol_id,
        ts: Vec::with_capacity(table.len()),
        rate: Vec::with_capacity(table.len()),
    };
    let mut adjusted = 0usize;
    for index in 0..table.len() {
        let ts = normalize_funding_ts(calc_time[index]);
        if ts != calc_time[index] {
            adjusted += 1;
        }
        series.ts.push(ts);
        series.rate.push(rate[index]);
    }
    let report = &data.report;
    let stats = BinanceFundingStats {
        rows: table.len(),
        files_monthly: report.files_monthly,
        rows_skipped: report.rows_skipped,
        gaps: report.gaps,
        adjusted,
    };
    Ok((series, stats))
}

/// 归档的 `calc_time` 偶尔比整点晚 1–2ms;结算语义是整点,向下取整到秒,
/// 避免被分到下一根 bar 而错用下一根收盘价。
fn normalize_funding_ts(ts: Ts) -> Ts {
    ts - ts.rem_euclid(1_000)
}

fn i64_column<'a>(
    table: &'a binance_core::Table,
    name: &str,
    context: &Context<'_>,
) -> Result<&'a [i64]> {
    table
        .i64(name)
        .ok_or_else(|| context.error(format!("归档缺少整数列 {name}")))
}

fn f64_column<'a>(
    table: &'a binance_core::Table,
    name: &str,
    context: &Context<'_>,
) -> Result<&'a [f64]> {
    table
        .f64(name)
        .ok_or_else(|| context.error(format!("归档缺少数值列 {name}")))
}

fn dukascopy_timeframe(timeframe: Timeframe) -> Option<dukascopy_core::Timeframe> {
    Some(match (timeframe.unit, timeframe.count) {
        (TimeUnit::Minute, 1) => dukascopy_core::Timeframe::M1,
        (TimeUnit::Hour, 1) => dukascopy_core::Timeframe::H1,
        (TimeUnit::Day, 1) => dukascopy_core::Timeframe::D1,
        _ => return None,
    })
}

fn dukascopy_timeframe_checked(timeframe: Timeframe) -> Result<dukascopy_core::Timeframe> {
    dukascopy_timeframe(timeframe).ok_or_else(|| {
        ConfigError::Unsupported {
            what: "数据源周期",
            value: format!("dukascopy {timeframe}"),
            supported: "dukascopy: 1m/1h/1d",
        }
        .into()
    })
}

fn dukascopy_price(price: DukascopyPrice) -> dukascopy_core::PriceType {
    match price {
        DukascopyPrice::Bid => dukascopy_core::PriceType::Bid,
        DukascopyPrice::Ask => dukascopy_core::PriceType::Ask,
    }
}

fn load_dukascopy(
    options: &LoadOptions<'_>,
    source: &backtest_core::config::DukascopySource,
) -> Result<(Vec<RawBar>, Vec<FundingSeries>, LoadDiagnostics)> {
    let timeframe = dukascopy_timeframe_checked(options.timeframe)?;
    let duration = options.timeframe.duration_ms();
    let window = date_window(options.start, options.end, duration)?;

    let mut bars = Vec::new();
    let mut notes = Vec::new();
    for spec in options.instruments {
        let (mut symbol_bars, stats) =
            load_dukascopy_bars(source, spec, timeframe, &window, duration)?;
        bars.append(&mut symbol_bars);
        notes.push(stats.note(source.price_type.as_str(), &spec.symbol, options.timeframe));
    }
    Ok((bars, Vec::new(), LoadDiagnostics { notes }))
}

fn load_dukascopy_bars(
    source: &backtest_core::config::DukascopySource,
    spec: &InstrumentSpec,
    timeframe: dukascopy_core::Timeframe,
    window: &DateWindow,
    duration: i64,
) -> Result<(Vec<RawBar>, DukascopyBarStats)> {
    let context = Context {
        provider: "dukascopy",
        root: &source.root,
        symbol: &spec.symbol,
    };
    let config = dukascopy_core::DataConfig::new(&source.root, &spec.symbol, timeframe)
        .and_then(|config| {
            config
                .with_price_type(dukascopy_price(source.price_type))
                .with_date_range(window.start, window.end)
        })
        .map_err(|error| context.error(error.to_string()))?;
    let data = dukascopy_core::load(&config).map_err(|error| context.error(error.to_string()))?;

    let columns = data.columns();
    let mut bars = Vec::with_capacity(columns.len());
    for index in 0..columns.len() {
        // 归档时间是开盘时刻;回测契约用收盘时刻。
        let ts = columns.time()[index]
            .checked_add(duration)
            .ok_or(DataError::TimestampRange {
                value: columns.time()[index],
            })?;
        validate_ohlc(
            &spec.symbol,
            ts,
            columns.open()[index],
            columns.high()[index],
            columns.low()[index],
            columns.close()[index],
        )?;
        bars.push(RawBar {
            symbol: spec.symbol.clone(),
            ts,
            open: columns.open()[index],
            high: columns.high()[index],
            low: columns.low()[index],
            close: columns.close()[index],
            volume: f64::from(columns.volume()[index]),
        });
    }
    let report = data.report();
    let stats = DukascopyBarStats {
        bars: columns.len(),
        requested_files: report.requested_files(),
        loaded_files: report.loaded_files(),
        missing_files: report.missing_files().len(),
        rows_invalid: report.rows_invalid(),
        gap_segments: report.gaps().segments(),
        missing_bars: report.gaps().missing_bars(),
    };
    Ok((bars, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("date")
    }

    #[test]
    fn normalizes_sub_second_funding_timestamps() {
        assert_eq!(normalize_funding_ts(1_704_412_800_001), 1_704_412_800_000);
        assert_eq!(normalize_funding_ts(1_704_412_800_000), 1_704_412_800_000);
        assert_eq!(normalize_funding_ts(0), 0);
    }

    #[test]
    fn widens_start_by_one_bar_and_keeps_end_day() {
        // 2024-01-02T00:00Z 收盘的 1h bar 开盘于 2024-01-01T23:00Z。
        let start = DateTime::<Utc>::from_timestamp_millis(1_704_153_600_000)
            .expect("start")
            .timestamp_millis();
        let end = start + 24 * HOUR;
        let window = date_window(Some(start), Some(end), HOUR).expect("window");
        assert_eq!(window.start, Some(date(2024, 1, 1)));
        assert_eq!(window.end, Some(date(2024, 1, 2)));
    }

    #[test]
    fn unbounded_side_stays_none() {
        let window = date_window(None, None, HOUR).expect("window");
        assert_eq!(
            window,
            DateWindow {
                start: None,
                end: None
            }
        );
        let window = date_window(None, Some(1_704_153_600_000), HOUR).expect("window");
        assert_eq!(window.start, None);
        assert_eq!(window.end, Some(date(2024, 1, 1)));
    }

    #[test]
    fn rejects_unrepresentable_bounds() {
        assert!(date_window(Some(i64::MIN), None, HOUR).is_err());
        assert!(date_window(None, Some(i64::MIN), HOUR).is_err());
    }

    #[test]
    fn maps_supported_binance_timeframes() {
        for (text, expected) in [
            ("1m", binance_core::Interval::M1),
            ("15m", binance_core::Interval::M15),
            ("1h", binance_core::Interval::H1),
            ("12h", binance_core::Interval::H12),
            ("1d", binance_core::Interval::D1),
            ("1w", binance_core::Interval::W1),
        ] {
            let timeframe = Timeframe::parse(text).expect("timeframe");
            assert_eq!(binance_interval(timeframe), Some(expected));
        }
        assert_eq!(binance_interval(Timeframe::parse("2d").unwrap()), None);
    }

    #[test]
    fn maps_supported_dukascopy_timeframes() {
        assert_eq!(
            dukascopy_timeframe(Timeframe::parse("1m").unwrap()),
            Some(dukascopy_core::Timeframe::M1)
        );
        assert_eq!(
            dukascopy_timeframe(Timeframe::parse("1d").unwrap()),
            Some(dukascopy_core::Timeframe::D1)
        );
        assert_eq!(dukascopy_timeframe(Timeframe::parse("4h").unwrap()), None);
    }

    #[test]
    fn accumulates_binance_stats() {
        let mut stats = BinanceBarStats {
            bars: 10,
            files_daily: 1,
            files_monthly: 2,
            rows_skipped: 3,
            gaps: 4,
        };
        stats.add(BinanceBarStats {
            bars: 20,
            files_daily: 5,
            files_monthly: 6,
            rows_skipped: 7,
            gaps: 8,
        });
        assert_eq!(stats.bars, 30);
        assert_eq!(stats.files_daily, 6);
        assert_eq!(stats.files_monthly, 8);
        assert_eq!(stats.rows_skipped, 10);
        assert_eq!(stats.gaps, 12);
    }
}
