//! 离线 JSON 与单文件 HTML 报告。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};

use backtest_core::analysis::{AnalysisReport, BootstrapStatistic};
use backtest_core::error::{ReportError, Result};
use backtest_core::model::{
    BacktestResult, DeflatedSharpe, DrawdownEpisode, EquityPoint, FillReason, FillRecord,
    FundingSummary, Metrics, MonthlyReturn, RunMeta, Side, SymbolFunding, TradeRecord, Ts,
};
use backtest_core::walkforward::WalkForwardResult;
use chrono::DateTime;
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "1.2";
const ANALYSIS_SCHEMA_VERSION: &str = "1.3";
const MAX_EQUITY_POINTS: usize = 20_000;
const MAX_TRADE_ROWS: usize = 500;
const MAX_SWEEP_ROWS: usize = 100;
const ASSUMPTIONS: &[&str] = &[
    "bar 时间戳为 UTC,bar 区间为 [start, end),收盘价对应 end。",
    "策略在收盘读取信号,下一根可用 bar 开盘成交,不使用未来数据。",
    "手续费按成交名义的万分比加单笔固定费;滑点按单边万分比。",
    "要求 cash >= 0,不做多头融资;空头保证金可用,不计融券费与隔夜利息。",
    "Binance 期货默认按归档资金费结算记录扣减现金(结算时刻对齐到整秒);未启用资金费或非期货数据源不计资金费。",
    "不建模分红送转、强平与保证金追缴。",
    "一次回测只允许一种计价货币;跨资产类别在收益层另行合成。",
    "美股使用复权价,加密与外汇使用原始报价。",
    "缺少 bar 时单标的顺延到下一根可用 bar;多标的不做填补。",
    "多标的同时有订单时同一 bar 先卖后买;同一方向内风险退出优先,再按标的 ID 升序。",
    "截面动量按 lookback 收盘收益选 top_k,方向由动量符号决定;逆波动率用对数收益样本标准差,无效时回退等权并写入 warnings。",
    "止损/止盈按首次入场均价计算;加减仓不重置,回到空仓后再次开仓才重置。",
    "同一 bar 同时触发止损与止盈时按止损成交,并标记 ambiguous_bar。",
    "收盘触发的止损/止盈在下一根开盘成交,不按触发 bar 的收盘价成交。",
    "周期由配置声明;年化因子按 timeframe 与 asset_class 推导,或用 periods_per_year 覆盖。",
    "样本外按时间 70/30 切分:样本内选参,样本外只评估一次;排名相关性只对 IS 前 100 名计算。",
    "walk-forward 的 OOS 收益按窗口测试段复利拼接;训练/测试预热区间不产生订单,也不计入 OOS 收益。",
    "DSR 只校正搜索次数,不校正参数相关性;高度相关网格的有效试验数小于 N,DSR 偏乐观。",
    "walk_forward 为 null 表示本次未运行,不是运行失败。",
];

#[derive(Debug)]
pub struct ReportPaths {
    pub json: PathBuf,
    pub html: PathBuf,
}

/// 报告附加信息。`run` 通常全为 `None`;`sweep`、`walkforward` 与
/// `[analysis]` 启用时填对应段。
#[derive(Clone, Debug, Default)]
pub struct ReportExtras {
    pub sweep: Option<SweepReport>,
    pub walk_forward: Option<WalkForwardResult>,
    /// 统计分析与稳健性检验结果;`None` 表示未启用。
    pub analysis: Option<AnalysisReport>,
}

impl ReportExtras {
    pub fn kind(&self) -> &'static str {
        if self.walk_forward.is_some() {
            "walk_forward"
        } else if self.sweep.is_some() {
            "sweep"
        } else {
            "run"
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SweepCounts {
    pub total: usize,
    pub valid: usize,
    pub skipped_invalid: usize,
    pub warnings: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SweepRow {
    pub rank: usize,
    pub params: BTreeMap<String, Value>,
    pub objective_value: Option<f64>,
    pub metrics: Metrics,
    pub oos_metrics: Option<Metrics>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MarginalPoint {
    pub value: Value,
    pub mean_objective: Option<f64>,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ParamMarginal {
    pub name: String,
    pub points: Vec<MarginalPoint>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HeatmapCell {
    pub x: Value,
    pub y: Value,
    pub mean_objective: Option<f64>,
    pub count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SweepHeatmap {
    pub x_param: String,
    pub y_param: String,
    pub x_values: Vec<Value>,
    pub y_values: Vec<Value>,
    pub cells: Vec<HeatmapCell>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OosReport {
    pub split: f64,
    pub split_index: usize,
    pub warmup_bars: usize,
    pub best_params: BTreeMap<String, Value>,
    pub in_sample: Metrics,
    pub out_sample: Metrics,
    pub rank_correlation: Option<f64>,
    pub sample_size: usize,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SweepReport {
    pub objective: String,
    pub top_k: usize,
    pub counts: SweepCounts,
    pub grid: BTreeMap<String, Vec<Value>>,
    pub rows: Vec<SweepRow>,
    pub marginals: Vec<ParamMarginal>,
    pub heatmap: Option<SweepHeatmap>,
    pub oos: Option<OosReport>,
    pub dsr: Option<DeflatedSharpe>,
    pub sweep_hash: String,
    pub elapsed_ms: u128,
    /// 扫描层面的统计分析结果(PBO 等);未启用时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<AnalysisReport>,
}

#[derive(Serialize)]
struct Costs {
    total_fees: f64,
    total_slippage: f64,
    fills: usize,
    turnover: Option<f64>,
    cost_drag: Option<f64>,
}

#[derive(Serialize)]
struct DownsampleNote {
    original_points: usize,
    kept_points: usize,
    method: &'static str,
}

#[derive(Serialize)]
struct ReportJson<'a> {
    schema_version: &'static str,
    report_kind: &'static str,
    result_hash: String,
    meta: &'a backtest_core::model::RunMeta,
    metrics: &'a Metrics,
    equity: Vec<EquityPoint>,
    underwater: Vec<f64>,
    equity_downsampled: Option<DownsampleNote>,
    drawdowns: &'a [DrawdownEpisode],
    monthly: &'a [MonthlyReturn],
    trades: &'a [TradeRecord],
    costs: Costs,
    funding: Option<&'a FundingSummary>,
    sweep: Option<&'a SweepReport>,
    walk_forward: Option<&'a WalkForwardResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis: Option<&'a AnalysisReport>,
    assumptions: &'static [&'static str],
    warnings: &'a [String],
}

pub fn write_report(
    result: &BacktestResult,
    extras: &ReportExtras,
    output_dir: &Path,
) -> Result<ReportPaths> {
    std::fs::create_dir_all(output_dir).map_err(|source| ReportError::CreateDir {
        path: output_dir.to_path_buf(),
        source,
    })?;
    let json_path = output_dir.join("result.json");
    let html_path = output_dir.join("report.html");
    let result_hash = content_hash(result)?;
    write_json_with_hash(result, extras, &json_path, &result_hash)?;
    write_html_with_hash(result, extras, &html_path, &result_hash)?;
    Ok(ReportPaths {
        json: json_path,
        html: html_path,
    })
}

pub fn write_json(result: &BacktestResult, extras: &ReportExtras, path: &Path) -> Result<()> {
    let result_hash = content_hash(result)?;
    write_json_with_hash(result, extras, path, &result_hash)
}

fn write_json_with_hash(
    result: &BacktestResult,
    extras: &ReportExtras,
    path: &Path,
    result_hash: &str,
) -> Result<()> {
    let (equity, note) = downsample_equity(&result.equity);
    let underwater = equity
        .iter()
        .map(|point| point.drawdown)
        .collect::<Vec<_>>();
    let payload = ReportJson {
        schema_version: if extras.analysis.is_some() {
            ANALYSIS_SCHEMA_VERSION
        } else {
            SCHEMA_VERSION
        },
        report_kind: extras.kind(),
        result_hash: result_hash.to_string(),
        meta: &result.meta,
        metrics: &result.metrics,
        equity,
        underwater,
        equity_downsampled: note,
        drawdowns: &result.drawdowns,
        monthly: &result.monthly,
        trades: &result.trades,
        costs: Costs {
            total_fees: result.metrics.total_fees,
            total_slippage: result.fills.iter().map(|fill| fill.slippage).sum(),
            fills: result.fills.len(),
            turnover: result.metrics.turnover,
            cost_drag: result.metrics.cost_drag,
        },
        funding: result.funding.as_ref(),
        sweep: extras.sweep.as_ref(),
        walk_forward: extras.walk_forward.as_ref(),
        analysis: extras.analysis.as_ref(),
        assumptions: ASSUMPTIONS,
        warnings: &result.warnings,
    };
    let file = std::fs::File::create(path).map_err(|source| ReportError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &payload).map_err(|err| ReportError::Serialize {
        message: err.to_string(),
    })?;
    writer.flush().map_err(|source| ReportError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// 与旧版 `serde_json::to_value` + 排序键 + 删除运行时字段完全一致的流式哈希。
///
/// `serde_json` 默认把对象键存进 `BTreeMap`,旧实现因此按字典序输出。这里
/// 直接按同一顺序序列化,只把大数组逐元素写进哈希器,不再构建整个 `Value` 树。
pub fn content_hash(result: &BacktestResult) -> Result<String> {
    let mut hasher = Sha256::new();
    {
        let mut writer = BufWriter::new(HashWriter {
            hasher: &mut hasher,
        });
        serde_json::to_writer(&mut writer, &CanonicalResult(result)).map_err(|err| {
            ReportError::Serialize {
                message: err.to_string(),
            }
        })?;
        writer.flush().map_err(|err| ReportError::Serialize {
            message: format!("写入内容哈希失败: {err}"),
        })?;
    }
    let mut out = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// 按旧版 `serde_json::Value` 的字典序流式计算规范 JSON 的 SHA-256。
///
/// `serde_json` 默认把对象键存进 `BTreeMap`,旧实现因此按字典序输出。实现
/// [`CanonicalItem`] 的类型按同一顺序逐字段写入哈希器,不构建 `Value` 树或
/// 完整 JSON 字节。
pub fn canonical_hash<T: CanonicalItem + ?Sized>(value: &T) -> Result<String> {
    let mut hasher = Sha256::new();
    {
        let mut writer = BufWriter::new(HashWriter {
            hasher: &mut hasher,
        });
        serde_json::to_writer(&mut writer, &CanonicalRef(value)).map_err(|err| {
            ReportError::Serialize {
                message: err.to_string(),
            }
        })?;
        writer.flush().map_err(|err| ReportError::Serialize {
            message: format!("写入内容哈希失败: {err}"),
        })?;
    }
    let mut out = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

struct HashWriter<'a> {
    hasher: &'a mut Sha256,
}

impl IoWrite for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct CanonicalResult<'a>(&'a BacktestResult);

impl Serialize for CanonicalResult<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let result = self.0;
        let mut map =
            serializer.serialize_map(Some(if result.funding.is_some() { 9 } else { 8 }))?;
        map.serialize_entry("drawdowns", &CanonicalSlice(&result.drawdowns))?;
        map.serialize_entry("equity", &CanonicalSlice(&result.equity))?;
        map.serialize_entry("fills", &CanonicalSlice(&result.fills))?;
        if let Some(funding) = &result.funding {
            map.serialize_entry("funding", &CanonicalRef(funding))?;
        }
        map.serialize_entry("meta", &CanonicalRef(&result.meta))?;
        map.serialize_entry("metrics", &CanonicalRef(&result.metrics))?;
        map.serialize_entry("monthly", &CanonicalSlice(&result.monthly))?;
        map.serialize_entry("trades", &CanonicalSlice(&result.trades))?;
        map.serialize_entry("warnings", &result.warnings)?;
        map.end()
    }
}

/// 按旧版 `Value` 的字典序逐字段写出结构体。
pub trait CanonicalItem {
    /// 将 `self` 写成规范 JSON 片段。
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer;
}

/// 在数组或 `Option` 中包装一个规范序列化项。
#[derive(Debug)]
pub struct CanonicalRef<'a, T: CanonicalItem + ?Sized>(pub &'a T);

impl<T: CanonicalItem + ?Sized> Serialize for CanonicalRef<'_, T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize_canonical(serializer)
    }
}

/// 在数组中包装一组规范序列化项。
#[derive(Debug)]
pub struct CanonicalSlice<'a, T: CanonicalItem>(pub &'a [T]);

impl<T: CanonicalItem> Serialize for CanonicalSlice<'_, T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&CanonicalRef(item))?;
        }
        seq.end()
    }
}

impl CanonicalItem for RunMeta {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("RunMeta", 12)?;
        state.serialize_field("config_hash", &self.config_hash)?;
        state.serialize_field("data_hash", &self.data_hash)?;
        state.serialize_field("first_bar_ts", &self.first_bar_ts)?;
        state.serialize_field("last_bar_ts", &self.last_bar_ts)?;
        state.serialize_field("params", &self.params)?;
        state.serialize_field("requested_end", &self.requested_end)?;
        state.serialize_field("requested_start", &self.requested_start)?;
        state.serialize_field("strategy", &self.strategy)?;
        state.serialize_field("symbols", &self.symbols)?;
        state.serialize_field("timeframe", &self.timeframe)?;
        state.serialize_field("walk_forward_window", &self.walk_forward_window)?;
        state.serialize_field("warnings", &self.warnings)?;
        state.end()
    }
}

impl CanonicalItem for Metrics {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("Metrics", 19)?;
        state.serialize_field("annual_volatility", &self.annual_volatility)?;
        state.serialize_field("cagr", &self.cagr)?;
        state.serialize_field("calmar", &self.calmar)?;
        state.serialize_field("cost_drag", &self.cost_drag)?;
        state.serialize_field("expectancy", &self.expectancy)?;
        state.serialize_field("exposure", &self.exposure)?;
        state.serialize_field("max_drawdown", &self.max_drawdown)?;
        state.serialize_field(
            "max_drawdown_duration_days",
            &self.max_drawdown_duration_days,
        )?;
        state.serialize_field("periods_per_year", &self.periods_per_year)?;
        state.serialize_field("profit_factor", &self.profit_factor)?;
        state.serialize_field("risk_free_rate", &self.risk_free_rate)?;
        state.serialize_field("sharpe", &self.sharpe)?;
        state.serialize_field("sortino", &self.sortino)?;
        state.serialize_field("total_fees", &self.total_fees)?;
        state.serialize_field("total_return", &self.total_return)?;
        state.serialize_field("trades", &self.trades)?;
        state.serialize_field("turnover", &self.turnover)?;
        state.serialize_field("ulcer", &self.ulcer)?;
        state.serialize_field("win_rate", &self.win_rate)?;
        state.end()
    }
}

impl CanonicalItem for FundingSummary {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("FundingSummary", 5)?;
        state.serialize_field("by_symbol", &CanonicalSlice(&self.by_symbol))?;
        state.serialize_field("events", &self.events)?;
        state.serialize_field("paid", &self.paid)?;
        state.serialize_field("received", &self.received)?;
        state.serialize_field("total_cost", &self.total_cost)?;
        state.end()
    }
}

impl CanonicalItem for SymbolFunding {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SymbolFunding", 3)?;
        state.serialize_field("cost", &self.cost)?;
        state.serialize_field("events", &self.events)?;
        state.serialize_field("symbol", &self.symbol)?;
        state.end()
    }
}

impl CanonicalItem for EquityPoint {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("EquityPoint", 5)?;
        state.serialize_field("cash", &self.cash)?;
        state.serialize_field("drawdown", &self.drawdown)?;
        state.serialize_field("equity", &self.equity)?;
        state.serialize_field("gross_exposure", &self.gross_exposure)?;
        state.serialize_field("ts", &self.ts)?;
        state.end()
    }
}

impl CanonicalItem for FillRecord {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("FillRecord", 10)?;
        state.serialize_field("ambiguous_bar", &self.ambiguous_bar)?;
        state.serialize_field("fee", &self.fee)?;
        state.serialize_field("notional", &self.notional)?;
        state.serialize_field("price", &self.price)?;
        state.serialize_field("qty", &self.qty)?;
        state.serialize_field("reason", &self.reason)?;
        state.serialize_field("side", &self.side)?;
        state.serialize_field("slippage", &self.slippage)?;
        state.serialize_field("symbol", &self.symbol)?;
        state.serialize_field("ts", &self.ts)?;
        state.end()
    }
}

impl CanonicalItem for TradeRecord {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("TradeRecord", 12)?;
        state.serialize_field("bars_held", &self.bars_held)?;
        state.serialize_field("entry_price", &self.entry_price)?;
        state.serialize_field("entry_ts", &self.entry_ts)?;
        state.serialize_field("exit_price", &self.exit_price)?;
        state.serialize_field("exit_reason", &self.exit_reason)?;
        state.serialize_field("exit_ts", &self.exit_ts)?;
        state.serialize_field("fees", &self.fees)?;
        state.serialize_field("pnl", &self.pnl)?;
        state.serialize_field("qty", &self.qty)?;
        state.serialize_field("return_pct", &self.return_pct)?;
        state.serialize_field("side", &self.side)?;
        state.serialize_field("symbol", &self.symbol)?;
        state.end()
    }
}

impl CanonicalItem for DrawdownEpisode {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("DrawdownEpisode", 6)?;
        state.serialize_field("depth", &self.depth)?;
        state.serialize_field("duration_days", &self.duration_days)?;
        state.serialize_field("peak_ts", &self.peak_ts)?;
        state.serialize_field("recovered", &self.recovered)?;
        state.serialize_field("recovery_ts", &self.recovery_ts)?;
        state.serialize_field("trough_ts", &self.trough_ts)?;
        state.end()
    }
}

impl CanonicalItem for MonthlyReturn {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("MonthlyReturn", 3)?;
        state.serialize_field("month", &self.month)?;
        state.serialize_field("return_pct", &self.return_pct)?;
        state.serialize_field("year", &self.year)?;
        state.end()
    }
}

impl CanonicalItem for OosReport {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("OosReport", 10)?;
        state.serialize_field("best_params", &self.best_params)?;
        state.serialize_field("in_sample", &CanonicalRef(&self.in_sample))?;
        state.serialize_field("out_sample", &CanonicalRef(&self.out_sample))?;
        state.serialize_field("rank_correlation", &self.rank_correlation)?;
        state.serialize_field("sample_size", &self.sample_size)?;
        state.serialize_field("split", &self.split)?;
        state.serialize_field("split_index", &self.split_index)?;
        state.serialize_field("truncated", &self.truncated)?;
        state.serialize_field("warmup_bars", &self.warmup_bars)?;
        state.serialize_field("warnings", &self.warnings)?;
        state.end()
    }
}

impl CanonicalItem for DeflatedSharpe {
    fn serialize_canonical<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("DeflatedSharpe", 8)?;
        state.serialize_field("expected_max_sharpe", &self.expected_max_sharpe)?;
        state.serialize_field("kurtosis", &self.kurtosis)?;
        state.serialize_field("observations", &self.observations)?;
        state.serialize_field("probability", &self.probability)?;
        state.serialize_field("sharpe_per_period", &self.sharpe_per_period)?;
        state.serialize_field("skewness", &self.skewness)?;
        state.serialize_field("trial_sharpe_variance", &self.trial_sharpe_variance)?;
        state.serialize_field("trials", &self.trials)?;
        state.end()
    }
}

fn downsample_equity(points: &[EquityPoint]) -> (Vec<EquityPoint>, Option<DownsampleNote>) {
    if points.len() <= MAX_EQUITY_POINTS {
        return (points.to_vec(), None);
    }
    let max_drawdown_index = points
        .iter()
        .enumerate()
        .min_by(|left, right| {
            left.1
                .drawdown
                .partial_cmp(&right.1.drawdown)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(index, _)| index)
        .unwrap_or(0);
    let stride = points.len().div_ceil(MAX_EQUITY_POINTS);
    let mut kept = Vec::with_capacity(MAX_EQUITY_POINTS + 3);
    for index in (0..points.len()).step_by(stride) {
        kept.push(points[index].clone());
    }
    for index in [0, points.len() - 1, max_drawdown_index] {
        if !kept.iter().any(|point| point.ts == points[index].ts) {
            kept.push(points[index].clone());
        }
    }
    kept.sort_by_key(|point| point.ts);
    let note = DownsampleNote {
        original_points: points.len(),
        kept_points: kept.len(),
        method: "等间隔下采样,保留首尾与最大回撤点",
    };
    (kept, Some(note))
}

fn report_title(kind: &str) -> &'static str {
    match kind {
        "sweep" => "参数扫描报告",
        "walk_forward" => "Walk-forward 报告",
        _ => "回测报告",
    }
}

pub fn write_html(result: &BacktestResult, extras: &ReportExtras, path: &Path) -> Result<()> {
    let hash = content_hash(result)?;
    write_html_with_hash(result, extras, path, &hash)
}

fn write_html_with_hash(
    result: &BacktestResult,
    extras: &ReportExtras,
    path: &Path,
    hash: &str,
) -> Result<()> {
    let meta = &result.meta;
    let metrics = &result.metrics;
    let kind = extras.kind();
    let (points, note) = downsample_equity(&result.equity);
    let mut html = String::with_capacity(64 * 1024);
    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(
        html,
        "<title>{} - {}</title>",
        report_title(kind),
        escape_html(&meta.strategy)
    );
    html.push_str("<style>\n");
    html.push_str(
        ":root{--bg:#f7f8fa;--panel:#fff;--ink:#1f2933;--muted:#6b7280;--line:#e5e7eb;--accent:#2563eb;--warn:#b45309}\n\
         *{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:14px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI','Noto Sans CJK SC',sans-serif}\n\
         main{max-width:1180px;margin:0 auto;padding:32px 20px 64px}h1{font-size:26px;margin:0 0 4px}h2{font-size:19px;margin:36px 0 12px}h3{font-size:15px;margin:22px 0 8px}\n\
         .sub{color:var(--muted);margin-bottom:24px}.panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:18px;margin:14px 0;overflow-x:auto}\n\
         table{width:100%;border-collapse:collapse}th,td{padding:8px 10px;border-bottom:1px solid var(--line);text-align:left;vertical-align:top}\n\
         th{color:var(--muted);font-weight:600;background:#fafafa}td.num,th.num{text-align:right;font-variant-numeric:tabular-nums}\n\
         .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:10px}\n\
         .kpi{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:14px}.kpi .k{color:var(--muted);font-size:12px}.kpi .v{font-size:20px;font-variant-numeric:tabular-nums}\n\
         .good{color:#047857}.bad{color:#b91c1c}.muted{color:var(--muted)}.warn{color:var(--warn)}\n\
         svg{width:100%;height:auto;display:block}code{font-size:12px;word-break:break-all}\n\
         ul{margin:8px 0 0 20px;padding:0}.scroll{max-height:420px;overflow:auto}\n\
         .heat{border-collapse:separate;border-spacing:2px}.heat td,.heat th{border:0;padding:5px 7px;border-radius:4px;text-align:center;font-variant-numeric:tabular-nums;min-width:52px}\n\
         .heat th{background:#fafafa}.note{color:var(--muted);font-size:12px;margin-top:8px}\n\
         @media print{body{background:#fff}main{max-width:none}.scroll{max-height:none;overflow:visible}}\n",
    );
    html.push_str("</style>\n</head>\n<body>\n<main>\n");
    let _ = writeln!(html, "<h1>{}</h1>", report_title(kind));
    let _ = writeln!(
        html,
        "<div class=\"sub\">策略 {} · 标的 {} · 周期 {} · 生成 {}</div>",
        escape_html(&meta.strategy),
        escape_html(&meta.symbols.join(", ")),
        escape_html(&meta.timeframe),
        escape_html(&meta.generated_at)
    );

    overview(&mut html, metrics, meta, kind);
    metrics_section(&mut html, metrics);
    equity_section(&mut html, &points, note.as_ref());
    monthly_section(&mut html, &result.monthly);
    drawdown_section(&mut html, &result.drawdowns);
    trades_section(&mut html, &result.trades, &meta.symbols);
    costs_section(&mut html, result, hash);
    sensitivity_section(&mut html, extras.sweep.as_ref());
    robustness_section(
        &mut html,
        extras.sweep.as_ref(),
        extras.walk_forward.as_ref(),
        extras.analysis.as_ref(),
    );
    assumptions_section(&mut html, &result.warnings);

    html.push_str("</main>\n</body>\n</html>\n");
    std::fs::write(path, html).map_err(|source| ReportError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn overview(
    html: &mut String,
    metrics: &Metrics,
    meta: &backtest_core::model::RunMeta,
    kind: &str,
) {
    html.push_str("<h2>1. 概览</h2>\n<div class=\"grid\">\n");
    kpi(
        html,
        "总收益",
        pct(metrics.total_return),
        metrics.total_return,
    );
    kpi(html, "CAGR", pct(metrics.cagr), metrics.cagr);
    kpi(
        html,
        "最大回撤",
        pct(metrics.max_drawdown),
        metrics.max_drawdown.map(|v| -v),
    );
    kpi(html, "Sharpe", num(metrics.sharpe, 3), metrics.sharpe);
    kpi(
        html,
        "交易数",
        if metrics.trades == 0 {
            "无交易".into()
        } else {
            metrics.trades.to_string()
        },
        Some(0.0),
    );
    kpi(html, "总费用", money(metrics.total_fees), Some(0.0));
    html.push_str("</div>\n<div class=\"panel\"><table><tbody>\n");
    metric_row(html, "报告类型", kind.into());
    metric_row(html, "策略", escape_html(&meta.strategy));
    metric_row(html, "参数", params_inline(&meta.params));
    metric_row(
        html,
        "区间",
        format!(
            "{} 至 {}",
            meta.first_bar_ts.map(date).unwrap_or_else(|| "—".into()),
            meta.last_bar_ts.map(date).unwrap_or_else(|| "—".into())
        ),
    );
    if let Some(window) = meta.walk_forward_window {
        metric_row(html, "walk-forward 窗口", window.to_string());
    }
    html.push_str("</tbody></table></div>\n");
}

fn metrics_section(html: &mut String, metrics: &Metrics) {
    html.push_str("<h2>2. 核心指标</h2>\n<div class=\"panel\"><table><tbody>\n");
    metric_row(html, "总收益", pct(metrics.total_return));
    metric_row(html, "CAGR", pct(metrics.cagr));
    metric_row(html, "年化波动", pct(metrics.annual_volatility));
    metric_row(html, "Sharpe", num(metrics.sharpe, 4));
    metric_row(html, "Sortino", num(metrics.sortino, 4));
    metric_row(html, "最大回撤", pct(metrics.max_drawdown));
    metric_row(
        html,
        "最长回撤天数",
        metrics
            .max_drawdown_duration_days
            .map(|value| format!("{value:.1}"))
            .unwrap_or_else(|| "—".into()),
    );
    metric_row(html, "Calmar", num(metrics.calmar, 4));
    metric_row(html, "Ulcer", num(metrics.ulcer, 4));
    metric_row(
        html,
        "胜率",
        if metrics.trades == 0 {
            "无交易".into()
        } else {
            pct(metrics.win_rate)
        },
    );
    metric_row(html, "盈亏比", num(metrics.profit_factor, 4));
    metric_row(html, "期望每笔", money_opt(metrics.expectancy));
    metric_row(html, "换手", num(metrics.turnover, 4));
    metric_row(html, "成本拖累", pct(metrics.cost_drag));
    metric_row(html, "敞口", pct(metrics.exposure));
    metric_row(html, "年化因子", metrics.periods_per_year.to_string());
    metric_row(html, "无风险利率", pct(Some(metrics.risk_free_rate)));
    html.push_str("</tbody></table></div>\n");
}

fn equity_section(html: &mut String, points: &[EquityPoint], note: Option<&DownsampleNote>) {
    html.push_str("<h2>3. 净值与回撤</h2>\n<div class=\"panel\">\n");
    html.push_str(&line_svg(
        points,
        |point| point.equity,
        "净值曲线",
        "#2563eb",
        280.0,
        true,
    ));
    html.push_str(&line_svg(
        points,
        |point| point.drawdown * 100.0,
        "水下曲线",
        "#b91c1c",
        180.0,
        false,
    ));
    if let Some(note) = note {
        let _ = writeln!(
            html,
            "<p class=\"note\">原始 {} 点,下采样为 {} 点({})。</p>",
            note.original_points, note.kept_points, note.method
        );
    }
    html.push_str("</div>\n");
}

fn monthly_section(html: &mut String, monthly: &[MonthlyReturn]) {
    html.push_str("<h2>4. 月度收益热力图</h2>\n<div class=\"panel\">\n");
    if monthly.is_empty() {
        html.push_str("<p class=\"muted\">无月度收益</p>\n</div>\n");
        return;
    }
    let mut by_year: BTreeMap<i32, [Option<f64>; 12]> = BTreeMap::new();
    for item in monthly {
        let slot = (item.month.clamp(1, 12) - 1) as usize;
        by_year.entry(item.year).or_insert([None; 12])[slot] = Some(item.return_pct);
    }
    let (min, max) = monthly
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, item| {
            (acc.0.min(item.return_pct), acc.1.max(item.return_pct))
        });
    let mut table = String::from("<table class=\"heat\"><thead><tr><th>年份</th>");
    for month in 1..=12 {
        let _ = write!(table, "<th>{month}月</th>");
    }
    table.push_str("<th>全年</th></tr></thead><tbody>\n");
    for (year, values) in by_year {
        let _ = write!(table, "<tr><th>{year}</th>");
        let mut compounded = 1.0;
        for value in values {
            match value {
                Some(value) => {
                    compounded *= 1.0 + value / 100.0;
                    let _ = write!(
                        table,
                        "<td style=\"background:{}\">{:.1}%</td>",
                        heat_color(Some(value), min, max),
                        value
                    );
                }
                None => table.push_str("<td class=\"muted\">—</td>"),
            }
        }
        let year_return = (compounded - 1.0) * 100.0;
        let _ = writeln!(
            table,
            "<td style=\"background:{}\">{:.1}%</td></tr>",
            heat_color(Some(year_return), min, max),
            year_return
        );
    }
    table.push_str("</tbody></table>\n");
    html.push_str(&table);
    html.push_str("<p class=\"note\">按 UTC 自然月复合 bar 收益;颜色由全表最小/最大月度收益线性映射。</p>\n</div>\n");
}

fn drawdown_section(html: &mut String, drawdowns: &[DrawdownEpisode]) {
    html.push_str("<h2>5. 回撤段</h2>\n<div class=\"panel\">\n");
    if drawdowns.is_empty() {
        html.push_str("<p class=\"muted\">无回撤</p>\n");
    } else {
        html.push_str(
            "<table><thead><tr><th>峰值</th><th>谷值</th><th>恢复</th><th class=\"num\">深度</th><th class=\"num\">天数</th></tr></thead><tbody>\n",
        );
        for item in drawdowns {
            let _ = writeln!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{:.2}%</td><td class=\"num\">{:.1}</td></tr>",
                date(item.peak_ts),
                date(item.trough_ts),
                item.recovery_ts
                    .map(date)
                    .unwrap_or_else(|| "未恢复".into()),
                item.depth * 100.0,
                item.duration_days
            );
        }
        html.push_str("</tbody></table>\n");
    }
    html.push_str("</div>\n");
}

fn trades_section(html: &mut String, trades: &[TradeRecord], symbols: &[String]) {
    html.push_str("<h2>6. 交易明细</h2>\n<div class=\"panel\">\n");
    if trades.is_empty() {
        html.push_str("<p class=\"muted\">无交易</p>\n</div>\n");
        return;
    }
    html.push_str("<div class=\"scroll\"><table><thead><tr><th>标的</th><th>方向</th><th>入场</th><th>出场</th><th class=\"num\">数量</th><th class=\"num\">PnL</th><th class=\"num\">费用</th><th class=\"num\">收益</th><th class=\"num\">持有 bar</th><th>原因</th></tr></thead><tbody>\n");
    for trade in trades.iter().take(MAX_TRADE_ROWS) {
        let symbol = symbols
            .get(trade.symbol as usize)
            .map(String::as_str)
            .unwrap_or("?");
        let class = if trade.pnl < 0.0 {
            "num bad"
        } else {
            "num good"
        };
        let _ = writeln!(
            html,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{:.6}</td><td class=\"{}\">{:.2}</td><td class=\"num\">{:.2}</td><td class=\"num\">{:.2}%</td><td class=\"num\">{}</td><td>{}</td></tr>",
            escape_html(symbol),
            side(trade.side),
            date(trade.entry_ts),
            trade.exit_ts.map(date).unwrap_or_else(|| "—".into()),
            trade.qty,
            class,
            trade.pnl,
            trade.fees,
            trade.return_pct,
            trade.bars_held,
            trade.exit_reason.map(reason).unwrap_or_else(|| "—".into())
        );
    }
    html.push_str("</tbody></table></div>\n");
    if trades.len() > MAX_TRADE_ROWS {
        let _ = writeln!(
            html,
            "<p class=\"note\">仅显示前 {MAX_TRADE_ROWS} 条,共 {} 条;JSON 保留全量。</p>",
            trades.len()
        );
    }
    html.push_str("</div>\n");
}

fn costs_section(html: &mut String, result: &BacktestResult, hash: &str) {
    let meta = &result.meta;
    html.push_str("<h2>7. 成本与元数据</h2>\n<div class=\"panel\"><table><tbody>\n");
    metric_row(
        html,
        "手续费合计",
        money(result.fills.iter().map(|fill| fill.fee).sum()),
    );
    metric_row(
        html,
        "滑点合计",
        money(result.fills.iter().map(|fill| fill.slippage).sum()),
    );
    metric_row(html, "成交笔数", result.fills.len().to_string());
    metric_row(html, "年化换手", num(result.metrics.turnover, 4));
    metric_row(html, "成本拖累", pct(result.metrics.cost_drag));
    if let Some(funding) = &result.funding {
        metric_row(html, "资金费净支出", money(funding.total_cost));
        metric_row(
            html,
            "资金费付出/收到",
            format!("{} / {}", money(funding.paid), money(funding.received)),
        );
        metric_row(html, "资金费结算次数", funding.events.to_string());
    }
    metric_row(
        html,
        "配置哈希",
        format!("<code>{}</code>", escape_html(&meta.config_hash)),
    );
    metric_row(
        html,
        "数据哈希",
        format!("<code>{}</code>", escape_html(&meta.data_hash)),
    );
    metric_row(html, "结果哈希", format!("<code>{hash}</code>"));
    metric_row(html, "线程数", meta.threads.to_string());
    metric_row(html, "引擎版本", escape_html(&meta.engine_version));
    html.push_str("</tbody></table></div>\n");
    if let Some(funding) = &result.funding
        && !funding.by_symbol.is_empty()
    {
        html.push_str(
            "<div class=\"panel\"><h3>资金费明细</h3><table><thead><tr><th>标的</th><th>净支出</th><th>结算次数</th></tr></thead><tbody>\n",
        );
        for item in &funding.by_symbol {
            let symbol = meta
                .symbols
                .get(item.symbol as usize)
                .map(String::as_str)
                .unwrap_or("—");
            let _ = writeln!(
                html,
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(symbol),
                money(item.cost),
                item.events
            );
        }
        html.push_str("</tbody></table></div>\n");
    }
}

fn sensitivity_section(html: &mut String, sweep: Option<&SweepReport>) {
    html.push_str("<h2>8. 参数敏感性</h2>\n");
    let Some(sweep) = sweep else {
        html.push_str("<div class=\"panel\"><p class=\"muted\">本次未运行参数扫描。</p></div>\n");
        return;
    };
    html.push_str("<div class=\"panel\">\n");
    let _ = writeln!(
        html,
        "<p class=\"muted\">目标 {} · 有效 {} / {} 组 · 非法跳过 {} 组 · top_k={} · sweep_hash=<code>{}</code></p>",
        escape_html(&sweep.objective),
        sweep.counts.valid,
        sweep.counts.total,
        sweep.counts.skipped_invalid,
        sweep.top_k,
        escape_html(&sweep.sweep_hash)
    );
    if sweep.rows.is_empty() {
        html.push_str("<p class=\"muted\">无有效组合</p>\n");
    } else {
        let mut params: Vec<&String> = sweep.grid.keys().collect();
        params.sort();
        html.push_str(
            "<h3>Top 组合</h3><div class=\"scroll\"><table><thead><tr><th class=\"num\">排名</th>",
        );
        for name in &params {
            let _ = write!(html, "<th>{}</th>", escape_html(name));
        }
        html.push_str(&format!(
            "<th class=\"num\">{} </th><th class=\"num\">Sharpe</th><th class=\"num\">MaxDD</th>",
            escape_html(&sweep.objective)
        ));
        if sweep.oos.is_some() {
            html.push_str("<th class=\"num\">OOS 目标</th>");
        }
        html.push_str("</tr></thead><tbody>\n");
        for row in sweep.rows.iter().take(MAX_SWEEP_ROWS) {
            let _ = write!(html, "<tr><td class=\"num\">{}</td>", row.rank);
            for name in &params {
                let text = row
                    .params
                    .get(*name)
                    .map(value_text)
                    .unwrap_or_else(|| "—".into());
                let _ = write!(html, "<td>{}</td>", escape_html(&text));
            }
            let oos = row
                .oos_metrics
                .as_ref()
                .and_then(|metrics| objective_value(&sweep.objective, metrics));
            let _ = writeln!(
                html,
                "<td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td>{}",
                num(row.objective_value, 4),
                num(row.metrics.sharpe, 4),
                pct(row.metrics.max_drawdown),
                match oos {
                    Some(_) => format!("<td class=\"num\">{}</td>", num(oos, 4)),
                    None => "<td class=\"num\">—</td>".into(),
                }
            );
        }
        html.push_str("</tbody></table></div>\n");
    }

    if !sweep.marginals.is_empty() {
        html.push_str("<h3>参数边际效应</h3><table><thead><tr><th>参数</th><th>取值</th><th class=\"num\">平均目标</th><th class=\"num\">组合数</th></tr></thead><tbody>\n");
        for marginal in &sweep.marginals {
            for point in &marginal.points {
                let _ = writeln!(
                    html,
                    "<tr><td>{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    escape_html(&marginal.name),
                    escape_html(&value_text(&point.value)),
                    num(point.mean_objective, 4),
                    point.count
                );
            }
        }
        html.push_str("</tbody></table>\n");
    }

    if let Some(heatmap) = &sweep.heatmap {
        html.push_str("<h3>两参数热力图</h3><div class=\"scroll\">");
        html.push_str(&heatmap_table(heatmap));
        html.push_str("</div>\n");
    }
    html.push_str("</div>\n");
}

fn robustness_section(
    html: &mut String,
    sweep: Option<&SweepReport>,
    walk_forward: Option<&WalkForwardResult>,
    analysis: Option<&AnalysisReport>,
) {
    html.push_str("<h2>9. 样本外与稳健性</h2>\n");
    let mut any = false;
    if let Some(oos) = sweep.and_then(|sweep| sweep.oos.as_ref()) {
        any = true;
        oos_block(html, oos);
    }
    let dsr = sweep
        .and_then(|sweep| sweep.dsr.as_ref())
        .or_else(|| walk_forward.and_then(|wf| wf.dsr.as_ref()));
    if let Some(dsr) = dsr {
        any = true;
        dsr_block(html, dsr);
    }
    if let Some(wf) = walk_forward {
        any = true;
        walk_forward_block(html, wf);
    }
    if let Some(analysis) = analysis {
        if analysis.bootstrap.is_some() {
            any = true;
            bootstrap_block(html, analysis);
        }
        if analysis.pbo.is_some() {
            any = true;
            pbo_block(html, analysis);
        }
        if analysis.bootstrap.is_none() && analysis.pbo.is_none() {
            any = true;
            html.push_str("<div class=\"panel\"><h3>统计分析</h3><p class=\"muted\">分析已启用,但没有可用的结果。</p>");
            if !analysis.warnings.is_empty() {
                html.push_str("<ul>\n");
                for warning in &analysis.warnings {
                    let _ = writeln!(html, "<li class=\"warn\">{}</li>", escape_html(warning));
                }
                html.push_str("</ul>\n");
            }
            html.push_str("</div>\n");
        }
    }
    if !any {
        html.push_str("<div class=\"panel\"><p class=\"muted\">本次未运行样本外、DSR、walk-forward 或 bootstrap。</p></div>\n");
    }
}

fn bootstrap_block(html: &mut String, analysis: &AnalysisReport) {
    html.push_str("<div class=\"panel\"><h3>Bootstrap 置信区间</h3>\n");
    let Some(report) = analysis.bootstrap.as_ref() else {
        html.push_str("<p class=\"muted\">分析已启用,但没有可用的 bootstrap 结果。</p>\n");
        if !analysis.warnings.is_empty() {
            html.push_str("<ul>\n");
            for warning in &analysis.warnings {
                let _ = writeln!(html, "<li class=\"warn\">{}</li>", escape_html(warning));
            }
            html.push_str("</ul>\n");
        }
        html.push_str("</div>\n");
        return;
    };
    let _ = writeln!(
        html,
        "<p class=\"muted\">stationary bootstrap · 重采样 {} 次 · 期望块长 {} · 置信度 {:.0}% · 种子 {}</p>",
        report.samples,
        report.block_length,
        report.confidence * 100.0,
        analysis.seed
    );
    if report.statistics.is_empty() {
        html.push_str("<p class=\"muted\">没有可用的统计量。</p>\n");
    } else {
        html.push_str("<table><thead><tr><th>统计量</th><th class=\"num\">点估计</th><th class=\"num\">下限</th><th class=\"num\">上限</th><th class=\"num\">有效样本</th></tr></thead><tbody>\n");
        for stat in &report.statistics {
            let _ = writeln!(
                html,
                "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}/{}</td></tr>",
                bootstrap_label(stat.name),
                bootstrap_value(stat.name, stat.point),
                bootstrap_value(stat.name, stat.lower),
                bootstrap_value(stat.name, stat.upper),
                stat.valid_samples,
                report.samples
            );
        }
        html.push_str("</tbody></table>\n");
    }
    if !analysis.warnings.is_empty() {
        html.push_str("<ul>\n");
        for warning in &analysis.warnings {
            let _ = writeln!(html, "<li class=\"warn\">{}</li>", escape_html(warning));
        }
        html.push_str("</ul>\n");
    }
    let _ = writeln!(
        html,
        "<p class=\"note\">区间用百分位法,只描述单条收益序列的抽样波动,不修复参数搜索的选择偏差。分析哈希 <code>{}</code>。</p>",
        escape_html(&analysis.hash)
    );
    html.push_str("</div>\n");
}

fn bootstrap_label(name: BootstrapStatistic) -> &'static str {
    match name {
        BootstrapStatistic::Mean => "均值(每期)",
        BootstrapStatistic::Volatility => "波动率(每期)",
        BootstrapStatistic::Sharpe => "Sharpe(年化)",
        BootstrapStatistic::Cagr => "CAGR",
        BootstrapStatistic::MaxDrawdown => "最大回撤",
    }
}

fn bootstrap_value(name: BootstrapStatistic, value: Option<f64>) -> String {
    let Some(value) = value else {
        return "—".into();
    };
    match name {
        BootstrapStatistic::Sharpe => num(Some(value), 3),
        BootstrapStatistic::Mean | BootstrapStatistic::Volatility => {
            format!("{:.4}%", value * 100.0)
        }
        BootstrapStatistic::Cagr | BootstrapStatistic::MaxDrawdown => {
            format!("{:.2}%", value * 100.0)
        }
    }
}

fn pbo_block(html: &mut String, analysis: &AnalysisReport) {
    let Some(pbo) = analysis.pbo.as_ref() else {
        return;
    };
    html.push_str("<div class=\"panel\"><h3>回测过拟合概率 PBO</h3>\n");
    let _ = writeln!(
        html,
        "<p class=\"muted\">CSCV 近似 · 时间块 {} · 组合 {} · 试验 {} · 观测 {} 个日收益</p>",
        pbo.blocks, pbo.splits, pbo.trials, pbo.observations
    );
    let label = if pbo.pbo > 0.5 {
        "PBO 高于 50%,样本内最优在样本外大概率失效。"
    } else {
        "PBO 不高于 50%,样本内选择仍可能受随机波动影响。"
    };
    let _ = writeln!(
        html,
        "<p><span class=\"num\">PBO {:.1}%</span> · {}</p>",
        pbo.pbo * 100.0,
        escape_html(label)
    );
    if let Some(best) = &pbo.best_label {
        let _ = writeln!(
            html,
            "<p class=\"muted\">最常被选为样本内最优: <code>{}</code></p>",
            escape_html(best)
        );
    }
    html.push_str(
        "<table><thead><tr><th>logit 分位</th><th class=\"num\">值</th></tr></thead><tbody>\n",
    );
    for (name, value) in [
        ("均值", pbo.logit_mean),
        ("p05", pbo.logit_p05),
        ("p50", pbo.logit_p50),
        ("p95", pbo.logit_p95),
    ] {
        let _ = writeln!(
            html,
            "<tr><td>{name}</td><td class=\"num\">{}</td></tr>",
            num(Some(value), 3)
        );
    }
    html.push_str("</tbody></table>\n");
    if !pbo.warnings.is_empty() {
        html.push_str("<ul>\n");
        for warning in &pbo.warnings {
            let _ = writeln!(html, "<li class=\"warn\">{}</li>", escape_html(warning));
        }
        html.push_str("</ul>\n");
    }
    html.push_str(
        "<p class=\"note\">CSCV 假设时间块可交换;止损、共享账户和路径依赖策略只能近似。当前扫描使用样本内区段日收益,未包含样本外区段。</p>\n",
    );
    html.push_str("</div>\n");
}

fn oos_block(html: &mut String, oos: &OosReport) {
    html.push_str("<div class=\"panel\"><h3>70/30 样本外</h3>\n");
    let _ = writeln!(
        html,
        "<p class=\"muted\">切分 {:.0}% · 切分下标 {} · 预热 {} bar · 样本 {} 组{}</p>",
        oos.split * 100.0,
        oos.split_index,
        oos.warmup_bars,
        oos.sample_size,
        if oos.truncated {
            "(仅 IS 前 100 名)"
        } else {
            ""
        }
    );
    html.push_str("<table><thead><tr><th>指标</th><th class=\"num\">样本内</th><th class=\"num\">样本外</th></tr></thead><tbody>\n");
    let rows: [(&str, Option<f64>, Option<f64>); 8] = [
        (
            "总收益",
            oos.in_sample.total_return,
            oos.out_sample.total_return,
        ),
        ("CAGR", oos.in_sample.cagr, oos.out_sample.cagr),
        ("Sharpe", oos.in_sample.sharpe, oos.out_sample.sharpe),
        ("Sortino", oos.in_sample.sortino, oos.out_sample.sortino),
        (
            "最大回撤",
            oos.in_sample.max_drawdown,
            oos.out_sample.max_drawdown,
        ),
        ("Calmar", oos.in_sample.calmar, oos.out_sample.calmar),
        ("Ulcer", oos.in_sample.ulcer, oos.out_sample.ulcer),
        ("交易数", None, None),
    ];
    for (name, left, right) in rows {
        if name == "交易数" {
            let _ = writeln!(
                html,
                "<tr><td>{name}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                oos.in_sample.trades, oos.out_sample.trades
            );
        } else {
            let _ = writeln!(
                html,
                "<tr><td>{name}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                pct_or_num(name, left),
                pct_or_num(name, right)
            );
        }
    }
    html.push_str("</tbody></table>\n");
    let _ = writeln!(
        html,
        "<p>样本内外排名相关(Spearman):{}</p>",
        match oos.rank_correlation {
            Some(value) => format!("{value:.4}"),
            None => "—".into(),
        }
    );
    for warning in &oos.warnings {
        let _ = writeln!(html, "<p class=\"warn\">{}</p>", escape_html(warning));
    }
    html.push_str("</div>\n");
}

fn pct_or_num(name: &str, value: Option<f64>) -> String {
    match name {
        "Sharpe" | "Sortino" | "Calmar" | "Ulcer" => num(value, 4),
        _ => pct(value),
    }
}

fn dsr_block(html: &mut String, dsr: &DeflatedSharpe) {
    html.push_str("<div class=\"panel\"><h3>Deflated Sharpe Ratio</h3><table><tbody>\n");
    metric_row(html, "DSR", pct(Some(dsr.probability)));
    metric_row(html, "试验次数 N", dsr.trials.to_string());
    metric_row(html, "每期 Sharpe", num(Some(dsr.sharpe_per_period), 6));
    metric_row(html, "SR0(期望最大)", num(Some(dsr.expected_max_sharpe), 6));
    metric_row(
        html,
        "试验 Sharpe 方差",
        num(Some(dsr.trial_sharpe_variance), 6),
    );
    metric_row(html, "收益偏度", num(Some(dsr.skewness), 4));
    metric_row(html, "收益峰度", num(Some(dsr.kurtosis), 4));
    metric_row(html, "观测数 T", dsr.observations.to_string());
    html.push_str("</tbody></table><p class=\"note\">DSR 是概率,不是收益保证;参数高度相关时有效试验数小于 N。</p></div>\n");
}

fn walk_forward_block(html: &mut String, wf: &WalkForwardResult) {
    html.push_str("<div class=\"panel\"><h3>Walk-forward 窗口</h3>\n");
    let _ = writeln!(
        html,
        "<p class=\"muted\">{} · train {} / test {} / step {} / embargo {} / warmup {} · 目标 {} · 耗时 {} ms</p>",
        escape_html(&format!("{:?}", wf.mode)),
        wf.train_bars,
        wf.test_bars,
        wf.step_bars,
        wf.embargo_bars,
        wf.warmup_bars,
        escape_html(&wf.objective),
        wf.elapsed_ms
    );
    html.push_str("<div class=\"scroll\"><table><thead><tr><th class=\"num\">窗口</th><th>训练区间</th><th>测试区间</th><th>最优参数</th><th class=\"num\">IS 目标</th><th class=\"num\">OOS 目标</th></tr></thead><tbody>\n");
    for window in &wf.windows {
        let is_value = objective_value(&wf.objective, &window.is_metrics);
        let oos_value = objective_value(&wf.objective, &window.oos_metrics);
        let _ = writeln!(
            html,
            "<tr><td class=\"num\">{}</td><td>[{}, {})</td><td>[{}, {})</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
            window.index,
            window.train_start,
            window.train_end,
            window.test_start,
            window.test_end,
            params_inline(&window.best_params),
            num(is_value, 4),
            num(oos_value, 4)
        );
    }
    html.push_str("</tbody></table></div>\n");

    if !wf.oos_equity.is_empty() {
        html.push_str("<h3>OOS 拼接净值</h3>\n");
        html.push_str(&line_svg(
            &wf.oos_equity,
            |point| point.equity,
            "OOS 拼接净值",
            "#0f766e",
            240.0,
            true,
        ));
    }
    if !wf.param_stability.is_empty() {
        html.push_str("<h3>参数稳定性</h3>\n");
        html.push_str("<table><thead><tr><th>参数</th>");
        for window in &wf.windows {
            let _ = write!(html, "<th class=\"num\">窗口 {}</th>", window.index);
        }
        html.push_str("</tr></thead><tbody>\n");
        for series in &wf.param_stability {
            let _ = write!(html, "<tr><td>{}</td>", escape_html(&series.name));
            for value in &series.values {
                let _ = write!(
                    html,
                    "<td class=\"num\">{}</td>",
                    escape_html(&value_text(value))
                );
            }
            html.push_str("</tr>\n");
            if let Some(svg) = param_svg(series, wf.windows.len()) {
                let _ = writeln!(
                    html,
                    "<tr><td colspan=\"{}\">{svg}</td></tr>",
                    wf.windows.len() + 1
                );
            }
        }
        html.push_str("</tbody></table>\n");
    }
    if !wf.skipped_windows.is_empty() {
        html.push_str("<h3>跳过的窗口</h3><ul>\n");
        for item in &wf.skipped_windows {
            let _ = writeln!(html, "<li class=\"warn\">{}</li>", escape_html(item));
        }
        html.push_str("</ul>\n");
    }
    if !wf.gaps.is_empty() {
        html.push_str("<h3>窗口空档</h3><ul>\n");
        for gap in &wf.gaps {
            let _ = writeln!(
                html,
                "<li>窗口 {} 之后缺少 bar [{}, {})</li>",
                gap.after_window, gap.start_index, gap.end_index
            );
        }
        html.push_str("</ul>\n");
    }
    html.push_str("</div>\n");
}

fn assumptions_section(html: &mut String, warnings: &[String]) {
    html.push_str("<h2>10. 假设与限制</h2>\n<div class=\"panel\"><ul>\n");
    for item in ASSUMPTIONS {
        let _ = writeln!(html, "<li>{}</li>", escape_html(item));
    }
    html.push_str("</ul></div>\n");
    html.push_str("<div class=\"panel\"><h3>警告</h3>\n");
    if warnings.is_empty() {
        html.push_str("<p class=\"muted\">无</p>\n");
    } else {
        html.push_str("<ul>\n");
        for item in warnings {
            let _ = writeln!(html, "<li class=\"warn\">{}</li>", escape_html(item));
        }
        html.push_str("</ul>\n");
    }
    html.push_str("</div>\n");
}

fn heatmap_table(heatmap: &SweepHeatmap) -> String {
    let (min, max) = heatmap
        .cells
        .iter()
        .filter_map(|cell| cell.mean_objective)
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, value| {
            (acc.0.min(value), acc.1.max(value))
        });
    let mut table = String::from("<table class=\"heat\"><thead><tr><th>");
    let _ = write!(
        table,
        "{} \\ {}</th>",
        escape_html(&heatmap.y_param),
        escape_html(&heatmap.x_param)
    );
    for x in &heatmap.x_values {
        let _ = write!(table, "<th>{}</th>", escape_html(&value_text(x)));
    }
    table.push_str("</tr></thead><tbody>\n");
    for y in &heatmap.y_values {
        let _ = write!(table, "<tr><th>{}</th>", escape_html(&value_text(y)));
        for x in &heatmap.x_values {
            let cell = heatmap
                .cells
                .iter()
                .find(|cell| &cell.x == x && &cell.y == y);
            match cell.and_then(|cell| cell.mean_objective) {
                Some(value) => {
                    let _ = write!(
                        table,
                        "<td style=\"background:{}\">{:.3}</td>",
                        heat_color(Some(value), min, max),
                        value
                    );
                }
                None => table.push_str("<td class=\"muted\">—</td>"),
            }
        }
        table.push_str("</tr>\n");
    }
    table.push_str("</tbody></table>\n");
    table
}

fn heat_color(value: Option<f64>, min: f64, max: f64) -> String {
    let Some(value) = value else {
        return "#f3f4f6".into();
    };
    let t = if (max - min).abs() < 1e-12 {
        0.5
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    };
    let (r, g, b) = if t < 0.5 {
        let u = t * 2.0;
        lerp_rgb((185.0, 28.0, 28.0), (250.0, 204.0, 21.0), u)
    } else {
        let u = (t - 0.5) * 2.0;
        lerp_rgb((250.0, 204.0, 21.0), (4.0, 120.0, 87.0), u)
    };
    format!("rgb({r:.0},{g:.0},{b:.0})")
}

fn lerp_rgb(from: (f64, f64, f64), to: (f64, f64, f64), t: f64) -> (f64, f64, f64) {
    (
        from.0 + (to.0 - from.0) * t,
        from.1 + (to.1 - from.1) * t,
        from.2 + (to.2 - from.2) * t,
    )
}

fn line_svg(
    points: &[EquityPoint],
    value: impl Fn(&EquityPoint) -> f64,
    label: &str,
    color: &str,
    height: f64,
    show_dates: bool,
) -> String {
    if points.len() < 2 {
        return format!("<p class=\"muted\">{label}:点不足,无法绘图</p>\n");
    }
    let values: Vec<f64> = points.iter().map(&value).collect();
    let (min, max) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, v| {
            (acc.0.min(*v), acc.1.max(*v))
        });
    let range = (max - min).max(max.abs() * 0.01).max(1e-9);
    let top = 20.0;
    let bottom = height - 28.0;
    let mut svg = String::new();
    let _ = writeln!(
        svg,
        "<svg viewBox=\"0 0 960 {height:.0}\" role=\"img\" aria-label=\"{label}\">"
    );
    let _ = writeln!(
        svg,
        "<rect x=\"0\" y=\"0\" width=\"960\" height=\"{height:.0}\" fill=\"#fff\"/>"
    );
    for i in 0..=4 {
        let y = top + i as f64 * (bottom - top) / 4.0;
        let _ = writeln!(
            svg,
            "<line x1=\"56\" y1=\"{y:.1}\" x2=\"944\" y2=\"{y:.1}\" stroke=\"#eef0f3\"/>"
        );
    }
    let mut path = String::new();
    for (index, point) in values.iter().enumerate() {
        let x = 56.0 + index as f64 / (values.len() - 1) as f64 * 888.0;
        let y = bottom - (point - min) / range * (bottom - top);
        if index > 0 {
            path.push(' ');
        }
        let _ = write!(path, "{x:.1},{y:.1}");
    }
    let _ = writeln!(
        svg,
        "<polyline fill=\"none\" stroke=\"{color}\" stroke-width=\"2\" points=\"{path}\"/>"
    );
    let _ = writeln!(
        svg,
        "<text x=\"56\" y=\"14\" font-size=\"12\" fill=\"#6b7280\">{max:.4}</text>"
    );
    let _ = writeln!(
        svg,
        "<text x=\"56\" y=\"{:.1}\" font-size=\"12\" fill=\"#6b7280\">{min:.4}</text>",
        height - 22.0
    );
    if show_dates {
        let _ = writeln!(
            svg,
            "<text x=\"56\" y=\"{:.1}\" font-size=\"12\" fill=\"#6b7280\">{}</text>",
            height - 4.0,
            date(points[0].ts)
        );
        let _ = writeln!(
            svg,
            "<text x=\"944\" y=\"{:.1}\" font-size=\"12\" fill=\"#6b7280\" text-anchor=\"end\">{}</text>",
            height - 4.0,
            date(points[points.len() - 1].ts)
        );
    }
    svg.push_str("</svg>\n");
    svg
}

fn param_svg(series: &backtest_core::walkforward::ParamSeries, windows: usize) -> Option<String> {
    if windows < 2 {
        return None;
    }
    let values: Vec<f64> = series
        .values
        .iter()
        .filter_map(|value| value.as_f64())
        .collect();
    if values.len() != series.values.len() {
        return None;
    }
    let (min, max) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, v| {
            (acc.0.min(*v), acc.1.max(*v))
        });
    let range = (max - min).max(max.abs() * 0.01).max(1e-9);
    let mut svg =
        String::from("<svg viewBox=\"0 0 960 90\" role=\"img\" aria-label=\"参数稳定性\">");
    svg.push_str("<rect x=\"0\" y=\"0\" width=\"960\" height=\"90\" fill=\"#fff\"/>");
    let mut path = String::new();
    for (index, value) in values.iter().enumerate() {
        let x = 56.0 + index as f64 / (values.len() - 1) as f64 * 848.0;
        let y = 70.0 - (value - min) / range * 50.0;
        if index > 0 {
            path.push(' ');
        }
        let _ = write!(path, "{x:.1},{y:.1}");
    }
    let _ = writeln!(
        svg,
        "<polyline fill=\"none\" stroke=\"#7c3aed\" stroke-width=\"2\" points=\"{path}\"/>"
    );
    let _ = writeln!(
        svg,
        "<text x=\"56\" y=\"14\" font-size=\"12\" fill=\"#6b7280\">{}</text>",
        escape_html(&series.name)
    );
    svg.push_str("</svg>\n");
    Some(svg)
}

fn objective_value(name: &str, metrics: &Metrics) -> Option<f64> {
    match name {
        "sharpe" => metrics.sharpe,
        "cagr" => metrics.cagr,
        "calmar" => metrics.calmar,
        _ => None,
    }
}

fn kpi(html: &mut String, name: &str, value: String, raw: Option<f64>) {
    let class = match raw {
        Some(value) if value < 0.0 => "v bad",
        Some(_) => "v good",
        None => "v",
    };
    let _ = writeln!(
        html,
        "<div class=\"kpi\"><div class=\"k\">{}</div><div class=\"{}\">{}</div></div>",
        escape_html(name),
        class,
        value
    );
}

fn metric_row(html: &mut String, name: &str, value: String) {
    let _ = writeln!(
        html,
        "<tr><th>{}</th><td>{}</td></tr>",
        escape_html(name),
        value
    );
}

fn params_inline(params: &BTreeMap<String, Value>) -> String {
    if params.is_empty() {
        return "—".into();
    }
    params
        .iter()
        .map(|(name, value)| format!("{}={}", escape_html(name), escape_html(&value_text(value))))
        .collect::<Vec<_>>()
        .join(", ")
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn pct(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.2}%", value * 100.0))
        .unwrap_or_else(|| "—".into())
}

fn num(value: Option<f64>, decimals: usize) -> String {
    value
        .map(|value| format!("{value:.decimals$}"))
        .unwrap_or_else(|| "—".into())
}

fn money(value: f64) -> String {
    format!("{value:.2}")
}

fn money_opt(value: Option<f64>) -> String {
    value.map(money).unwrap_or_else(|| "—".into())
}

fn date(ts: Ts) -> String {
    DateTime::from_timestamp_millis(ts)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn side(value: Side) -> &'static str {
    match value {
        Side::Buy => "买入",
        Side::Sell => "卖出",
    }
}

fn reason(value: FillReason) -> String {
    match value {
        FillReason::Entry => "开仓",
        FillReason::Rebalance => "调仓",
        FillReason::StopLoss => "止损",
        FillReason::TakeProfit => "止盈",
        FillReason::Close => "平仓",
    }
    .to_string()
}

fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 检查报告 JSON 的必填字段、类型与 null 规则。
pub fn validate_report_json(value: &serde_json::Value) -> std::result::Result<(), String> {
    let object = value.as_object().ok_or("顶层必须是对象")?;
    for key in [
        "schema_version",
        "report_kind",
        "result_hash",
        "meta",
        "metrics",
        "equity",
        "underwater",
        "drawdowns",
        "monthly",
        "trades",
        "costs",
        "assumptions",
        "warnings",
    ] {
        if !object.contains_key(key) {
            return Err(format!("缺少字段 {key}"));
        }
    }
    let schema = object["schema_version"]
        .as_str()
        .ok_or("schema_version 必须是字符串")?;
    let analysis = object.get("analysis");
    match schema {
        SCHEMA_VERSION => {
            if analysis.is_some_and(|value| !value.is_null()) {
                return Err("schema 1.2 不允许 analysis 字段".into());
            }
        }
        ANALYSIS_SCHEMA_VERSION => {
            let analysis = analysis.ok_or("schema 1.3 缺少 analysis 字段")?;
            if !analysis.is_null() {
                validate_analysis_json(analysis)?;
            }
        }
        other => return Err(format!("不支持的 schema_version: {other}")),
    }
    if object["result_hash"].as_str().map(str::len) != Some(64) {
        return Err("result_hash 必须是 64 位十六进制".into());
    }
    let equity = object["equity"].as_array().ok_or("equity 必须是数组")?;
    let underwater = object["underwater"]
        .as_array()
        .ok_or("underwater 必须是数组")?;
    if equity.len() != underwater.len() {
        return Err("underwater 长度必须与 equity 一致".into());
    }
    for point in equity {
        let item = point.as_object().ok_or("equity 元素必须是对象")?;
        for key in ["ts", "equity", "cash", "gross_exposure", "drawdown"] {
            if !item.get(key).is_some_and(serde_json::Value::is_number) {
                return Err(format!("equity 元素缺少数值字段 {key}"));
            }
        }
    }
    for value in underwater {
        if !value.is_number() {
            return Err("underwater 元素必须是数值".into());
        }
    }
    let metrics = object["metrics"].as_object().ok_or("metrics 必须是对象")?;
    for key in [
        "total_return",
        "cagr",
        "annual_volatility",
        "sharpe",
        "sortino",
        "max_drawdown",
        "max_drawdown_duration_days",
        "calmar",
        "ulcer",
        "win_rate",
        "profit_factor",
        "expectancy",
        "turnover",
        "cost_drag",
        "exposure",
    ] {
        match metrics.get(key) {
            Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Number(_)) => {}
            Some(_) => return Err(format!("metrics.{key} 必须是数值或 null")),
            None => return Err(format!("metrics 缺少字段 {key}")),
        }
    }
    for key in ["trades", "drawdowns", "monthly", "assumptions", "warnings"] {
        if !object[key].is_array() {
            return Err(format!("{key} 必须是数组"));
        }
    }
    for key in ["sweep", "walk_forward"] {
        match object.get(key) {
            None => return Err(format!("缺少字段 {key}")),
            Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Object(_)) => {}
            Some(_) => return Err(format!("{key} 必须是对象或 null")),
        }
    }
    Ok(())
}

fn validate_analysis_json(value: &serde_json::Value) -> std::result::Result<(), String> {
    let object = value.as_object().ok_or("analysis 必须是对象或 null")?;
    for key in [
        "version",
        "hash",
        "seed",
        "input_hash",
        "bootstrap",
        "warnings",
    ] {
        if !object.contains_key(key) {
            return Err(format!("analysis 缺少字段 {key}"));
        }
    }
    if object["version"].as_u64() != Some(1) {
        return Err("analysis.version 必须是 1".into());
    }
    for key in ["hash", "input_hash"] {
        if object[key].as_str().map(str::len) != Some(64) {
            return Err(format!("analysis.{key} 必须是 64 位十六进制"));
        }
    }
    if object["seed"].as_u64().is_none() {
        return Err("analysis.seed 必须是非负整数".into());
    }
    if !object["warnings"].is_array() {
        return Err("analysis.warnings 必须是数组".into());
    }
    match object.get("bootstrap") {
        None => return Err("analysis 缺少字段 bootstrap".into()),
        Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(bootstrap)) => {
            for key in ["samples", "block_length"] {
                if bootstrap
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .is_none()
                {
                    return Err(format!("analysis.bootstrap.{key} 必须是非负整数"));
                }
            }
            if !bootstrap
                .get("confidence")
                .is_some_and(serde_json::Value::is_number)
            {
                return Err("analysis.bootstrap.confidence 必须是数值".into());
            }
            let statistics = bootstrap
                .get("statistics")
                .and_then(serde_json::Value::as_array)
                .ok_or("analysis.bootstrap.statistics 必须是数组")?;
            for stat in statistics {
                let stat = stat
                    .as_object()
                    .ok_or("analysis.bootstrap.statistics 元素必须是对象")?;
                if stat
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_none()
                {
                    return Err("analysis.bootstrap.statistics 缺少 name".into());
                }
                if stat
                    .get("valid_samples")
                    .and_then(serde_json::Value::as_u64)
                    .is_none()
                {
                    return Err("analysis.bootstrap.statistics 缺少 valid_samples".into());
                }
                for key in ["point", "lower", "upper"] {
                    match stat.get(key) {
                        Some(serde_json::Value::Null) | Some(serde_json::Value::Number(_)) => {}
                        _ => {
                            return Err(format!(
                                "analysis.bootstrap.statistics.{key} 必须是数值或 null"
                            ));
                        }
                    }
                }
            }
        }
        Some(_) => return Err("analysis.bootstrap 必须是对象或 null".into()),
    }
    match object.get("pbo") {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Object(pbo)) => {
            for key in ["pbo", "logit_mean", "logit_p05", "logit_p50", "logit_p95"] {
                if !pbo.get(key).is_some_and(serde_json::Value::is_number) {
                    return Err(format!("analysis.pbo.{key} 必须是数值"));
                }
            }
            for key in ["splits", "blocks", "trials", "observations"] {
                if pbo.get(key).and_then(serde_json::Value::as_u64).is_none() {
                    return Err(format!("analysis.pbo.{key} 必须是非负整数"));
                }
            }
            match pbo.get("best_label") {
                Some(serde_json::Value::Null) | Some(serde_json::Value::String(_)) => {}
                _ => return Err("analysis.pbo.best_label 必须是字符串或 null".into()),
            }
            if !pbo.get("warnings").is_some_and(serde_json::Value::is_array) {
                return Err("analysis.pbo.warnings 必须是数组".into());
            }
        }
        Some(_) => return Err("analysis.pbo 必须是对象或 null".into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::analysis::BootstrapReport;
    use backtest_core::model::{
        DrawdownEpisode, EquityPoint, Metrics, MonthlyReturn, RunMeta, Side, TradeRecord,
    };
    use backtest_core::walkforward::{
        ParamSeries, WalkForwardGap, WalkForwardResult, WalkForwardWindow,
    };
    use std::collections::BTreeMap;

    fn sample_result() -> BacktestResult {
        let ts = 1_577_836_800_000;
        let equity = vec![
            EquityPoint {
                ts,
                equity: 100_000.0,
                cash: 100_000.0,
                gross_exposure: 0.0,
                drawdown: 0.0,
            },
            EquityPoint {
                ts: ts + 86_400_000,
                equity: 110_000.0,
                cash: 10_000.0,
                gross_exposure: 100_000.0,
                drawdown: 0.0,
            },
            EquityPoint {
                ts: ts + 2 * 86_400_000,
                equity: 99_000.0,
                cash: 10_000.0,
                gross_exposure: 89_000.0,
                drawdown: -0.1,
            },
        ];
        let trade = TradeRecord {
            symbol: 0,
            side: Side::Buy,
            entry_ts: ts,
            exit_ts: Some(ts + 2 * 86_400_000),
            entry_price: 100.0,
            exit_price: Some(99.0),
            qty: 10.0,
            pnl: -10.0,
            fees: 1.0,
            return_pct: -1.0,
            bars_held: 2,
            exit_reason: Some(FillReason::Close),
        };
        let mut metrics = Metrics::empty(365, 0.0);
        metrics.total_return = Some(0.1);
        metrics.cost_drag = Some(0.002);
        BacktestResult {
            meta: RunMeta {
                strategy: "sma_cross".into(),
                params: BTreeMap::new(),
                symbols: vec!["TEST".into()],
                requested_start: None,
                requested_end: None,
                first_bar_ts: Some(ts),
                last_bar_ts: Some(ts + 2 * 86_400_000),
                timeframe: "1d".into(),
                config_hash: "config".into(),
                data_hash: "data".into(),
                threads: 1,
                walk_forward_window: None,
                engine_version: "0.1.0".into(),
                generated_at: "2026-01-01T00:00:00Z".into(),
                warnings: Vec::new(),
            },
            equity,
            fills: Vec::new(),
            trades: vec![trade],
            metrics,
            drawdowns: vec![DrawdownEpisode {
                peak_ts: ts + 86_400_000,
                trough_ts: ts + 2 * 86_400_000,
                recovery_ts: None,
                depth: 0.1,
                duration_days: 1.0,
                recovered: false,
            }],
            monthly: vec![MonthlyReturn {
                year: 2020,
                month: 1,
                return_pct: 10.0,
            }],
            funding: None,
            warnings: vec!["测试警告".into()],
        }
    }

    fn empty_extras() -> ReportExtras {
        ReportExtras::default()
    }

    fn sample_analysis() -> AnalysisReport {
        AnalysisReport {
            version: 1,
            hash: "a".repeat(64),
            seed: 42,
            input_hash: "b".repeat(64),
            bootstrap: Some(BootstrapReport {
                samples: 1_000,
                block_length: 11,
                confidence: 0.95,
                statistics: vec![
                    backtest_core::analysis::BootstrapStat {
                        name: BootstrapStatistic::Mean,
                        point: Some(0.0004),
                        lower: Some(0.0001),
                        upper: Some(0.0007),
                        valid_samples: 1_000,
                    },
                    backtest_core::analysis::BootstrapStat {
                        name: BootstrapStatistic::Sharpe,
                        point: Some(1.25),
                        lower: Some(0.4),
                        upper: Some(2.1),
                        valid_samples: 998,
                    },
                ],
            }),
            pbo: Some(backtest_core::analysis::PboReport {
                pbo: 0.25,
                splits: 252,
                blocks: 10,
                trials: 40,
                observations: 600,
                logit_mean: -0.4,
                logit_p05: -1.2,
                logit_p50: -0.35,
                logit_p95: 0.6,
                best_label: Some("fast=5".into()),
                warnings: vec!["测试 PBO 警告".into()],
            }),
            warnings: vec!["测试分析警告".into()],
        }
    }

    #[test]
    fn analysis_report_uses_schema_1_3_and_keeps_base_hash() {
        let result = sample_result();
        let base_hash = content_hash(&result).expect("hash");
        let dir = tempfile::tempdir().expect("tempdir");
        let plain_dir = dir.path().join("plain");
        let plain_paths = write_report(&result, &empty_extras(), &plain_dir).expect("plain report");
        let plain: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&plain_paths.json).expect("read plain json"),
        )
        .expect("parse plain json");
        assert_eq!(plain["schema_version"], SCHEMA_VERSION);
        assert!(plain.get("analysis").is_none());

        let extras = ReportExtras {
            analysis: Some(sample_analysis()),
            ..ReportExtras::default()
        };
        let paths = write_report(&result, &extras, &dir.path().join("analysis")).expect("report");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.json).expect("read json"))
                .expect("parse json");
        assert_eq!(value["schema_version"], ANALYSIS_SCHEMA_VERSION);
        assert_eq!(value["result_hash"], base_hash);
        assert_eq!(value["analysis"]["hash"].as_str().map(str::len), Some(64));
        assert_eq!(value["analysis"]["bootstrap"]["samples"], 1_000);
        validate_report_json(&value).expect("schema");

        let html = std::fs::read_to_string(&paths.html).expect("read html");
        assert!(html.contains("Bootstrap 置信区间"));
        assert!(html.contains("stationary bootstrap"));
        assert!(html.contains("Sharpe(年化)"));
        assert!(html.contains("测试分析警告"));
    }

    #[test]
    fn analysis_schema_rejects_bad_hash() {
        let mut extras = ReportExtras {
            analysis: Some(sample_analysis()),
            ..ReportExtras::default()
        };
        extras.analysis.as_mut().expect("analysis").hash = "short".into();
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = write_report(&sample_result(), &extras, dir.path()).expect("report");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&paths.json).expect("read json"))
                .expect("parse json");
        assert!(validate_report_json(&value).is_err());
    }

    fn sample_walk_forward() -> WalkForwardResult {
        let ts = 1_577_836_800_000;
        WalkForwardResult {
            mode: backtest_core::config::WindowMode::Rolling,
            train_bars: 756,
            test_bars: 252,
            step_bars: 252,
            embargo_bars: 1,
            warmup_bars: 60,
            objective: "sharpe".into(),
            windows: vec![WalkForwardWindow {
                index: 0,
                train_start: 0,
                train_end: 756,
                test_start: 757,
                test_end: 1009,
                train_start_ts: ts,
                train_end_ts: ts + 756 * 86_400_000,
                test_start_ts: ts + 757 * 86_400_000,
                test_end_ts: ts + 1009 * 86_400_000,
                best_params: BTreeMap::from([("fast".into(), serde_json::json!(10))]),
                is_metrics: Metrics::empty(365, 0.0),
                oos_metrics: Metrics::empty(365, 0.0),
                warnings: Vec::new(),
            }],
            oos_equity: vec![EquityPoint {
                ts: ts + 757 * 86_400_000,
                equity: 100_000.0,
                cash: 100_000.0,
                gross_exposure: 0.0,
                drawdown: 0.0,
            }],
            oos_metrics: Metrics::empty(365, 0.0),
            param_stability: vec![ParamSeries {
                name: "fast".into(),
                values: vec![serde_json::json!(10)],
            }],
            skipped_windows: vec!["窗口 1 丢弃".into()],
            gaps: vec![WalkForwardGap {
                after_window: 0,
                start_index: 1009,
                end_index: 1100,
            }],
            dsr: None,
            elapsed_ms: 12,
        }
    }

    fn sample_sweep() -> SweepReport {
        SweepReport {
            objective: "sharpe".into(),
            top_k: 2,
            counts: SweepCounts {
                total: 4,
                valid: 3,
                skipped_invalid: 1,
                warnings: 0,
            },
            grid: BTreeMap::from([(
                "fast".into(),
                vec![serde_json::json!(5), serde_json::json!(10)],
            )]),
            rows: vec![SweepRow {
                rank: 1,
                params: BTreeMap::from([("fast".into(), serde_json::json!(5))]),
                objective_value: Some(1.2),
                metrics: Metrics::empty(365, 0.0),
                oos_metrics: Some(Metrics::empty(365, 0.0)),
            }],
            marginals: vec![ParamMarginal {
                name: "fast".into(),
                points: vec![MarginalPoint {
                    value: serde_json::json!(5),
                    mean_objective: Some(1.0),
                    count: 2,
                }],
            }],
            heatmap: None,
            oos: None,
            dsr: None,
            sweep_hash: "abc".into(),
            elapsed_ms: 1,
            analysis: Some(sample_analysis()),
        }
    }

    #[test]
    fn writes_parseable_offline_json_and_html() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = sample_result();
        let paths = write_report(&result, &empty_extras(), dir.path()).expect("report");

        let json = std::fs::read_to_string(&paths.json).expect("read json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["report_kind"], "run");
        assert!(value["result_hash"].as_str().unwrap().len() == 64);
        validate_report_json(&value).expect("schema");

        let html = std::fs::read_to_string(&paths.html).expect("read html");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
        assert!(!html.contains("<script"));
        assert!(html.contains("回测报告"));
        assert!(html.contains("测试警告"));
        for chapter in [
            "1. 概览",
            "2. 核心指标",
            "3. 净值与回撤",
            "4. 月度收益热力图",
            "5. 回撤段",
            "6. 交易明细",
            "7. 成本与元数据",
            "8. 参数敏感性",
            "9. 样本外与稳健性",
            "10. 假设与限制",
        ] {
            assert!(html.contains(chapter), "缺少章节 {chapter}");
        }
        assert!(!html.contains("S1–S5 只实现日线"));
        assert!(html.contains("周期由配置声明"));
        assert!(html.contains("复合 bar 收益"));
    }

    #[test]
    fn renders_funding_costs_and_keeps_hash_stable_without_funding() {
        use backtest_core::model::SymbolFunding;

        let base = sample_result();
        let without = content_hash(&base).expect("hash");
        let fresh = sample_result();
        assert_eq!(without, content_hash(&fresh).expect("hash"));
        let mut result = sample_result();
        result.funding = Some(FundingSummary {
            total_cost: 12.5,
            paid: 20.0,
            received: 7.5,
            events: 3,
            by_symbol: vec![SymbolFunding {
                symbol: 0,
                cost: 12.5,
                events: 3,
            }],
        });
        assert_ne!(without, content_hash(&result).expect("hash"));

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = write_report(&result, &empty_extras(), dir.path()).expect("report");
        let html = std::fs::read_to_string(&paths.html).expect("read html");
        assert!(html.contains("资金费净支出"));
        assert!(html.contains("12.50"));
        assert!(html.contains("资金费明细"));
        assert!(html.contains("结算次数"));
        let json = std::fs::read_to_string(&paths.json).expect("read json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(value["funding"]["total_cost"], 12.5);
        validate_report_json(&value).expect("schema");
    }

    #[test]
    fn content_hash_ignores_runtime_fields() {
        let first = sample_result();
        let mut second = sample_result();
        second.meta.generated_at = "2026-02-02T00:00:00Z".into();
        second.meta.threads = 8;
        second.meta.engine_version = "9.9.9".into();
        assert_eq!(
            content_hash(&first).expect("hash"),
            content_hash(&second).expect("hash")
        );
    }

    fn legacy_canonical_bytes(result: &BacktestResult) -> Vec<u8> {
        let mut value = serde_json::to_value(result).expect("value");
        if let Some(meta) = value.get_mut("meta").and_then(|meta| meta.as_object_mut()) {
            meta.remove("generated_at");
            meta.remove("threads");
            meta.remove("engine_version");
        }
        serde_json::to_vec(&value).expect("bytes")
    }

    fn canonical_bytes(result: &BacktestResult) -> Vec<u8> {
        let mut out = Vec::new();
        serde_json::to_writer(&mut out, &CanonicalResult(result)).expect("canonical");
        out
    }

    #[test]
    fn canonical_stream_matches_legacy_value_bytes() {
        let mut result = sample_result();
        result.fills = vec![FillRecord {
            ts: 1_577_836_800_000,
            symbol: 0,
            side: Side::Buy,
            qty: 10.0,
            price: 100.0,
            notional: 1_000.0,
            fee: 0.5,
            slippage: 0.2,
            reason: FillReason::Entry,
            ambiguous_bar: false,
        }];
        result.funding = Some(FundingSummary {
            total_cost: 12.5,
            paid: 20.0,
            received: 7.5,
            events: 3,
            by_symbol: vec![SymbolFunding {
                symbol: 0,
                cost: 12.5,
                events: 3,
            }],
        });
        assert_eq!(canonical_bytes(&result), legacy_canonical_bytes(&result));

        let digest = Sha256::digest(legacy_canonical_bytes(&result));
        let mut expected = String::with_capacity(64);
        for byte in digest {
            let _ = write!(expected, "{byte:02x}");
        }
        assert_eq!(content_hash(&result).expect("hash"), expected);

        result.funding = None;
        assert_eq!(canonical_bytes(&result), legacy_canonical_bytes(&result));
    }

    #[test]
    fn empty_trades_show_no_trade_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut result = sample_result();
        result.trades.clear();
        let paths = write_report(&result, &empty_extras(), dir.path()).expect("report");
        let html = std::fs::read_to_string(&paths.html).expect("read html");
        assert!(html.contains("无交易"));
    }

    #[test]
    fn sweep_and_walk_forward_sections_render() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut extras = ReportExtras {
            sweep: Some(sample_sweep()),
            walk_forward: Some(sample_walk_forward()),
            ..ReportExtras::default()
        };
        let paths = write_report(&sample_result(), &extras, dir.path()).expect("report");
        let html = std::fs::read_to_string(&paths.html).expect("read html");
        assert!(html.contains("Walk-forward 报告"));
        assert!(html.contains("Top 组合"));
        assert!(html.contains("Walk-forward 窗口"));
        assert!(html.contains("窗口 1 丢弃"));
        let json = std::fs::read_to_string(&paths.json).expect("read json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(value["sweep"].is_object());
        assert!(value["walk_forward"].is_object());
        validate_report_json(&value).expect("schema");
        extras.walk_forward = None;
        extras.sweep = None;
    }

    #[test]
    fn t5_12_walk_forward_param_stability_chart_renders() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut wf = sample_walk_forward();
        let mut second = wf.windows[0].clone();
        second.index = 1;
        second.best_params = BTreeMap::from([("fast".into(), serde_json::json!(12))]);
        let mut third = wf.windows[0].clone();
        third.index = 2;
        third.best_params = BTreeMap::from([("fast".into(), serde_json::json!(8))]);
        wf.windows.push(second);
        wf.windows.push(third);
        wf.param_stability = vec![ParamSeries {
            name: "fast".into(),
            values: vec![
                serde_json::json!(10),
                serde_json::json!(12),
                serde_json::json!(8),
            ],
        }];
        let extras = ReportExtras {
            walk_forward: Some(wf),
            ..ReportExtras::default()
        };
        let paths = write_report(&sample_result(), &extras, dir.path()).expect("report");
        let html = std::fs::read_to_string(&paths.html).expect("read html");
        assert!(html.contains("Walk-forward 窗口"));
        assert!(html.contains("OOS 拼接净值"));
        assert!(html.contains("参数稳定性"));
        assert!(html.contains("aria-label=\"参数稳定性\""));
        let json = std::fs::read_to_string(&paths.json).expect("read json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        validate_report_json(&value).expect("schema");
    }

    #[test]
    fn t5_8_report_case_fixture_passes_schema() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/report_case.json");
        let text = std::fs::read_to_string(path).expect("read fixture");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture");
        validate_report_json(&value).expect("fixture schema");
        assert_eq!(value["report_kind"], "run");
        assert!(value["sweep"].is_null());
        assert!(value["walk_forward"].is_null());
        assert_eq!(
            value["equity"].as_array().expect("equity").len(),
            value["underwater"].as_array().expect("underwater").len()
        );
    }

    #[test]
    fn downsample_keeps_first_last_and_max_drawdown() {
        let mut points = Vec::new();
        for index in 0..MAX_EQUITY_POINTS + 500 {
            points.push(EquityPoint {
                ts: index as i64,
                equity: 100.0,
                cash: 100.0,
                gross_exposure: 0.0,
                drawdown: if index == 12_345 { -0.5 } else { 0.0 },
            });
        }
        let (kept, note) = downsample_equity(&points);
        assert!(kept.len() <= MAX_EQUITY_POINTS + 3);
        assert_eq!(kept.first().unwrap().ts, 0);
        assert_eq!(kept.last().unwrap().ts, points.len() as i64 - 1);
        assert!(kept.iter().any(|point| point.drawdown == -0.5));
        assert_eq!(note.expect("note").original_points, points.len());
    }

    #[test]
    fn schema_validator_rejects_nan_like_and_missing_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = write_report(&sample_result(), &empty_extras(), dir.path()).expect("report");
        let json = std::fs::read_to_string(&paths.json).expect("read json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let mut broken = value.clone();
        broken.as_object_mut().unwrap().remove("metrics");
        assert!(validate_report_json(&broken).is_err());
        let mut broken = value;
        broken["metrics"]["sharpe"] = serde_json::json!("NaN");
        assert!(validate_report_json(&broken).is_err());
    }
}
