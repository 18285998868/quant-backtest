//! `data-reader` 归档装载。
//!
//! 归档里的时间统一是 bar 开盘时刻;本模块转换成回测契约要求的收盘时刻:
//!
//! - Binance:`ts = close_time + 1`;
//! - Dukascopy:`ts = open_time + timeframe`。
//!
//! 日期范围按开盘时刻向外放宽一个 bar 周期,再交由 `finalize` 做精确的
//! `[start, end)` 过滤,避免跨日/跨周 bar 被归档日期窗口切掉。

use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};

use backtest_core::config::{BinanceMarket, DukascopyPrice, SourceConfig};
use backtest_core::error::{ConfigError, DataError, Error, Result};
use backtest_core::model::{TimeUnit, Timeframe, Ts};

use crate::{LoadDiagnostics, LoadOptions, RawBar, validate_ohlc};

pub(crate) fn load(
    options: &LoadOptions<'_>,
    source: &SourceConfig,
) -> Result<(Vec<RawBar>, LoadDiagnostics)> {
    match source {
        SourceConfig::Binance(source) => load_binance(options, source),
        SourceConfig::Dukascopy(source) => load_dukascopy(options, source),
    }
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

fn binance_market(market: BinanceMarket) -> binance_core::Market {
    match market {
        BinanceMarket::Spot => binance_core::Market::Spot,
        BinanceMarket::Um => binance_core::Market::Um,
        BinanceMarket::Cm => binance_core::Market::Cm,
    }
}

fn load_binance(
    options: &LoadOptions<'_>,
    source: &backtest_core::config::BinanceSource,
) -> Result<(Vec<RawBar>, LoadDiagnostics)> {
    // 配置入口已经校验过周期;这里的失败只代表内部映射遗漏。
    let interval = binance_interval(options.timeframe).ok_or_else(|| ConfigError::Unsupported {
        what: "数据源周期",
        value: format!("binance {}", options.timeframe),
        supported: "binance: 1m/3m/5m/15m/30m/1h/2h/4h/6h/8h/12h/1d/3d/1w",
    })?;
    let window = date_window(options.start, options.end, options.timeframe.duration_ms())?;

    let mut bars = Vec::new();
    let mut notes = Vec::new();
    for spec in options.instruments {
        let context = Context {
            provider: "binance",
            root: &source.root,
            symbol: &spec.symbol,
        };
        let mut builder = binance_core::Config::builder(
            &source.root,
            binance_core::Dataset::Klines,
            &spec.symbol,
        )
        .market(binance_market(source.market))
        .interval(interval);
        if window.start.is_some() || window.end.is_some() {
            builder = builder.date_range(window.start, window.end);
        }
        let config = builder
            .build()
            .map_err(|error| context.error(error.to_string()))?;
        let data = binance_core::read(&config).map_err(|error| context.error(error.to_string()))?;

        let table = &data.table;
        let open_time = i64_column(table, "open_time", &context)?;
        let close_time = i64_column(table, "close_time", &context)?;
        let open = f64_column(table, "open", &context)?;
        let high = f64_column(table, "high", &context)?;
        let low = f64_column(table, "low", &context)?;
        let close = f64_column(table, "close", &context)?;
        let volume = f64_column(table, "volume", &context)?;
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
            debug_assert_eq!(ts - open_time[index], options.timeframe.duration_ms());
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
        notes.push(format!(
            "binance {} {} {}: {} 根 bar(daily={}, monthly={}, 跳过={}, 缺口={})",
            source.market.as_str(),
            spec.symbol,
            options.timeframe,
            table.len(),
            report.files_daily,
            report.files_monthly,
            report.rows_skipped,
            report.gaps
        ));
    }
    Ok((bars, LoadDiagnostics { notes }))
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

fn dukascopy_price(price: DukascopyPrice) -> dukascopy_core::PriceType {
    match price {
        DukascopyPrice::Bid => dukascopy_core::PriceType::Bid,
        DukascopyPrice::Ask => dukascopy_core::PriceType::Ask,
    }
}

fn load_dukascopy(
    options: &LoadOptions<'_>,
    source: &backtest_core::config::DukascopySource,
) -> Result<(Vec<RawBar>, LoadDiagnostics)> {
    let timeframe =
        dukascopy_timeframe(options.timeframe).ok_or_else(|| ConfigError::Unsupported {
            what: "数据源周期",
            value: format!("dukascopy {}", options.timeframe),
            supported: "dukascopy: 1m/1h/1d",
        })?;
    let duration = options.timeframe.duration_ms();
    let window = date_window(options.start, options.end, duration)?;

    let mut bars = Vec::new();
    let mut notes = Vec::new();
    for spec in options.instruments {
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
        let data =
            dukascopy_core::load(&config).map_err(|error| context.error(error.to_string()))?;

        let columns = data.columns();
        for index in 0..columns.len() {
            // 归档时间是开盘时刻;回测契约用收盘时刻。
            let ts =
                columns.time()[index]
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
        notes.push(format!(
            "dukascopy {} {} {}: {} 根 bar(文件 {}/{}, 缺失={}, 跳过={}, 缺口={} 段/{} 根)",
            source.price_type.as_str(),
            spec.symbol,
            options.timeframe,
            columns.len(),
            report.loaded_files(),
            report.requested_files(),
            report.missing_files().len(),
            report.rows_invalid(),
            report.gaps().segments(),
            report.gaps().missing_bars()
        ));
    }
    Ok((bars, LoadDiagnostics { notes }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("date")
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
}
