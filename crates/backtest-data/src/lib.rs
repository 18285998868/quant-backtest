//! 回测数据装载:规范 CSV 与 `data-reader` 归档。
//!
//! 两种入口最终都归一成同一条 `RawBar` 流,由 `finalize` 统一做区间过滤、
//! 时间线校验与 `data_hash` 计算,因此同一份内容无论来自 CSV 还是归档都得到
//! 相同哈希。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use csv::StringRecord;

use backtest_core::config::{DataInput, SourceConfig};
use backtest_core::error::{ConfigError, DataError, Result};
use backtest_core::model::{BarSeries, FundingSeries, InstrumentSpec, Timeframe, Ts, Universe};

mod archive;
mod hash;

/// 一次装载的数据入口。
#[derive(Clone, Copy, Debug)]
pub enum LoadSource<'a> {
    /// 规范 CSV 文件。
    Csv(&'a Path),
    /// `data-reader` 归档目录。
    Archive(&'a SourceConfig),
}

impl<'a> From<&'a DataInput> for LoadSource<'a> {
    fn from(input: &'a DataInput) -> Self {
        match input {
            DataInput::Csv(path) => Self::Csv(path),
            DataInput::Archive(source) => Self::Archive(source),
        }
    }
}

#[derive(Debug)]
pub struct LoadOptions<'a> {
    pub source: LoadSource<'a>,
    pub timeframe: Timeframe,
    pub instruments: &'a [InstrumentSpec],
    pub start: Option<Ts>,
    pub end: Option<Ts>,
}

/// 装载过程中产生的诊断信息,供 CLI 打印。
#[derive(Clone, Debug, Default)]
pub struct LoadDiagnostics {
    pub notes: Vec<String>,
}

/// 分块回放的数据计划:完整时间线、每标的行数、资金费与内容哈希。
///
/// 计划阶段不保留 OHLCV,内存只与时间线和资金费成正比。
#[derive(Clone, Debug)]
pub struct DataPlan {
    pub timeframe: Timeframe,
    pub specs: Vec<InstrumentSpec>,
    pub timeline: Vec<Ts>,
    pub funding: Vec<FundingSeries>,
    pub counts: Vec<usize>,
    pub data_hash: String,
}

impl DataPlan {
    pub fn len(&self) -> usize {
        self.timeline.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timeline.is_empty()
    }

    pub fn symbol_count(&self) -> usize {
        self.specs.len()
    }

    /// 全量常驻内存估算,口径与 [`Universe::estimated_data_bytes`] 一致。
    pub fn estimated_data_bytes(&self) -> u64 {
        let timeline = self.len() as u64 * std::mem::size_of::<Ts>() as u64;
        let series = self
            .counts
            .iter()
            .map(|count| *count as u64 * 5 * std::mem::size_of::<f64>() as u64)
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

    /// 每多一根 bar 需要为全部标的增加的 OHLCV 字节数。
    pub fn bytes_per_bar(&self) -> u64 {
        self.symbol_count() as u64 * 5 * std::mem::size_of::<f64>() as u64
    }
}

/// 规划数据:流式读取并计算 v2 `data_hash`,不保留 OHLCV。
pub fn plan_universe_reporting(options: &LoadOptions<'_>) -> Result<(DataPlan, LoadDiagnostics)> {
    if options.instruments.is_empty() {
        let path = match options.source {
            LoadSource::Csv(path) => path.to_path_buf(),
            LoadSource::Archive(source) => source.root().to_path_buf(),
        };
        return Err(DataError::Empty { path }.into());
    }
    match options.source {
        LoadSource::Csv(path) => plan_csv(options, path),
        LoadSource::Archive(_) => plan_archive(options),
    }
}

fn plan_csv(options: &LoadOptions<'_>, path: &Path) -> Result<(DataPlan, LoadDiagnostics)> {
    let mut timeline: Vec<Ts> = Vec::new();
    let mut timeline_symbol: Option<usize> = None;
    let mut counts = vec![0usize; options.instruments.len()];
    let mut hasher = hash::DataHasher::new(options.instruments.len());
    scan_csv(
        path,
        options.instruments,
        options.start,
        options.end,
        &mut |symbol_id, bar| {
            if timeline_symbol.is_none() {
                timeline_symbol = Some(symbol_id);
            }
            if timeline_symbol == Some(symbol_id) {
                timeline.push(bar.ts);
            } else if let Some(expected) = timeline.get(counts[symbol_id]) {
                if *expected != bar.ts {
                    return Err(DataError::TimelineMismatch {
                        symbol: bar.symbol.clone(),
                        expected: Some(*expected),
                        actual: Some(bar.ts),
                    }
                    .into());
                }
            } else {
                return Err(DataError::TimelineMismatch {
                    symbol: bar.symbol.clone(),
                    expected: None,
                    actual: Some(bar.ts),
                }
                .into());
            }
            counts[symbol_id] += 1;
            hasher.push(
                symbol_id,
                bar.ts,
                [bar.open, bar.high, bar.low, bar.close, bar.volume],
            );
            Ok(())
        },
    )?;
    if timeline.is_empty() {
        return Err(DataError::Empty {
            path: path.to_path_buf(),
        }
        .into());
    }
    for (index, spec) in options.instruments.iter().enumerate() {
        if counts[index] != timeline.len() {
            return Err(DataError::MissingSymbolData {
                symbol: spec.symbol.clone(),
            }
            .into());
        }
    }
    let funding = Vec::new();
    let data_hash = hasher.finish(options.instruments, &funding);
    Ok((
        DataPlan {
            timeframe: options.timeframe,
            specs: options.instruments.to_vec(),
            timeline,
            funding,
            counts,
            data_hash,
        },
        LoadDiagnostics::default(),
    ))
}

fn plan_archive(options: &LoadOptions<'_>) -> Result<(DataPlan, LoadDiagnostics)> {
    let mut timeline: Vec<Ts> = Vec::new();
    let mut counts = vec![0usize; options.instruments.len()];
    let mut hasher = hash::DataHasher::new(options.instruments.len());
    let mut funding: Vec<FundingSeries> = Vec::new();
    let mut notes = Vec::new();
    for (symbol_id, spec) in options.instruments.iter().enumerate() {
        let single = [spec.clone()];
        let one = LoadOptions {
            source: options.source,
            timeframe: options.timeframe,
            instruments: &single,
            start: options.start,
            end: options.end,
        };
        let (universe, diagnostics) = load_universe_reporting(&one)?;
        notes.extend(diagnostics.notes);
        let series = &universe.series[0];
        if timeline.is_empty() {
            timeline = series.ts.clone();
        } else if series.ts != timeline {
            let index = series
                .ts
                .iter()
                .zip(&timeline)
                .position(|(left, right)| left != right)
                .unwrap_or(series.ts.len().min(timeline.len()));
            return Err(DataError::TimelineMismatch {
                symbol: spec.symbol.clone(),
                expected: timeline.get(index).copied(),
                actual: series.ts.get(index).copied(),
            }
            .into());
        }
        counts[symbol_id] = series.len();
        for index in 0..series.len() {
            hasher.push(
                symbol_id,
                series.ts[index],
                [
                    series.open[index],
                    series.high[index],
                    series.low[index],
                    series.close[index],
                    series.volume[index],
                ],
            );
        }
        if let Some(mut item) = universe.funding.into_iter().next() {
            item.symbol = symbol_id as u32;
            funding.push(item);
        }
    }
    if !funding.is_empty() {
        validate_funding(&funding, options.instruments)?;
    }
    let data_hash = hasher.finish(options.instruments, &funding);
    Ok((
        DataPlan {
            timeframe: options.timeframe,
            specs: options.instruments.to_vec(),
            timeline,
            funding,
            counts,
            data_hash,
        },
        LoadDiagnostics { notes },
    ))
}

/// 按绝对下标加载一块数据。窗口可向前带 `history` 根,供策略读取历史。
pub fn load_window(
    plan: &DataPlan,
    options: &LoadOptions<'_>,
    start_index: usize,
    end_index: usize,
    history: usize,
) -> Result<Universe> {
    if start_index >= end_index || end_index > plan.len() {
        return Err(ConfigError::Invalid {
            field: "chunk".into(),
            message: format!(
                "窗口 [{start_index}, {end_index}) 超出数据范围 [0, {})",
                plan.len()
            ),
        }
        .into());
    }
    let window_start = start_index.saturating_sub(history);
    let start_ts = plan.timeline[window_start];
    let end_ts = plan
        .timeline
        .get(end_index - 1)
        .and_then(|ts| ts.checked_add(1))
        .ok_or(DataError::TimestampRange { value: i64::MAX })?;
    let window_options = LoadOptions {
        source: options.source,
        timeframe: plan.timeframe,
        instruments: &plan.specs,
        start: Some(start_ts),
        end: Some(end_ts),
    };
    let (mut universe, _) = load_universe_reporting(&window_options)?;
    let expected_len = end_index - window_start;
    for (symbol, series) in universe.series.iter().enumerate() {
        if series.len() != expected_len {
            return Err(DataError::TimelineMismatch {
                symbol: plan.specs[symbol].symbol.clone(),
                expected: plan.timeline.get(end_index - 1).copied(),
                actual: series.ts.last().copied(),
            }
            .into());
        }
        for (local, ts) in series.ts.iter().enumerate() {
            if *ts != plan.timeline[window_start + local] {
                return Err(DataError::TimelineMismatch {
                    symbol: plan.specs[symbol].symbol.clone(),
                    expected: plan.timeline.get(window_start + local).copied(),
                    actual: Some(*ts),
                }
                .into());
            }
        }
    }
    universe.timeline = plan.timeline.clone();
    universe.funding = plan.funding.clone();
    universe.data_hash = plan.data_hash.clone();
    universe.base_index = window_start;
    Ok(universe)
}

#[derive(Debug, Clone)]
pub(crate) struct RawBar {
    pub symbol: String,
    pub ts: Ts,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// 只返回数据本身,丢弃诊断信息。
pub fn load_universe(options: &LoadOptions<'_>) -> Result<Universe> {
    load_universe_reporting(options).map(|(universe, _)| universe)
}

/// 装载并返回数据与诊断信息。
pub fn load_universe_reporting(options: &LoadOptions<'_>) -> Result<(Universe, LoadDiagnostics)> {
    if options.instruments.is_empty() {
        let path = match options.source {
            LoadSource::Csv(path) => path.to_path_buf(),
            LoadSource::Archive(source) => source.root().to_path_buf(),
        };
        return Err(DataError::Empty { path }.into());
    }
    let (bars, funding, diagnostics) = match options.source {
        LoadSource::Csv(path) => (
            read_csv(path, options.instruments, options.start, options.end)?,
            Vec::new(),
            LoadDiagnostics::default(),
        ),
        LoadSource::Archive(source) => archive::load(options, source)?,
    };
    finalize(options, bars, funding, diagnostics)
}

fn scan_csv(
    path: &Path,
    instruments: &[InstrumentSpec],
    start: Option<Ts>,
    end: Option<Ts>,
    sink: &mut dyn FnMut(usize, RawBar) -> Result<()>,
) -> Result<()> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_path(path)
        .map_err(|source| DataError::Csv {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
    let headers = reader.headers().map_err(|source| DataError::Csv {
        path: path.to_path_buf(),
        message: source.to_string(),
    })?;
    let columns = Columns::new(headers, path)?;
    let ids: HashMap<&str, usize> = instruments
        .iter()
        .enumerate()
        .map(|(index, spec)| (spec.symbol.as_str(), index))
        .collect();
    let start = start.unwrap_or(i64::MIN);
    let end = end.unwrap_or(i64::MAX);

    let mut last_symbol: Option<String> = None;
    let mut last_ts: Option<Ts> = None;
    for record in reader.records() {
        let record = record.map_err(|source| DataError::Csv {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
        let row = record.position().map(|p| p.line()).unwrap_or(0);
        let bar = parse_record(&record, &columns, path, row)?;
        let symbol_id = *ids
            .get(bar.symbol.as_str())
            .ok_or_else(|| DataError::UnknownSymbol {
                symbol: bar.symbol.clone(),
            })?;
        if let Some(prev_symbol) = &last_symbol {
            if bar.symbol == *prev_symbol {
                if let Some(prev_ts) = last_ts {
                    if bar.ts == prev_ts {
                        return Err(DataError::Duplicate {
                            symbol: bar.symbol,
                            ts: bar.ts,
                        }
                        .into());
                    }
                    if bar.ts < prev_ts {
                        return Err(DataError::Unsorted {
                            symbol: bar.symbol,
                            ts: bar.ts,
                        }
                        .into());
                    }
                }
            } else if bar.symbol.as_str() < prev_symbol.as_str() {
                return Err(DataError::Unsorted {
                    symbol: bar.symbol,
                    ts: bar.ts,
                }
                .into());
            }
        }
        last_ts = Some(bar.ts);
        last_symbol = Some(bar.symbol.clone());
        if bar.ts >= start && bar.ts < end {
            sink(symbol_id, bar)?;
        }
    }
    Ok(())
}

fn read_csv(
    path: &Path,
    instruments: &[InstrumentSpec],
    start: Option<Ts>,
    end: Option<Ts>,
) -> Result<Vec<RawBar>> {
    let mut bars = Vec::new();
    scan_csv(path, instruments, start, end, &mut |_, bar| {
        bars.push(bar);
        Ok(())
    })?;
    if bars.is_empty() {
        if start.is_some() || end.is_some() {
            return Err(DataError::EmptyRange {
                symbol: instruments[0].symbol.clone(),
                start: start.unwrap_or(i64::MIN),
                end: end.unwrap_or(i64::MAX),
            }
            .into());
        }
        return Err(DataError::Empty {
            path: path.to_path_buf(),
        }
        .into());
    }
    Ok(bars)
}

fn finalize(
    options: &LoadOptions<'_>,
    bars: Vec<RawBar>,
    funding: Vec<FundingSeries>,
    diagnostics: LoadDiagnostics,
) -> Result<(Universe, LoadDiagnostics)> {
    if bars.is_empty() {
        return Err(match options.source {
            LoadSource::Csv(path) => DataError::Empty {
                path: path.to_path_buf(),
            },
            LoadSource::Archive(source) => DataError::EmptyArchive {
                provider: source.kind(),
                root: source.root().to_path_buf(),
            },
        }
        .into());
    }

    let start = options.start.unwrap_or(i64::MIN);
    let end = options.end.unwrap_or(i64::MAX);
    let mut by_symbol: BTreeMap<String, Vec<RawBar>> = BTreeMap::new();
    let mut last_by_symbol: BTreeMap<String, Ts> = BTreeMap::new();
    for bar in bars {
        if let Some(previous) = last_by_symbol.get(&bar.symbol) {
            if bar.ts == *previous {
                return Err(DataError::Duplicate {
                    symbol: bar.symbol,
                    ts: bar.ts,
                }
                .into());
            }
            if bar.ts < *previous {
                return Err(DataError::Unsorted {
                    symbol: bar.symbol,
                    ts: bar.ts,
                }
                .into());
            }
        }
        last_by_symbol.insert(bar.symbol.clone(), bar.ts);
        by_symbol.entry(bar.symbol.clone()).or_default().push(bar);
    }

    let mut series = Vec::with_capacity(options.instruments.len());
    for (symbol_id, spec) in options.instruments.iter().enumerate() {
        let Some(item) = by_symbol.remove(&spec.symbol) else {
            return Err(DataError::MissingSymbolData {
                symbol: spec.symbol.clone(),
            }
            .into());
        };
        let item = item
            .into_iter()
            .filter(|bar| bar.ts >= start && bar.ts < end)
            .collect::<Vec<_>>();
        if item.is_empty() {
            return Err(DataError::EmptyRange {
                symbol: spec.symbol.clone(),
                start,
                end,
            }
            .into());
        }
        let mut result = BarSeries {
            symbol: symbol_id as u32,
            ts: Vec::with_capacity(item.len()),
            open: Vec::with_capacity(item.len()),
            high: Vec::with_capacity(item.len()),
            low: Vec::with_capacity(item.len()),
            close: Vec::with_capacity(item.len()),
            volume: Vec::with_capacity(item.len()),
        };
        for bar in item {
            result.ts.push(bar.ts);
            result.open.push(bar.open);
            result.high.push(bar.high);
            result.low.push(bar.low);
            result.close.push(bar.close);
            result.volume.push(bar.volume);
        }
        series.push(result);
    }
    if let Some((symbol, _)) = by_symbol.into_iter().next() {
        return Err(DataError::UnknownSymbol { symbol }.into());
    }

    let timeline = series[0].ts.clone();
    for item in &series[1..] {
        if item.ts.len() != timeline.len() {
            let index = item.ts.len().min(timeline.len());
            return Err(DataError::TimelineMismatch {
                symbol: options.instruments[item.symbol as usize].symbol.clone(),
                expected: timeline.get(index).copied(),
                actual: item.ts.get(index).copied(),
            }
            .into());
        }
        for (index, (expected, actual)) in timeline.iter().zip(&item.ts).enumerate() {
            if expected != actual {
                return Err(DataError::TimelineMismatch {
                    symbol: options.instruments[item.symbol as usize].symbol.clone(),
                    expected: Some(*expected),
                    actual: Some(*actual),
                }
                .into());
            }
            if index > 0 && item.ts[index] <= item.ts[index - 1] {
                return Err(DataError::Unsorted {
                    symbol: options.instruments[item.symbol as usize].symbol.clone(),
                    ts: item.ts[index],
                }
                .into());
            }
        }
    }

    let mut diagnostics = diagnostics;
    let mut funding = funding;
    if !funding.is_empty() {
        validate_funding(&funding, options.instruments)?;
        let first_open = timeline[0]
            .checked_sub(options.timeframe.duration_ms())
            .unwrap_or(i64::MIN);
        let last_ts = timeline.last().copied().ok_or_else(|| DataError::Funding {
            symbol: "-".into(),
            message: "资金费区间过滤需要非空时间线".into(),
        })?;
        for series in &mut funding {
            let mut kept_ts = Vec::with_capacity(series.ts.len());
            let mut kept_rate = Vec::with_capacity(series.rate.len());
            for (ts, rate) in series.ts.iter().zip(&series.rate) {
                if *ts > first_open && *ts <= last_ts {
                    kept_ts.push(*ts);
                    kept_rate.push(*rate);
                }
            }
            series.ts = kept_ts;
            series.rate = kept_rate;
        }
        let rows = funding.iter().map(FundingSeries::len).sum::<usize>();
        diagnostics
            .notes
            .push(format!("资金费: 区间内 {rows} 次结算"));
    }
    let data_hash = compute_data_hash(&series, options.instruments, &funding);
    Ok((
        Universe {
            timeframe: options.timeframe,
            specs: options.instruments.to_vec(),
            series,
            funding,
            timeline,
            data_hash,
            base_index: 0,
        },
        diagnostics,
    ))
}

fn validate_funding(funding: &[FundingSeries], instruments: &[InstrumentSpec]) -> Result<()> {
    if funding.len() != instruments.len() {
        return Err(DataError::Funding {
            symbol: "-".into(),
            message: format!(
                "资金费序列数 {} 与标的数 {} 不一致",
                funding.len(),
                instruments.len()
            ),
        }
        .into());
    }
    for (series, spec) in funding.iter().zip(instruments) {
        if series.ts.len() != series.rate.len() {
            return Err(DataError::Funding {
                symbol: spec.symbol.clone(),
                message: "时间与费率长度不一致".into(),
            }
            .into());
        }
        let mut previous = None;
        for (ts, rate) in series.ts.iter().zip(&series.rate) {
            if !rate.is_finite() {
                return Err(DataError::Funding {
                    symbol: spec.symbol.clone(),
                    message: format!("ts={ts} 的费率 {rate} 不是有限值"),
                }
                .into());
            }
            if previous.is_some_and(|value| *ts <= value) {
                return Err(DataError::Funding {
                    symbol: spec.symbol.clone(),
                    message: format!("ts={ts} 未严格递增"),
                }
                .into());
            }
            previous = Some(*ts);
        }
    }
    Ok(())
}

struct Columns {
    ts: usize,
    symbol: usize,
    open: usize,
    high: usize,
    low: usize,
    close: usize,
    volume: usize,
}

impl Columns {
    fn new(headers: &StringRecord, path: &Path) -> Result<Self> {
        let find = |name: &str| {
            headers
                .iter()
                .position(|header| header.trim_start_matches('\u{feff}').trim() == name)
        };
        let required = |name: &str| -> Result<usize> {
            find(name).ok_or_else(|| {
                DataError::MissingColumn {
                    path: path.to_path_buf(),
                    column: name.to_string(),
                }
                .into()
            })
        };
        Ok(Self {
            ts: required("ts")?,
            symbol: required("symbol")?,
            open: required("open")?,
            high: required("high")?,
            low: required("low")?,
            close: required("close")?,
            volume: required("volume")?,
        })
    }
}

fn parse_record(record: &StringRecord, columns: &Columns, path: &Path, row: u64) -> Result<RawBar> {
    let symbol = record.get(columns.symbol).unwrap_or("").trim().to_string();
    if symbol.is_empty() {
        return Err(DataError::InvalidRow {
            path: path.to_path_buf(),
            row,
            message: "symbol 不能为空".into(),
        }
        .into());
    }
    let ts = parse_ts(record.get(columns.ts).unwrap_or("").trim()).map_err(|message| {
        DataError::InvalidRow {
            path: path.to_path_buf(),
            row,
            message: format!("ts: {message}"),
        }
    })?;
    let open = parse_number(record.get(columns.open), path, row, "open")?;
    let high = parse_number(record.get(columns.high), path, row, "high")?;
    let low = parse_number(record.get(columns.low), path, row, "low")?;
    let close = parse_number(record.get(columns.close), path, row, "close")?;
    let volume = parse_number(record.get(columns.volume), path, row, "volume")?;
    validate_ohlc(&symbol, ts, open, high, low, close)?;
    if !volume.is_finite() || volume < 0.0 {
        return Err(DataError::InvalidRow {
            path: path.to_path_buf(),
            row,
            message: "volume 必须是有限且 >= 0 的数值".into(),
        }
        .into());
    }
    Ok(RawBar {
        symbol,
        ts,
        open,
        high,
        low,
        close,
        volume,
    })
}

pub(crate) fn validate_ohlc(
    symbol: &str,
    ts: Ts,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
) -> Result<()> {
    if !(open.is_finite() && high.is_finite() && low.is_finite() && close.is_finite()) {
        return Err(DataError::Ohlc {
            symbol: symbol.to_string(),
            ts,
            message: "价格必须是有限值".into(),
        }
        .into());
    }
    if open <= 0.0 || high <= 0.0 || low <= 0.0 || close <= 0.0 {
        return Err(DataError::Ohlc {
            symbol: symbol.to_string(),
            ts,
            message: "价格必须 > 0".into(),
        }
        .into());
    }
    if high < open.max(close).max(low) {
        return Err(DataError::Ohlc {
            symbol: symbol.to_string(),
            ts,
            message: format!("high={high} 小于 max(open, close, low)"),
        }
        .into());
    }
    if low > open.min(close).min(high) {
        return Err(DataError::Ohlc {
            symbol: symbol.to_string(),
            ts,
            message: format!("low={low} 大于 min(open, close, high)"),
        }
        .into());
    }
    Ok(())
}

fn parse_number(raw: Option<&str>, path: &Path, row: u64, column: &str) -> Result<f64> {
    let text = raw.unwrap_or("").trim();
    text.parse::<f64>().map_err(|_| {
        DataError::InvalidRow {
            path: path.to_path_buf(),
            row,
            message: format!("{column}='{text}' 不是合法数值"),
        }
        .into()
    })
}

fn parse_ts(raw: &str) -> std::result::Result<Ts, String> {
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(value);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc).timestamp_millis());
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt.and_utc().timestamp_millis());
    }
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        && let Some(dt) = date.and_hms_opt(0, 0, 0)
    {
        return Ok(dt.and_utc().timestamp_millis());
    }
    Err(format!("'{raw}' 不是 epoch 毫秒或 RFC3339"))
}

fn compute_data_hash(
    series: &[BarSeries],
    specs: &[InstrumentSpec],
    funding: &[FundingSeries],
) -> String {
    let mut hasher = hash::DataHasher::new(series.len());
    for (symbol, item) in series.iter().enumerate() {
        for index in 0..item.len() {
            hasher.push(
                symbol,
                item.ts[index],
                [
                    item.open[index],
                    item.high[index],
                    item.low[index],
                    item.close[index],
                    item.volume[index],
                ],
            );
        }
    }
    hasher.finish(specs, funding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::error::{ConfigError, DataError, Error};
    use std::path::PathBuf;

    const HEADER: &str = "ts,symbol,open,high,low,close,volume\n";
    const DAY: i64 = 86_400_000;
    const BASE_TS: i64 = 1_577_836_800_000;

    fn spec(symbol: &str) -> InstrumentSpec {
        InstrumentSpec {
            symbol: symbol.into(),
            asset_class: backtest_core::model::AssetClass::Crypto,
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

    fn write_csv(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write csv");
        path
    }

    fn row(index: i64, symbol: &str, open: f64, high: f64, low: f64, close: f64) -> String {
        format!(
            "{},{symbol},{open},{high},{low},{close},1000\n",
            BASE_TS + index * DAY
        )
    }

    fn load<'a>(path: &'a Path, instruments: &'a [InstrumentSpec]) -> Result<Universe> {
        load_universe(&LoadOptions {
            source: LoadSource::Csv(path),
            timeframe: Timeframe::ONE_DAY,
            instruments,
            start: None,
            end: None,
        })
    }

    #[test]
    fn loads_single_symbol_and_hashes_are_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(1, "AAA", 100.0, 102.0, 99.0, 101.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let specs = [spec("AAA")];

        let first = load(&path, &specs).expect("load");
        let second = load(&path, &specs).expect("load");
        assert_eq!(first.len(), 2);
        assert_eq!(first.symbol_count(), 1);
        assert_eq!(first.data_hash, second.data_hash);
        assert_eq!(first.timeline, first.series[0].ts);
        assert_eq!(first.bar(0, 1).close, 101.0);
    }

    #[test]
    fn rejects_duplicate_timestamps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let error = load(&path, &[spec("AAA")]).expect_err("must fail");
        assert!(matches!(error, Error::Data(DataError::Duplicate { .. })));
    }

    #[test]
    fn rejects_unsorted_timestamps() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(1, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let error = load(&path, &[spec("AAA")]).expect_err("must fail");
        assert!(matches!(error, Error::Data(DataError::Unsorted { .. })));
    }

    #[test]
    fn rejects_invalid_ohlc() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 99.0, 98.0, 101.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let error = load(&path, &[spec("AAA")]).expect_err("must fail");
        assert!(matches!(error, Error::Data(DataError::Ohlc { .. })));
    }

    #[test]
    fn rejects_empty_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_csv(dir.path(), "bars.csv", HEADER);
        let error = load(&path, &[spec("AAA")]).expect_err("must fail");
        assert!(matches!(error, Error::Data(DataError::Empty { .. })));
    }

    #[test]
    fn rejects_timeline_mismatch_between_symbols() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(1, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(0, "BBB", 50.0, 51.0, 49.0, 50.0));
        body.push_str(&row(2, "BBB", 50.0, 51.0, 49.0, 50.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let error = load(&path, &[spec("AAA"), spec("BBB")]).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Data(DataError::TimelineMismatch { .. })
        ));
    }

    #[test]
    fn rejects_unknown_symbol_in_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(0, "BBB", 50.0, 51.0, 49.0, 50.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let error = load(&path, &[spec("AAA")]).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Data(DataError::UnknownSymbol { .. })
        ));
    }

    #[test]
    fn rejects_missing_symbol_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let error = load(&path, &[spec("AAA"), spec("BBB")]).expect_err("must fail");
        assert!(matches!(
            error,
            Error::Data(DataError::MissingSymbolData { .. })
        ));
    }

    #[test]
    fn filters_by_time_range() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(1, "AAA", 100.0, 101.0, 99.0, 100.0));
        body.push_str(&row(2, "AAA", 100.0, 101.0, 99.0, 100.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let universe = load_universe(&LoadOptions {
            source: LoadSource::Csv(&path),
            timeframe: Timeframe::ONE_DAY,
            instruments: &[spec("AAA")],
            start: Some(BASE_TS + DAY),
            end: Some(BASE_TS + 2 * DAY),
        })
        .expect("load");
        assert_eq!(universe.len(), 1);
        assert_eq!(universe.timeline[0], BASE_TS + DAY);
    }

    #[test]
    fn reports_empty_range_for_symbol_without_bars_in_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        body.push_str(&row(0, "AAA", 100.0, 101.0, 99.0, 100.0));
        let path = write_csv(dir.path(), "bars.csv", &body);
        let error = load_universe(&LoadOptions {
            source: LoadSource::Csv(&path),
            timeframe: Timeframe::ONE_DAY,
            instruments: &[spec("AAA")],
            start: Some(BASE_TS + 2 * DAY),
            end: Some(BASE_TS + 3 * DAY),
        })
        .expect_err("must fail");
        assert!(matches!(error, Error::Data(DataError::EmptyRange { .. })));
    }

    fn raw_bar(index: i64, symbol: &str) -> RawBar {
        RawBar {
            symbol: symbol.into(),
            ts: BASE_TS + index * DAY,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 1_000.0,
        }
    }

    fn funding_options<'a>(instruments: &'a [InstrumentSpec]) -> LoadOptions<'a> {
        LoadOptions {
            source: LoadSource::Csv(Path::new("unused.csv")),
            timeframe: Timeframe::ONE_DAY,
            instruments,
            start: None,
            end: None,
        }
    }

    #[test]
    fn funding_filters_to_bar_window_and_changes_hash() {
        let specs = [spec("AAA")];
        let bars = vec![raw_bar(0, "AAA"), raw_bar(1, "AAA")];
        let options = funding_options(&specs);
        let (without, _) = finalize(
            &options,
            bars.clone(),
            Vec::new(),
            LoadDiagnostics::default(),
        )
        .expect("finalize");

        let funding = vec![FundingSeries {
            symbol: 0,
            ts: vec![BASE_TS - DAY, BASE_TS, BASE_TS + DAY, BASE_TS + 10 * DAY],
            rate: vec![0.1, 0.2, 0.3, 0.4],
        }];
        let (with, diagnostics) =
            finalize(&options, bars, funding, LoadDiagnostics::default()).expect("finalize");
        assert_eq!(with.funding[0].ts, vec![BASE_TS, BASE_TS + DAY]);
        assert_ne!(with.data_hash, without.data_hash);
        assert!(diagnostics.notes.iter().any(|note| note.contains("资金费")));
    }

    #[test]
    fn rejects_unsorted_or_non_finite_funding() {
        let specs = [spec("AAA")];
        let options = funding_options(&specs);
        for (ts, rate) in [
            (vec![BASE_TS + DAY, BASE_TS], vec![0.1, 0.2]),
            (vec![BASE_TS], vec![f64::NAN]),
        ] {
            let funding = vec![FundingSeries {
                symbol: 0,
                ts,
                rate,
            }];
            let error = finalize(
                &options,
                vec![raw_bar(0, "AAA"), raw_bar(1, "AAA")],
                funding,
                LoadDiagnostics::default(),
            )
            .expect_err("must fail");
            assert!(matches!(error, Error::Data(DataError::Funding { .. })));
        }
    }

    #[test]
    fn plan_matches_full_load_and_windows_keep_absolute_indexes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        for index in 0..10 {
            body.push_str(&row(index, "AAA", 100.0, 101.0, 99.0, 100.0));
        }
        for index in 0..10 {
            body.push_str(&row(index, "BBB", 200.0, 201.0, 199.0, 200.0));
        }
        let path = write_csv(dir.path(), "bars.csv", &body);
        let specs = [spec("AAA"), spec("BBB")];
        let options = LoadOptions {
            source: LoadSource::Csv(&path),
            timeframe: Timeframe::ONE_DAY,
            instruments: &specs,
            start: None,
            end: None,
        };
        let (plan, _) = plan_universe_reporting(&options).expect("plan");
        let full = load(&path, &specs).expect("load");
        assert_eq!(plan.len(), 10);
        assert_eq!(plan.counts, vec![10, 10]);
        assert_eq!(plan.symbol_count(), 2);
        assert_eq!(plan.data_hash, full.data_hash);
        assert_eq!(plan.estimated_data_bytes(), full.estimated_data_bytes());

        let window = load_window(&plan, &options, 3, 7, 2).expect("window");
        assert_eq!(window.base_index, 1);
        assert_eq!(window.window_start(), 1);
        assert_eq!(window.window_end(), 7);
        assert_eq!(window.len(), 10);
        assert_eq!(window.series[0].len(), 6);
        assert_eq!(window.timeline, full.timeline);
        assert_eq!(window.data_hash, full.data_hash);
        assert!(window.funding.is_empty());
        for index in 1..7 {
            assert_eq!(window.bar(0, index).ts, full.bar(0, index).ts);
            assert_eq!(window.bar(0, index).close, full.bar(0, index).close);
            assert_eq!(window.bar(1, index).ts, full.bar(1, index).ts);
            assert_eq!(window.bar(1, index).close, full.bar(1, index).close);
        }

        let head = load_window(&plan, &options, 0, 2, 5).expect("head");
        assert_eq!(head.base_index, 0);
        assert_eq!(head.series[0].len(), 2);
        let tail = load_window(&plan, &options, 8, 10, 5).expect("tail");
        assert_eq!(tail.base_index, 3);
        assert_eq!(tail.window_start(), 3);
        assert_eq!(tail.window_end(), 10);
        assert_eq!(tail.series[0].len(), 7);
    }

    #[test]
    fn load_window_rejects_out_of_range_chunks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut body = HEADER.to_string();
        for index in 0..4 {
            body.push_str(&row(index, "AAA", 100.0, 101.0, 99.0, 100.0));
        }
        let path = write_csv(dir.path(), "bars.csv", &body);
        let specs = [spec("AAA")];
        let options = LoadOptions {
            source: LoadSource::Csv(&path),
            timeframe: Timeframe::ONE_DAY,
            instruments: &specs,
            start: None,
            end: None,
        };
        let (plan, _) = plan_universe_reporting(&options).expect("plan");
        for (start, end) in [(2, 2), (3, 2), (0, 5)] {
            let error = load_window(&plan, &options, start, end, 0).expect_err("must fail");
            assert!(matches!(error, Error::Config(ConfigError::Invalid { .. })));
        }
    }
}
