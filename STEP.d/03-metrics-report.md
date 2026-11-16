# 03 指标与报告

## 目标

从净值、成交、交易计算指标,输出离线 JSON(schema 1.1)与 10 章 HTML;
接入样本外、DSR、参数敏感性与 walk-forward。

## 输入输出

- 输入:`BacktestResult`、`ReportExtras`(`sweep`/`walk_forward` 均可选)。
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
| 回撤时长 | 峰值到恢复的日历天数;未恢复记到序列末尾 |
| Calmar | CAGR / 最大回撤 |
| Ulcer | sqrt(mean(dd²)) |
| 胜率/盈亏比/期望 | 仅统计已平仓交易 |
| 换手 | 成交名义 / 平均权益 / 年数 |
| 成本拖累 | 费用 / 平均权益 / 年数 |
| 敞口 | 有持仓 bar 数 / 总 bar 数 |

## 空值与边界

- 样本不足、标准差为 0、无交易时输出 JSON null。
- 不输出 NaN/Inf;正态分位数与 DSR 用同一套有限值检查。
- 回撤段按深度排序取前 5;水下曲线与净值同长度。
- 权益超过 20000 点时等间隔下采样,保留首尾与最大回撤点。

## DSR

- 用每期 Sharpe、收益偏度 γ3、峰度 γ4(正态为 3)与试验次数 N。
- `SR0 = sqrt(Var(SR_trials)) * ((1-γ)Φ^-1(1-1/N) + γΦ^-1(1-1/(N e)))`。
- `DSR = Φ((SR_hat - SR0) * sqrt(T-1) / sqrt(1 - γ3*SR_hat + (γ4-1)/4*SR_hat^2))`。
- `N=1` 时 `SR0=0`;N 为实际运行的参数组合数,跳过的不计。
- DSR 只校正搜索次数,不校正参数相关性;报告注明该局限。

## 样本外与 walk-forward

- 70/30:样本内选参,样本外带预热重跑 IS 前 100 名;报告 IS/OOS 指标与 Spearman 排名相关。
- walk-forward:报告窗口表、每窗最优参数、IS/OOS 指标、OOS 拼接净值、
  参数序列图、跳过窗口与空档;放在第 9 章。
- OOS 按测试段日收益复利拼接,不以窗口绝对权益首尾相接。
- 预热段只喂策略,不交易、不计入 OOS 收益;训练预热同样不重置 `ctx.index`。

## 报告

- `result.json`:含 `schema_version`(1.1)、`report_kind`、`result_hash`、meta、metrics、
  equity、underwater、equity_downsampled、drawdowns、monthly、trades、costs、
  sweep、walk_forward、assumptions、warnings。
- `report.html`:单文件、离线、中文;内联 SVG 净值/回撤、月度热力图、参数曲线与参数热力图;
  指标表、交易表、成本、假设与警告;固定 10 章。
- 不引用 CDN,不写绝对路径;HTML 只用前 500 条交易。

## 测试

- 最大回撤 100/110/99/120 为 10%,Ulcer 为 0.05。
- 无交易时可选指标为 null,HTML 显示“无交易”。
- HTML 不含 `http://`/`https://` 资源引用与 `<script>`。
- JSON 可被 `serde_json` 重新解析,并通过 `validate_report_json`。
- `fixtures/report_case.json` 作为 schema 1.1 的固定样例参与校验。
- DSR 边界 N=1/SR=0 为 0.5,试验次数单调递减、Sharpe 单调递增,并与 SciPy 参考值一致。

## 验收

`run`、`sweep`、`walkforward` 各生成 `result.json` 与 `report.html`;HTML 断网可打开;
JSON 无 NaN;walk-forward 的 OOS 拼接与参数稳定性见 `STEP.d/09-s5-acceptance.md`。

## 依赖

依赖 02 的结果类型;walk-forward 依赖 04 的扫描编排。
