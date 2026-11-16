# 03 指标与报告

## 目标

从净值、成交、交易计算 S1 指标,输出离线 JSON 与 HTML。

## 输入输出

- 输入:`BacktestResult`。
- 输出:`Metrics`、回撤段、月收益、`result.json`、`report.html`。

## 指标

| 指标 | 定义 |
| --- | --- |
| 总收益 | 末值 / 初始资金 - 1 |
| CAGR | 按日历天数年化 |
| 年化波动 | 日收益样本标准差 × sqrt(ppy) |
| Sharpe | 超额均值 / 标准差 × sqrt(ppy) |
| Sortino | 下行偏差版本 |
| 最大回撤 | 净值 / 历史峰值 - 1 的最小值,报告为正值 |
| Calmar | CAGR / 最大回撤 |
| Ulcer | sqrt(mean(dd²)) |
| 胜率/盈亏比/期望 | 仅统计已平仓交易 |
| 换手/成本 | 成交名义/费用除以平均权益与年数 |
| 敞口 | 有持仓 bar 数 / 总 bar 数 |

## 空值与边界

- 样本不足、标准差为 0、无交易时输出 JSON null。
- 不输出 NaN/Inf。
- 回撤段按深度排序取前 5;水下曲线与净值同长度。

## 报告

- `result.json`:含 `schema_version`、meta、metrics、equity、drawdowns、monthly、trades、costs、warnings。
- `report.html`:单文件、离线、中文;内联 SVG 净值/回撤图;指标表、交易表、成本、假设与警告。
- 不引用 CDN,不写绝对路径。

## 测试

- 最大回撤 100/110/99/120 为 10%,Ulcer 为 0.05。
- 无交易时指标为 null。
- HTML 不含 `http://`/`https://` 资源引用。
- JSON 可被 `serde_json` 重新解析。

## 验收

`run --config config/00.smoke.toml` 生成两个文件;HTML 断网可打开;JSON 无 NaN。

## 依赖

依赖 02 的结果类型。
