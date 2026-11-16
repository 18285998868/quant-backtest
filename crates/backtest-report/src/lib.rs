//! 离线 JSON 与单文件 HTML 报告。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use backtest_core::error::{ReportError, Result};
use backtest_core::model::{
    BacktestResult, FillReason, Metrics, MonthlyReturn, Side, TradeRecord, Ts,
};
use chrono::DateTime;
use serde::Serialize;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: &str = "1.0";
const ASSUMPTIONS: &[&str] = &[
    "bar 时间戳为 UTC,bar 区间为 [start, end),收盘价对应 end。",
    "策略在收盘读取信号,下一根可用 bar 开盘成交,不使用未来数据。",
    "手续费按成交名义的万分比加单笔固定费;滑点按单边万分比。",
    "首版要求 cash >= 0,不做多头融资;空头保证金可用,不计融券费与资金费率。",
    "不建模分红送转、隔夜利息、强平与保证金追缴。",
    "一次回测只允许一种计价货币;跨资产类别在收益层另行合成。",
    "美股使用复权价,加密与外汇使用原始报价。",
    "缺少 bar 时单标的顺延到下一根可用 bar;多标的不做填补。",
    "多标的同时有订单时同一 bar 先卖后买;同一方向内风险退出优先,再按标的 ID 升序。",
    "截面动量按 lookback 收盘收益选 top_k,方向由动量符号决定;逆波动率用对数收益样本标准差,无效时回退等权并写入 warnings。",
    "止损/止盈按首次入场均价计算;加减仓不重置,回到空仓后再次开仓才重置。",
    "同一 bar 同时触发止损与止盈时按止损成交,并标记 ambiguous_bar。",
    "收盘触发的止损/止盈在下一根开盘成交,不按触发 bar 的收盘价成交。",
    "S1–S4 只实现日线;分钟/小时/周线在 M1 阶段实现。",
];

#[derive(Debug)]
pub struct ReportPaths {
    pub json: PathBuf,
    pub html: PathBuf,
}

#[derive(Serialize)]
struct Costs {
    total_fees: f64,
    total_slippage: f64,
    fills: usize,
    turnover: Option<f64>,
}

#[derive(Serialize)]
struct ReportJson<'a> {
    schema_version: &'static str,
    result_hash: String,
    meta: &'a backtest_core::model::RunMeta,
    metrics: &'a Metrics,
    equity: &'a [backtest_core::model::EquityPoint],
    drawdowns: &'a [backtest_core::model::DrawdownEpisode],
    monthly: &'a [MonthlyReturn],
    trades: &'a [TradeRecord],
    costs: Costs,
    sweep: Option<()>,
    walk_forward: Option<()>,
    assumptions: &'static [&'static str],
    warnings: &'a [String],
}

pub fn write_report(result: &BacktestResult, output_dir: &Path) -> Result<ReportPaths> {
    std::fs::create_dir_all(output_dir).map_err(|source| ReportError::CreateDir {
        path: output_dir.to_path_buf(),
        source,
    })?;
    let json_path = output_dir.join("result.json");
    let html_path = output_dir.join("report.html");
    write_json(result, &json_path)?;
    write_html(result, &html_path)?;
    Ok(ReportPaths {
        json: json_path,
        html: html_path,
    })
}

pub fn write_json(result: &BacktestResult, path: &Path) -> Result<()> {
    let result_hash = content_hash(result)?;
    let payload = ReportJson {
        schema_version: SCHEMA_VERSION,
        result_hash,
        meta: &result.meta,
        metrics: &result.metrics,
        equity: &result.equity,
        drawdowns: &result.drawdowns,
        monthly: &result.monthly,
        trades: &result.trades,
        costs: Costs {
            total_fees: result.metrics.total_fees,
            total_slippage: result.fills.iter().map(|fill| fill.slippage).sum(),
            fills: result.fills.len(),
            turnover: result.metrics.turnover,
        },
        sweep: None,
        walk_forward: None,
        assumptions: ASSUMPTIONS,
        warnings: &result.warnings,
    };
    let text = serde_json::to_string_pretty(&payload).map_err(|err| ReportError::Serialize {
        message: err.to_string(),
    })?;
    std::fs::write(path, text).map_err(|source| ReportError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

pub fn content_hash(result: &BacktestResult) -> Result<String> {
    let mut value = serde_json::to_value(result).map_err(|err| ReportError::Serialize {
        message: err.to_string(),
    })?;
    if let Some(meta) = value.get_mut("meta").and_then(|meta| meta.as_object_mut()) {
        meta.remove("generated_at");
        meta.remove("threads");
        meta.remove("engine_version");
    }
    let bytes = serde_json::to_vec(&value).map_err(|err| ReportError::Serialize {
        message: err.to_string(),
    })?;
    let digest = Sha256::digest(&bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

pub fn write_html(result: &BacktestResult, path: &Path) -> Result<()> {
    let hash = content_hash(result)?;
    let meta = &result.meta;
    let metrics = &result.metrics;
    let mut html = String::with_capacity(32 * 1024);
    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    let _ = writeln!(
        html,
        "<title>回测报告 - {}</title>",
        escape_html(&meta.strategy)
    );
    html.push_str("<style>\n");
    html.push_str(
        ":root{--bg:#f7f8fa;--panel:#fff;--ink:#1f2933;--muted:#6b7280;--line:#e5e7eb;--accent:#2563eb;--warn:#b45309}\n\
         *{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:14px/1.6 -apple-system,BlinkMacSystemFont,'Segoe UI','Noto Sans CJK SC',sans-serif}\n\
         main{max-width:1120px;margin:0 auto;padding:32px 20px 64px}h1{font-size:26px;margin:0 0 4px}h2{font-size:19px;margin:36px 0 12px}\n\
         .sub{color:var(--muted);margin-bottom:24px}.panel{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:18px;margin:14px 0}\n\
         table{width:100%;border-collapse:collapse}th,td{padding:8px 10px;border-bottom:1px solid var(--line);text-align:left;vertical-align:top}\n\
         th{color:var(--muted);font-weight:600;background:#fafafa}td.num{text-align:right;font-variant-numeric:tabular-nums}\n\
         .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:10px}\n\
         .kpi{background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:14px}.kpi .k{color:var(--muted);font-size:12px}.kpi .v{font-size:20px;font-variant-numeric:tabular-nums}\n\
         .good{color:#047857}.bad{color:#b91c1c}.muted{color:var(--muted)}.warn{color:var(--warn)}\n\
         svg{width:100%;height:auto;display:block}code{font-size:12px;word-break:break-all}\n\
         ul{margin:8px 0 0 20px;padding:0}@media print{body{background:#fff}main{max-width:none}}\n",
    );
    html.push_str("</style>\n</head>\n<body>\n<main>\n");
    let _ = writeln!(html, "<h1>回测报告</h1>");
    let _ = writeln!(
        html,
        "<div class=\"sub\">策略 {} · 标的 {} · 周期 {} · 生成 {}</div>",
        escape_html(&meta.strategy),
        escape_html(&meta.symbols.join(", ")),
        escape_html(&meta.timeframe),
        escape_html(&meta.generated_at)
    );

    html.push_str("<div class=\"grid\">\n");
    kpi(
        &mut html,
        "总收益",
        pct(metrics.total_return),
        metrics.total_return,
    );
    kpi(&mut html, "CAGR", pct(metrics.cagr), metrics.cagr);
    kpi(
        &mut html,
        "最大回撤",
        pct(metrics.max_drawdown),
        metrics.max_drawdown.map(|v| -v),
    );
    kpi(&mut html, "Sharpe", num(metrics.sharpe, 3), metrics.sharpe);
    kpi(&mut html, "交易数", metrics.trades.to_string(), Some(0.0));
    kpi(&mut html, "总费用", money(metrics.total_fees), Some(0.0));
    html.push_str("</div>\n");

    html.push_str("<h2>净值与回撤</h2>\n<div class=\"panel\">\n");
    html.push_str(&equity_svg(result));
    html.push_str(&drawdown_svg(result));
    html.push_str("</div>\n");

    html.push_str("<h2>指标</h2>\n<div class=\"panel\"><table><tbody>\n");
    metric_row(&mut html, "总收益", pct(metrics.total_return));
    metric_row(&mut html, "CAGR", pct(metrics.cagr));
    metric_row(&mut html, "年化波动", pct(metrics.annual_volatility));
    metric_row(&mut html, "Sharpe", num(metrics.sharpe, 4));
    metric_row(&mut html, "Sortino", num(metrics.sortino, 4));
    metric_row(&mut html, "最大回撤", pct(metrics.max_drawdown));
    metric_row(
        &mut html,
        "最长回撤天数",
        metrics
            .max_drawdown_duration_days
            .map(|value| format!("{value:.1}"))
            .unwrap_or_else(|| "—".into()),
    );
    metric_row(&mut html, "Calmar", num(metrics.calmar, 4));
    metric_row(&mut html, "Ulcer", num(metrics.ulcer, 4));
    metric_row(&mut html, "胜率", pct(metrics.win_rate));
    metric_row(&mut html, "盈亏比", num(metrics.profit_factor, 4));
    metric_row(&mut html, "期望每笔", money_opt(metrics.expectancy));
    metric_row(&mut html, "换手", num(metrics.turnover, 4));
    metric_row(&mut html, "敞口", pct(metrics.exposure));
    metric_row(&mut html, "年化因子", metrics.periods_per_year.to_string());
    metric_row(&mut html, "无风险利率", pct(Some(metrics.risk_free_rate)));
    html.push_str("</tbody></table></div>\n");

    html.push_str("<h2>月度收益</h2>\n<div class=\"panel\">\n");
    monthly_table(&mut html, &result.monthly);
    html.push_str("</div>\n");

    html.push_str("<h2>回撤段</h2>\n<div class=\"panel\">\n");
    drawdown_table(&mut html, &result.drawdowns);
    html.push_str("</div>\n");

    html.push_str("<h2>交易明细</h2>\n<div class=\"panel\">\n");
    trades_table(&mut html, &result.trades, &meta.symbols);
    html.push_str("</div>\n");

    html.push_str("<h2>成本与元数据</h2>\n<div class=\"panel\"><table><tbody>\n");
    metric_row(
        &mut html,
        "手续费合计",
        money(result.fills.iter().map(|fill| fill.fee).sum()),
    );
    metric_row(
        &mut html,
        "滑点合计",
        money(result.fills.iter().map(|fill| fill.slippage).sum()),
    );
    metric_row(&mut html, "成交笔数", result.fills.len().to_string());
    metric_row(
        &mut html,
        "配置哈希",
        format!("<code>{}</code>", escape_html(&meta.config_hash)),
    );
    metric_row(
        &mut html,
        "数据哈希",
        format!("<code>{}</code>", escape_html(&meta.data_hash)),
    );
    metric_row(&mut html, "结果哈希", format!("<code>{hash}</code>"));
    metric_row(&mut html, "线程数", meta.threads.to_string());
    html.push_str("</tbody></table></div>\n");

    html.push_str("<h2>假设与限制</h2>\n<div class=\"panel\"><ul>\n");
    for item in ASSUMPTIONS {
        let _ = writeln!(html, "<li>{}</li>", escape_html(item));
    }
    html.push_str("</ul></div>\n");

    html.push_str("<h2>警告</h2>\n<div class=\"panel\">\n");
    if result.warnings.is_empty() {
        html.push_str("<p class=\"muted\">无</p>\n");
    } else {
        html.push_str("<ul>\n");
        for item in &result.warnings {
            let _ = writeln!(html, "<li class=\"warn\">{}</li>", escape_html(item));
        }
        html.push_str("</ul>\n");
    }
    html.push_str("</div>\n");
    html.push_str("</main>\n</body>\n</html>\n");
    std::fs::write(path, html).map_err(|source| ReportError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
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

fn monthly_table(html: &mut String, monthly: &[MonthlyReturn]) {
    if monthly.is_empty() {
        html.push_str("<p class=\"muted\">无</p>");
        return;
    }
    html.push_str("<table><thead><tr><th>年月</th><th>收益</th></tr></thead><tbody>\n");
    for item in monthly {
        let class = if item.return_pct < 0.0 {
            "num bad"
        } else {
            "num good"
        };
        let _ = writeln!(
            html,
            "<tr><td>{:04}-{:02}</td><td class=\"{}\">{:.2}%</td></tr>",
            item.year, item.month, class, item.return_pct
        );
    }
    html.push_str("</tbody></table>\n");
}

fn drawdown_table(html: &mut String, drawdowns: &[backtest_core::model::DrawdownEpisode]) {
    if drawdowns.is_empty() {
        html.push_str("<p class=\"muted\">无回撤</p>");
        return;
    }
    html.push_str(
        "<table><thead><tr><th>峰值</th><th>谷值</th><th>恢复</th><th>深度</th><th>天数</th></tr></thead><tbody>\n",
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

fn trades_table(html: &mut String, trades: &[TradeRecord], symbols: &[String]) {
    if trades.is_empty() {
        html.push_str("<p class=\"muted\">无已平仓交易</p>");
        return;
    }
    html.push_str(
        "<table><thead><tr><th>标的</th><th>方向</th><th>入场</th><th>出场</th><th>数量</th><th>PnL</th><th>费用</th><th>收益</th><th>持有 bar</th><th>原因</th></tr></thead><tbody>\n",
    );
    for trade in trades.iter().take(500) {
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
    html.push_str("</tbody></table>\n");
    if trades.len() > 500 {
        let _ = writeln!(
            html,
            "<p class=\"muted\">仅显示前 500 条,共 {} 条。</p>",
            trades.len()
        );
    }
}

fn equity_svg(result: &BacktestResult) -> String {
    let points = &result.equity;
    if points.len() < 2 {
        return "<p class=\"muted\">净值点不足,无法绘图</p>\n".into();
    }
    let (min, max) = points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, p| {
            (acc.0.min(p.equity), acc.1.max(p.equity))
        });
    let range = (max - min).max(max.abs() * 0.01).max(1.0);
    let mut svg = String::new();
    svg.push_str("<svg viewBox=\"0 0 960 280\" role=\"img\" aria-label=\"净值曲线\">\n");
    svg.push_str("<rect x=\"0\" y=\"0\" width=\"960\" height=\"280\" fill=\"#fff\"/>\n");
    for i in 0..=4 {
        let y = 20.0 + i as f64 * 60.0;
        let _ = writeln!(
            svg,
            "<line x1=\"48\" y1=\"{y:.1}\" x2=\"940\" y2=\"{y:.1}\" stroke=\"#eef0f3\"/>"
        );
    }
    let mut line = String::new();
    for (index, point) in points.iter().enumerate() {
        let x = 48.0 + index as f64 / (points.len() - 1) as f64 * 892.0;
        let y = 240.0 - (point.equity - min) / range * 220.0;
        if index > 0 {
            line.push(' ');
        }
        let _ = write!(line, "{x:.1},{y:.1}");
    }
    let _ = writeln!(
        svg,
        "<polyline fill=\"none\" stroke=\"#2563eb\" stroke-width=\"2\" points=\"{line}\"/>"
    );
    let _ = writeln!(
        svg,
        "<text x=\"48\" y=\"16\" font-size=\"12\" fill=\"#6b7280\">{:.2}</text>",
        max
    );
    let _ = writeln!(
        svg,
        "<text x=\"48\" y=\"274\" font-size=\"12\" fill=\"#6b7280\">{:.2}</text>",
        min
    );
    svg.push_str("</svg>\n<div class=\"muted\">");
    let _ = write!(
        svg,
        "{} 至 {} · 末值 {:.2}",
        date(points[0].ts),
        date(points[points.len() - 1].ts),
        points[points.len() - 1].equity
    );
    svg.push_str("</div>\n");
    svg
}

fn drawdown_svg(result: &BacktestResult) -> String {
    let points = &result.equity;
    if points.len() < 2 {
        return String::new();
    }
    let min = points
        .iter()
        .map(|point| point.drawdown)
        .fold(0.0_f64, f64::min)
        .min(-0.0001);
    let mut svg = String::new();
    svg.push_str("<svg viewBox=\"0 0 960 200\" role=\"img\" aria-label=\"回撤曲线\">\n");
    svg.push_str("<rect x=\"0\" y=\"0\" width=\"960\" height=\"200\" fill=\"#fff\"/>\n");
    let mut line = String::new();
    for (index, point) in points.iter().enumerate() {
        let x = 48.0 + index as f64 / (points.len() - 1) as f64 * 892.0;
        let y = 20.0 - point.drawdown / min * 150.0;
        if index > 0 {
            line.push(' ');
        }
        let _ = write!(line, "{x:.1},{y:.1}");
    }
    let _ = writeln!(
        svg,
        "<polyline fill=\"none\" stroke=\"#b91c1c\" stroke-width=\"1.5\" points=\"{line}\"/>"
    );
    let _ = writeln!(
        svg,
        "<line x1=\"48\" y1=\"20\" x2=\"940\" y2=\"20\" stroke=\"#e5e7eb\"/>"
    );
    let _ = writeln!(
        svg,
        "<text x=\"48\" y=\"190\" font-size=\"12\" fill=\"#6b7280\">最大 {:.2}%</text>",
        min * 100.0
    );
    svg.push_str("</svg>\n");
    svg
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

#[cfg(test)]
mod tests {
    use super::*;
    use backtest_core::model::{
        DrawdownEpisode, EquityPoint, Metrics, MonthlyReturn, RunMeta, Side, TradeRecord,
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
            warnings: vec!["测试警告".into()],
        }
    }

    #[test]
    fn writes_parseable_offline_json_and_html() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = sample_result();
        let paths = write_report(&result, dir.path()).expect("report");

        let json = std::fs::read_to_string(&paths.json).expect("read json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse json");
        assert_eq!(value["schema_version"], "1.0");
        assert!(value["result_hash"].as_str().unwrap().len() == 64);

        let html = std::fs::read_to_string(&paths.html).expect("read html");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
        assert!(html.contains("回测报告"));
        assert!(html.contains("测试警告"));
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
}
