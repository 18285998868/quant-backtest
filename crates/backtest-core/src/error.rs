use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Data(#[from] DataError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Config(ConfigError::Io { .. } | ConfigError::ParseToml { .. }) => 2,
            Error::Config(_) => 2,
            Error::Data(_) => 3,
            Error::Execution(_) | Error::Report(_) | Error::Io(_) => 1,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("读取配置失败 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("解析配置失败 {path}: {source}")]
    ParseToml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("配置字段 '{field}' 非法: {message}")]
    Invalid { field: String, message: String },
    #[error("不支持的{what} '{value}' (支持: {supported})")]
    Unsupported {
        what: &'static str,
        value: String,
        supported: &'static str,
    },
}

#[derive(Debug, Error)]
pub enum DataError {
    #[error("读取数据文件失败 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("解析 CSV {path} 失败: {message}")]
    Csv { path: PathBuf, message: String },
    #[error("数据文件 {path} 缺少必需列 '{column}'")]
    MissingColumn { path: PathBuf, column: String },
    #[error("数据文件 {path} 第 {row} 行非法: {message}")]
    InvalidRow {
        path: PathBuf,
        row: u64,
        message: String,
    },
    #[error("标的 {symbol} 在 ts={ts} 重复")]
    Duplicate { symbol: String, ts: i64 },
    #[error("标的 {symbol} 在 ts={ts} 乱序:文件必须按 symbol, ts 升序")]
    Unsorted { symbol: String, ts: i64 },
    #[error("标的 {symbol} 在 ts={ts} 的 OHLC 非法: {message}")]
    Ohlc {
        symbol: String,
        ts: i64,
        message: String,
    },
    #[error("数据文件 {path} 为空")]
    Empty { path: PathBuf },
    #[error("标的 {symbol} 时间线与其他标的不一致:期望 ts={expected:?},实际 ts={actual:?}")]
    TimelineMismatch {
        symbol: String,
        expected: Option<i64>,
        actual: Option<i64>,
    },
    #[error("数据中的标的 '{symbol}' 未在 [[instruments]] 中声明")]
    UnknownSymbol { symbol: String },
    #[error("配置中的标的 '{symbol}' 没有数据")]
    MissingSymbolData { symbol: String },
    #[error("标的 {symbol} 在区间 [{start}, {end}) 内没有数据")]
    EmptyRange {
        symbol: String,
        start: i64,
        end: i64,
    },
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("策略输出非法权重:标的 '{symbol}' 权重 {weight} 必须是有限值")]
    InvalidWeight { symbol: String, weight: f64 },
    #[error("标的索引 {symbol_id} 越界 (标的数 {symbols})")]
    InvalidSymbol { symbol_id: u32, symbols: usize },
    #[error("策略目标总敞口 {gross:.6} 超过上限 {limit:.6} (margin_rate={margin_rate})")]
    GrossLimit {
        gross: f64,
        limit: f64,
        margin_rate: f64,
    },
    #[error("ts={ts} 账本不变量被破坏: {message}")]
    Invariant { ts: i64, message: String },
}

#[derive(Debug, Error)]
pub enum ReportError {
    #[error("创建报告目录失败 {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("写入报告失败 {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("序列化报告失败: {message}")]
    Serialize { message: String },
}
