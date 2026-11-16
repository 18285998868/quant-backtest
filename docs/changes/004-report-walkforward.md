# 004 指标、报告与 walk-forward

## 动机

S5 需要补齐指标空值与 DSR、样本外 70/30、walk-forward、参数敏感性,
并把 S4 留下的 `result.json.sweep = null` 接入报告契约。报告必须保持单文件离线、
固定 10 章、不引入 PDF 或 CDN 依赖。

## 影响范围

### 指标与 DSR

- `Metrics` 新增 `cost_drag`:`Σ费用 / 平均权益 / 年数`,与换手同口径;为空时输出 `null`。
- 新增 `DeflatedSharpe`,字段为 `probability`、`trials`、`sharpe_per_period`、
  `expected_max_sharpe`、`trial_sharpe_variance`、`skewness`、`kurtosis`、`observations`。
- 正态 CDF/分位数自实现:Acklam 分位数加一步 Halley 修正,`erfc` 用泰勒级数与连分式,
  不新增依赖。已知输入与 SciPy 参考值误差小于 `1e-9`。
- DSR 用每期 Sharpe、收益偏度/峰度与试验 Sharpe 方差;`N=1` 且 `SR=0` 时 DSR 为 0.5。
  walk-forward 的 `N` 取实际运行的组合数,即网格大小乘窗口数;跳过的窗口不计。
- `N` 只校正搜索次数,不校正参数相关性;报告与假设章节都保留这一局限说明。

### 引擎区间与预热

- `EngineInput` 新增 `start_index`、`trade_start_index`、`end_index`。
- `[start_index, trade_start_index)` 只喂策略、更新指标状态,不产生订单、不记录权益;
  `ctx.index` 保持全局下标,避免截面动量调仓周期漂移。
- 权益与指标只从 `trade_start_index` 开始;`first_bar_ts`/`last_bar_ts` 改取权益首尾,
  不再取全量数据首尾。
- 默认值保持全量区间,`run` 与既有测试行为不变。

### 样本外与扫描

- `sweep` 在样本内 `[0, split)` 选参,再带预热在 `[split, len)` 评估 IS 前 100 名;
  报告 IS/OOS 指标与 Spearman 排名相关性。
- `oos_split` 按全量 bar 数四舍五入后夹在 `[1, len-1]`;切分早于最长策略窗口时记录预热不足告警。
- 新增参数边际效应与两参数热力图;热力图取边际极差最大的两个参数,其余参数按均值聚合。
- `sweep` 子命令现在同时写出 `result.json` 与 `report.html`,
  报告类型为 `sweep`;`sweep_summary.json` 保留全量有效组合与 top-K 曲线。

### walk-forward

- `backtest-core::walkforward` 放纯函数:窗口规划、空档、OOS 复利拼接、参数稳定性。
  CLI 编排在 `backtest-cli/src/walkforward.rs`,复用 `sweep` 的网格展开与窗口内并行。
- 支持 `rolling`/`anchored`、`embargo_bars`、`warmup_bars`;窗口之间串行、窗口内并行,避免嵌套 rayon。
- 训练窗口只用训练集选参;训练与测试预热都只喂策略,不交易、不计入指标/OOS 收益。
- 末窗不足 `test_bars` 时丢弃并记入 `skipped_windows`;`step_bars > test_bars` 的空档记入 `gaps`;
  `step_bars < test_bars` 视为重叠并在配置校验时报错。
- OOS 按测试段日收益复利拼接,现金与名义敞口按同一比例缩放,保留原窗口的杠杆/持仓比例。
- 新增 `walkforward` 子命令:配置同时含 `[sweep]` 与 `[walk_forward]`,输出 `result.json` 与 `report.html`。
- `[walk_forward]` 默认 `rolling`、`train 756`、`test 252`、`step = test`、`embargo 1`、`warmup 60`、
  目标 `sharpe`;`warmup_bars` 必须大于等于网格内最长策略窗口。

### 报告 schema 1.1

- 顶层新增 `report_kind`(`run`/`sweep`/`walk_forward`)、`underwater`、`equity_downsampled`。
- `sweep` 与 `walk_forward` 不再恒为 `null`;本次未运行时仍为 `null`,含义是“未运行”而非失败。
- `costs` 增加 `cost_drag`;`metrics` 增加 `cost_drag`。
- 新增 `validate_report_json`:检查必填字段、数值或 `null`、`equity`/`underwater` 等长、
  `result_hash` 长度与 `sweep`/`walk_forward` 类型。
- HTML 仍为 10 章,walk-forward 放在第 9 章“样本外与稳健性”;内联 SVG/CSS,无 CDN、无脚本。
- 权益超过 20000 点时等间隔下采样并保留首尾与最大回撤点;JSON 记录 `equity_downsampled`。
- `trades` 在 JSON 中全量,HTML 只渲染前 500 条;`sweep` 的 HTML 只展示 top 100 行。
- JSON 中未定义指标输出 `null`,禁止 NaN/Inf;`result.json` 的 `result_hash` 仍只覆盖
  `BacktestResult` 并排除生成时间、线程数与引擎版本。

### 夹具与文档

- 新增 `fixtures/s5_daily.csv`(400 根日线)、`fixtures/sweep.toml`、`fixtures/walkforward.toml`、
  `fixtures/report_case.json`。
- `fixtures/report_case.json` 是 schema 1.1 的最小可校验样例,不代表策略有效性。
- 新增本变更说明与 `STEP.d/09-s5-acceptance.md`,同步 README、STEP、CHANGELOG 与报告步骤文档。

## 替代方案

1. 内嵌 ECharts。体积约 1 MB,离线测试需额外约束;首版用内联 SVG 已能表达净值、水下、
   热力图与参数序列,弃用。
2. 报告输出 PDF。增加渲染依赖与字体问题,且不是验收项,弃用。
3. 为 walk-forward 新增顶级章节。违反固定 10 章契约,弃用。
4. 把全量扫描曲线写入 `result.json.sweep`。1000 组会放大 JSON 与内存,且与 top-K 预算冲突,弃用。
5. 每个 walk-forward 窗口单独输出报告。报告数量随窗口膨胀,汇总表与参数稳定性已足够,弃用。
6. 窗口之间并行。会与窗口内 rayon 嵌套,线程数不可控;窗口内并行已能满足首版数据量,弃用。
7. 用 70/30 的 OOS 指标充当 walk-forward。两者语义不同,报告字段分开,弃用。
8. DSR 只按网格大小计 `N`。walk-forward 实际运行了多窗网格,按实际运行数计更贴近搜索次数;
   代价是偏保守,已在报告中注明。

## 回退计划

删除 `cost_drag`、`DeflatedSharpe`、DSR/正态函数、`EngineInput` 的三个区间字段、
`backtest-core/src/walkforward.rs`、`backtest-cli/src/walkforward.rs`、`walkforward` 子命令、
`[walk_forward]` 解析与报告 schema 1.1 字段,把 `schema_version` 退回 1.0,
并恢复 `ReportExtras` 与 `write_report` 的旧签名即可。数据 schema、策略 trait、
成交顺序与 S1–S4 的扫描语义不受影响。
