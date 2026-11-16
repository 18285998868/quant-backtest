# 09 S5 指标与报告验收

## 目标

验证 `docs/03-流程设计.md` 的 T5.1–T5.13:完整指标、回撤段、成本拖累、DSR、
单文件离线 HTML、样本外、walk-forward、窗口边界、预热与 OOS 拼接。

## 环境

- 远程构建机:CachyOS,i5-12400F(12 线程),32 GiB 内存。
- release 构建:`cargo build --release --locked`。
- 冒烟数据:`fixtures/s5_daily.csv`,400 根日线。
- 报告渲染:Chromium headless,`--host-resolver-rules="MAP * ~NOTFOUND"` 阻断外部解析,
  窗口 1280×1800。

## 用例

| 用例 | 证据 |
| --- | --- |
| T5.1 最大回撤 | `max_drawdown_and_ulcer_match_hand_calculation`:权益 100/110/99/120,MaxDD=10%,恢复于 t4 |
| T5.2 Ulcer | 同用例:Ulcer=0.05;`zero_drawdown_is_not_negative_zero` 覆盖 0 不为负零 |
| T5.3 Sharpe | `sharpe_matches_independent_calculation`:已知 0/1%/2%/-1%/3% 收益,与独立公式误差小于 `1e-9` |
| T5.4 空交易 | `empty_trades_leave_optional_metrics_null`:胜率、盈亏比、期望值为 `null`;`empty_trades_show_no_trade_text`:HTML 显示“无交易” |
| T5.5 HTML 离线 | `writes_parseable_offline_json_and_html`:无 `http://`、`https://`、`<script`,含 10 个固定章节;headless Chromium 阻断解析后正常渲染中文、表格与内联 SVG |
| T5.6 样本外 | `serial_and_parallel_metrics_match`:断言 `oos_split=0.7`、`split_index=280`、样本 100、IS/OOS Sharpe 非空;冒烟 OOS 收益 16.196%,Sharpe 6.7979,排名相关 0.9845588 |
| T5.7 DSR | `dsr_is_half_for_single_zero_sharpe_trial`、`dsr_falls_with_more_trials_and_rises_with_sharpe`、`dsr_matches_reference_implementation`(SciPy 参考值,误差小于 `1e-9`) |
| T5.8 JSON schema | `t5_8_report_case_fixture_passes_schema`:`fixtures/report_case.json` 通过 schema 1.1;`schema_validator_rejects_nan_like_and_missing_fields` 拒绝缺字段与字符串 NaN |
| T5.9 窗口边界 | `t5_9_window_boundaries_follow_contract`:train 0..756、test 757..1009,窗口 1 训练起点 252;`overlap_is_rejected_and_partial_tail_is_skipped`、`gaps_are_reported_when_step_exceeds_test` |
| T5.10 warmup 不交易 | `t5_10_warmup_orders_are_ignored_and_metrics_start_at_trade_start`:预热第 5、9 根订单被忽略,第 10 根订单在 11 成交,权益从第 10 根开始 |
| T5.11 OOS 拼接 | `t5_11_oos_stitching_compounds_across_windows`:100→110→99,回撤 -10%;`t5_10_oos_stitching_keeps_exposure_and_cash_ratio`:现金与敞口同步缩放;CLI 两次运行哈希一致 |
| T5.12 参数稳定性 | `t5_12_walk_forward_param_stability_chart_renders`:窗口表、OOS 净值、参数稳定性 SVG;`param_stability_keeps_window_order` 保持窗口顺序 |
| T5.13 训练预热 | `t5_13_training_warmup_uses_pre_range_history_only`:等价区间 `[20,30)` 只预热、`[30,40)` 才交易;预热订单被忽略,权益与首笔成交从 30/31 开始;冒烟首窗记录“没有预热历史”告警 |

## 额外覆盖

- `cost_drag_is_fees_over_average_equity_per_year`:费用拖累按平均权益与年数归一。
- `downsample_keeps_first_last_and_max_drawdown`:超过 20000 点保留首尾与最大回撤点。
- `sweep_and_walk_forward_sections_render`:报告同时渲染扫描与 walk-forward 区块。
- `repeated_sweeps_have_same_hash`、`t5_9_t5_11_t5_12_windows_stitch_and_hash_are_stable`:
  扫描与 walk-forward 的内容哈希可复现。
- `write_outputs_creates_csv_and_summary`:`sweep.csv` 与 `sweep_summary.json` 落盘。
- `spearman_matches_known_ranking`:排名相关覆盖完全正相关、完全负相关与常数序列。

## 门禁

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked --no-tests=warn
cargo test --workspace --doc --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features --locked
cargo deny check
cargo audit
cargo build --release --locked
bash scripts/check.sh --ci
```

验收结果:

- nextest:117 个测试,117 通过,0 跳过。
- doctest:5 个 crate 均为 0 个文档测试,0 失败。
- fmt、check、clippy `-D warnings`、rustdoc `-D warnings`、release build 通过。
- `cargo deny check`:advisories、bans、licenses、sources 均为 ok;仅有无匹配许可证告警。
- `cargo audit`:加载 1509 条安全公告,扫描 101 个依赖,无漏洞报告。
- `scripts/check.sh --ci` 退出码 0;脚本跳过 bench 目标与 MSRV,项目无 bench target,
  MSRV 未在本次启用。
- deny/audit 通过远程本机 SOCKS 转 HTTP 代理在线拉取公告库。

## 真实冒烟

### 扫描与样本外

```bash
target/release/quant-backtest sweep --config fixtures/sweep.toml
target/release/quant-backtest sweep --config fixtures/sweep.toml --threads 1 --out out/s5-sweep-1t
target/release/quant-backtest sweep --config fixtures/sweep.toml --threads 2 --out out/s5-sweep-2t
```

- 16 组参数,16 有效,0 非法跳过;目标 Sharpe,top_k=10。
- OOS 切分 0.7,切分下标 280,预热 40 根;IS 280 根,OOS 120 根。
- 最优参数 `fast=3, slow=13`;IS 收益 56.410%,IS Sharpe 6.6624;
  OOS 收益 16.196%,OOS Sharpe 6.7979,OOS 最大回撤 2.8678%。
- IS/OOS 排名相关 0.9845588。
- DSR 0.9785087,试验数 16,每期 Sharpe 0.3487264。
- `sweep_hash` 为 `6edce9d9b7fc6763ae426c43f816ed7dc0a774b62694ef6c094e14619f736fc2`,
  auto、1、2 线程一致。
- `result_hash` 为 `4750a59489fd2fd0a159a79fc0932d97b46abedf7a8a03d794ba1a39a31c7e00`,
  三组线程一致。
- 报告 10 章,2 个内联 SVG,无外链、无脚本;参数边际 2 个,热力图 16 格。

### Walk-forward

```bash
target/release/quant-backtest walkforward --config fixtures/walkforward.toml
target/release/quant-backtest walkforward --config fixtures/walkforward.toml --threads 1 --out out/s5-wf-1t
target/release/quant-backtest walkforward --config fixtures/walkforward.toml --threads 2 --out out/s5-wf-2t
```

- rolling,train 180、test 60、step 60、embargo 1、warmup 40;3 个完整窗口,1 个尾窗丢弃。
- 窗口 0:train 0..180,test 181..241;窗口 1:train 60..240,test 241..301;
  窗口 2:train 120..300,test 301..361。
- 每窗最优参数均为 `fast=3, slow=13`;参数稳定性序列为 fast `[3,3,3]`、slow `[13,13,13]`。
- 窗口 OOS 收益分别为 11.500%、9.859%、9.176%,Sharpe 分别为 9.0335、7.4099、6.7635。
- OOS 拼接 180 点,首点 100000,末点 133731.9561;总收益 33.732%,CAGR 80.960%,
  Sharpe 7.739821,最大回撤 2.8678%。
- DSR 0.9943426,试验数 48(16 组 × 3 窗),每期 Sharpe 0.4051207。
- `result_hash` 为 `0bee99bbe6532ad053710ada945bdd84a2ca8859d5297cee19b3488554975776`,
  auto、1、2 线程一致。
- 警告:窗口 0 无预热历史、测试末仍有未成交订单、尾窗超出数据长度被丢弃。
- 报告 10 章,5 个内联 SVG;headless Chromium 在阻断外部解析时正常渲染。

## 与设计基线的差异

- 报告 schema 升到 1.1,新增 `report_kind`、`underwater`、`equity_downsampled` 与
  `cost_drag`;未运行扫描或 walk-forward 时对应字段仍为 `null`。详见
  [004-report-walkforward.md](../docs/changes/004-report-walkforward.md)。
- 图表使用内联 SVG/CSS,不引入 ECharts;报告不输出 PDF,章节固定为 10 章。
- DSR 的 `N` 取实际运行的试验数:单次扫描为网格大小,walk-forward 为网格大小乘成功窗口数;
  跳过的窗口不计,参数相关性未校正。
- 扫描的报告行只展示前 100 名,完整 top-K 曲线仍在 `sweep_summary.json`;
  JSON 交易全量,HTML 只渲染前 500 条。
- S4 的 `sweep_hash` 基线因扫描输出加入 OOS/DSR 而变化;确定性仍由重复运行验证。
- S1–S3 的 `result_hash` 因指标新增 `cost_drag` 而变化,数据、信号与成交语义未变。

## S1–S3 回归

| 配置 | `result_hash` |
| --- | --- |
| `config/00.smoke.toml` | `d771891de94d0082f31f496a173f3645c31256c70c3b6a6d29d26b72f837af1b` |
| `config/01.risk-smoke.toml` | `00c15e27a2e05e4ecaa5e9729c01639d153d7152c1aa59f71485d7b96151d802` |
| `config/02.portfolio-smoke.toml` | `6375664e77d7f0e03dc85d51a7879d3462701fcef9aaf478c0b0fa77817995e0` |

S4 回归:`config/03.sweep.toml` 为 27 组、18 有效、9 非法跳过,
`sweep_hash=e3bb4e53b7ec362e07a207ed7a55f00a13d82b33ece9ee124f13a68d33bc8c23`。

## 未运行项与限制

- 未用生产数据或真实交易所数据验收;冒烟是 400 根合成日线。
- 未做 Firefox/WebKit 和移动端渲染矩阵,只验证 Chromium headless。
- 离线验证使用解析阻断,不等同于物理断网;HTML 本身无外链、无脚本。
- 未跑 MSRV 工具链、Miri 与 semver-checks;项目无 bench target,性能基准由
  `bench` 子命令单独执行。
- DSR 只校正搜索次数,不校正试验间相关性;报告假设章节保留该限制。

## 退出条件

- T5.1–T5.13 通过。
- fmt/check/clippy/test/doc/deny/audit/release/check 通过。
- 扫描与 walk-forward 冒烟通过,auto/1/2 线程哈希一致。
- S1–S3 回归哈希已记录,S4 扫描哈希已记录。
- 以上运行项与未运行项均已汇报。
