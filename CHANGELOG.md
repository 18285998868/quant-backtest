# Changelog

本项目遵循 Keep a Changelog。

## Unreleased

### Added

- S1:CSV 日线数据加载与校验。
- S1:双均线策略、收盘信号、下一根开盘成交、手续费/滑点/手数。
- S1:净值、回撤、基础指标、JSON 与离线 HTML 报告。
- S1:自动线程检测与合成吞吐基准。
- S2:止损/止盈,支持 `intrabar` 与 `close` 两种触发。
- S2:跳空、同 bar 双触发、入场 bar 触发、风险退出优先与 `ambiguous_bar` 告警。
- S2:风险退出部分成交顺延、零成交量顺延、加减仓不重置风险价。
- S3:多标的共享账户,先卖后买、风险退出 > 减仓 > 开仓/加仓的开盘成交顺序。
- S3:截面动量 `xs_momentum`,支持 `top_k`、逆波动率权重、调仓周期与 σ 无效回退等权告警。
- S3:`Strategy::take_warnings`,策略告警进入结果与元数据。
- S3:减仓/平仓不受保证金约束,避免浮亏账户无法止损。
- S4:参数扫描 `[sweep]`,支持 `sharpe/cagr/calmar` 目标、笛卡尔积、非法组合跳过与 top-K。
- S4:`sweep` 子命令,两阶段执行,输出 `sweep.csv` 与 `sweep_summary.json`。
- S4:扫描结果规范化 `sweep_hash`,排除线程数、时间与内存等运行时字段。
- S4:`bench --symbols`,按 symbol-bar 计吞吐,输出每任务耗时与外推时间。
- S4:S4 冒烟配置 `config/03.sweep.toml` 与验收文档 `STEP.d/08-s4-acceptance.md`。
- S5:指标补全 `cost_drag`、DSR(含正态 CDF/分位数自实现)、空值规则与已知值测试。
- S5:引擎新增 `start_index`/`trade_start_index`/`end_index`,预热只喂策略、不交易、不计入指标。
- S5:`sweep` 接入 70/30 样本外、Spearman 排名相关、DSR、参数边际效应与两参数热力图。
- S5:报告 schema 1.1、10 章离线 HTML、`validate_report_json`、下采样与 `result.json.sweep` 接入。
- S5:walk-forward 纯函数与 `walkforward` 子命令,支持 rolling/anchored、embargo、warmup、OOS 拼接与参数稳定性。
- S5:夹具 `fixtures/s5_daily.csv`、`fixtures/sweep.toml`、`fixtures/walkforward.toml`、`fixtures/report_case.json`。
- S5:变更说明 `docs/changes/004-report-walkforward.md` 与验收文档 `STEP.d/09-s5-acceptance.md`。
- S6:walk-forward 参数与窗口下标改用 checked 算术,极端配置返回 Config 错误而不是 panic。
- S6:显式内存上限超过检测值时写入 warning;负数、NaN 与 Inf 在配置或 CLI 覆盖层拒绝。
- S6:CLI 契约测试覆盖退出码 2/3、错误字段、零成交量合法、非法夹具与跨线程结果哈希。
- S6:新增 `fixtures/invalid/*`、MIT/Apache-2.0 许可证、第三方依赖声明与 S6 验收文档。
- M1:配置接受 `1m/5m/15m/30m/1h/4h/1d/1w` 及同单位正整数周期,引擎语义不变。
- M1:年化因子按 `timeframe × asset_class` 推导,`periods_per_year` 可覆盖。
- M1:报告使用实际周期,假设与月度说明不再写死日线。
- M1:CLI 输出数据常驻内存估算,超过内存上限时告警。
- M1:新增 `fixtures/intraday_1h.csv`、`fixtures/weekly.csv`、`config/04.intraday.toml`、`config/05.weekly.toml`。
- M1:新增 `timeframe_contract` 集成测试与 `STEP.d/11-m1-acceptance.md`。
- M2:新增顶层 `[source]`,直接读取 Binance spot/UM/CM klines 与 Dukascopy bid/ask 的 `1m/1h/1d`,与 `run.data` 二选一。
- M2:归档时间戳统一转换为 bar 收盘时刻;日期窗口前移一个周期,避免跨日/跨周 bar 丢失;缺失文件只进入诊断。
- M2:`backtest-data` 通过 path 依赖同级 `data-reader`,CSV 与归档共用时间线校验和 `data_hash`;CLI 打印 `数据源:` 摘要。
- M2:新增 `STEP.d/12-archive-source.md`、`docs/changes/006-archive-source.md`;`deny.toml` 允许 Zlib。
- M3:Binance `futures/um`/`futures/cm` 默认按归档 `fundingRate` 结算资金费;`spot`/Dukascopy 默认关闭,`source.funding` 可显式覆盖,现货误开在配置阶段报错。
- M3:稀疏 `FundingSeries`;结算窗口 `(上一根 bar 收盘, 本根 bar 收盘]`,本根收盘价计名义价值,正费率多头支付;归档 `calc_time` 的亚秒偏移归一为整秒。
- M3:`BacktestResult.funding`(未启用时 JSON 省略)、报告 schema 1.2、HTML 资金费成本与按标的明细、walk-forward 跨窗口合并。
- M3:启用资金费时 `data_hash` 追加 `quant-backtest/funding/v1` 段;未启用时 spot/CSV 哈希不变,`config_hash` 不纳入开关;M4 把资金费段升级为 v2,见下。
- M3:新增 `STEP.d/13-funding.md` 与 `docs/changes/007-funding.md`。
- M4:`run.chunk_bars` 显式分块;`memory_limit_gb > 0` 且全量估算超限时自动分块;`run`/`validate`/`sweep` 接入,`walkforward` 保持整段。
- M4:窗口按绝对下标连续推进,块首携带只读策略历史;`EngineState` 携带现金、持仓、挂单、目标权重、权益、成交、交易、告警、资金费光标与下一根下标。
- M4:数据哈希升级为 v2 固定页 `PAGE_BARS = 65_536`,支持时间分块汇总;资金费段同步升为 `quant-backtest/funding/v2`。
- M4:CSV/归档读取新增 `DataPlan`、`plan_universe_reporting()` 与 `load_window()`;CLI 输出全量与单窗口内存估算。
- M4:新增 `crates/backtest-data/src/hash.rs`、`crates/backtest-cli/src/stream.rs`、`STEP.d/14-chunking.md` 与 `docs/changes/008-chunked-streaming.md`。
- M5:`walkforward` 接入 `chunk_bars`/`memory_limit_gb` 分块,训练网格与 OOS 测试共用 `sweep::Data` 与 `run_single` 路径。
- M5:`walkforward::execute` 改为接收 `sweep::Data`,窗口规划、时间戳与 `data_hash` 不再依赖整段 `Universe`;无新增配置、CLI 或报告字段。
- M5:新增整段/分块 walkforward 单测与 CLI 契约测试,覆盖窗口参数、OOS 净值、参数稳定性、DSR 与资金费;新增 `STEP.d/15-walkforward-chunking.md` 与 `docs/changes/009-walkforward-chunking.md`。
- M6:`content_hash` 改为流式规范序列化,按旧 `serde_json::Value` 的字典序直接写入 SHA-256,不再构建整棵结果树;排除的运行时字段与 `funding` 省略规则不变。
- M6:`write_json` 改用 `BufWriter` 与 `to_writer_pretty`;`write_report` 只计算一次 `result_hash` 并复用于 JSON 与 HTML。
- M6:新增旧实现逐字节对比测试;1843200 bar 样本峰值 RSS 从约 2088 MB 降到约 413 MB,整段/分块 `result_hash` 与历史冒烟哈希不变;新增 `STEP.d/16-report-streaming-hash.md` 与 `docs/changes/010-report-streaming-hash.md`。
- M7:`plan_archive()` 在归档、`start`/`end` 有界且周期短于一天时按 UTC 月分窗规划,逐窗只保留单标的单月 `RawBar`;无界区间、日线及以上、窗口数 `<= 1` 与参考标的无数据回退整段。
- M7:资金费一次性装载后按 `(首根开盘, 末根收盘]` 过滤,诊断按标的累计月窗口;不修改 `data-reader`,无新增配置、CLI 或报告字段,`data_hash`/`result_hash` 不变。
- M7:1843200 bar 样本 `validate` 峰值 RSS 从 413.1 MB 降到 38.1 MB,分块 `run` 从 413.7 MB 降到 209.8 MB;新增 `STEP.d/17-monthly-archive-planning.md` 与 `docs/changes/011-monthly-archive-planning.md`。
- M8:`sweep_hash` 改为流式规范序列化,不再构建 summary `serde_json::Value` 树;`sweep_summary.json` 改为 `BufWriter` 流式写入。
- M8:网格与 OOS 新增 metrics-only 回放 `run_backtest_stats()`/`run_backtest_stream_stats()`,只保留指标、逐期 Sharpe 与告警;top-K 仍生成完整结果。
- M8:归档按标的并行装载;`RawBar` 去掉 `symbol` 字符串,`finalize()` 直接按标的分桶构造 `BarSeries`;`monthly_returns()` 改为单遍月边界推进。
- M8:W1–W4 端到端耗时降 27%–42%,峰值 RSS 降 56%–73%,`result_hash`/`sweep_hash` 与历史哈希不变;新增 `STEP.d/18-sweep-metrics-performance.md` 与 `docs/changes/012-sweep-metrics-performance.md`。
- M8:曲线降采样(P6)未实施,本轮跳过;该改动会改变 `sweep_hash`,需 schema 1.3 与新的哈希基线,留待单独立项。
- M8-B:`compute_metrics_with_period_sharpe()` 合并为单次权益遍历,同时累计收益、回撤、回撤段落、均价与暴露;样本标准差保留第二次等价遍历。
- M8-B:删除 `series_drawdowns()` 与收益/回撤临时向量,单曲线约 20.6 MB 临时内存归零;新增新旧实现逐位对比测试。
- M8-B:W2 中位耗时 4.38→3.67 s,W3 20.73→15.00 s,`result_hash`/`sweep_hash` 与历史哈希不变;新增 `STEP.d/19-single-pass-metrics.md` 与 `docs/changes/013-single-pass-metrics.md`。
- M9:新增 `Universe::close()`/`try_close()`,`mark_close()`、`apply_funding()` 与内置策略改为只读收盘价列,不再为每根 bar 构造 `BarView`。
- M9:合成基准 64 标的吞吐从约 191M 提升到约 496M bar步/秒(2.60 倍),256 标的 2.07–2.19 倍,单标的空策略持平;W4 端到端中位 14.38→13.34 s,`result_hash`/`sweep_hash` 与历史哈希不变;新增 `STEP.d/20-multi-symbol-marking.md` 与 `docs/changes/014-multi-symbol-marking.md`。
- M11-C:新增 `backtest-core::analysis`,内置 SplitMix64、stationary bootstrap 与百分位置信区间,覆盖均值、波动率、Sharpe、CAGR 与最大回撤;每个重采样样本独立派生子种子,结果与线程数无关。
- M11-C:新增 `[analysis]`(`enabled`、`seed`、`bootstrap_samples`),默认关闭;关闭时 `config_hash`、`result_hash` 与报告 schema 1.2 逐位不变。
- M11-C:`run` 启用分析时报告升级为 schema 1.3,在第 9 章输出 bootstrap 子节,并写入独立的 `analysis_hash`;基础 `result_hash` 仍与关闭分析时一致。
- M11-C:`sweep`/`walkforward` 遇到 `analysis.enabled = true` 会明确告警并跳过分析,PBO/SPA/plateau 留给后续阶段。
- M11-C:新增 `docs/changes/015-analysis-module.md` 与 `crates/backtest-core/src/analysis/`。

### 验收

- 2026-11-16:60 个测试、fmt、clippy、doc、deny、audit 与 release 冒烟全部通过。
- 缺失 bar 由数据加载加引擎集成测试覆盖;零成交量 bar 由引擎单测覆盖。
- S1 冒烟二次运行 `result_hash` 一致;2 线程空策略吞吐 50,175,123 bar 步/秒。
- S2 风险冒烟 `result_hash` 为 `cd330ad9d98802e9d3492a61da4d24fcf5b17f1e51cba8b718d8a72e7a25979e`,止损退出 1 笔。
- 2026-11-16:S3 验收,82 个测试、fmt、clippy、doc、deny/audit(离线)与 release 冒烟全部通过。
- S3 组合冒烟 2 个标的、4 笔交易、10 笔成交、0 条告警;二次运行 `result_hash` 一致:`d84b79855eae8f066b0ec6024673f8df4cffed2b7029ff9c119c84bb3ba8b1be`。
- S1/S2 回归哈希保持不变:`efbb00b9...`、`cd330ad9...`。
- 2026-11-16:S4 验收,92 个测试、fmt、clippy、doc、deny/audit(离线)与 release 冒烟全部通过。
- S4 扫描冒烟 27 组、18 有效、9 组非法跳过,`sweep_hash` 为 `9fe1c058...`,auto/1/2 线程一致。
- S1/S2/S3 回归哈希保持不变:`efbb00b9...`、`cd330ad9...`、`d84b7985...`。
- 2026-11-16:S6 验收,130 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit 与 release build 全部通过。
- 10 个非法数据夹具退出码 3,非法配置与 CLI 覆盖退出码 2,均含字段上下文且无 panic。
- S1 冒烟 1/2 线程 `result_hash` 同为 `efbb00b9...`;S2/S3 回归哈希保持不变。
- 干净克隆的测试、release 构建与 validate/run/portfolio/sweep/walkforward 全部成功;
  sweep 与 walkforward 哈希分别为 `41d11ac9...`、`4f0e7bd6...`,与 S5 记录一致。
- 密钥与个人绝对路径扫描无命中;`cargo audit` 扫描 101 个依赖无漏洞报告。
- 2026-11-16:M2 验收,151 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit 与 release 构建全部通过。
- Binance BTCUSDT 1h 归档与等价 CSV 的 `data_hash`、`result_hash` 完全一致;
  1m 2023-06 为 43200 根,4 标的 1h 组合为 744 根,Dukascopy EURUSD H1 为 743 根。
- M2 归档冒烟 1/2 线程与重复运行 `result_hash` 一致;`cargo audit` 扫描 131 个依赖无漏洞报告。
- 提交 `22979ff` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked` 与 `cargo build --release --locked`。
- 2026-11-16:M3 验收,159 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit、release 构建与 `scripts/check.sh --ci` 全部通过。
- BTCUSDT UM 1h 2024-01:744 根 bar、93 次区间内结算、87 次扣费,
  净支出 `-34.92654974212712`;独立复算差 `1.8e-13`,1/2 线程 `result_hash` 同为 `a8d5b41e...`。
- 4 标的组合 179 次结算,`by_symbol` 成本与次数之和等于总账,
  1/2 线程 `result_hash` 同为 `54259a53...`。
- `funding = false` 结果无资金费字段且 `config_hash` 不变;spot 冒烟保持 `efbb00b9...`。
- 提交 `558be63` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(159 个)与 `cargo build --release --locked`;
  `fund_1h` 与 spot 冒烟哈希与工作副本一致。
- 2026-11-16:M1 验收,138 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit、release build 与 `scripts/check.sh --ci` 全部通过。
- 1h 冒烟 `result_hash` 为 `7c680a05...`,年化因子 8760,2 笔交易;1w 冒烟 `result_hash` 为 `7c4c0da1...`,年化因子 52。
- 1h 扫描 `sweep_hash` 为 `93ea6a6d...`(4 组/3 有效/1 跳过),walk-forward `result_hash` 为 `0a6e9982...`(2 窗口/2 跳过)。
- S1 日线冒烟 1/2 线程 `result_hash` 保持 `efbb00b9...`,周期解锁未改变日线语义。
- 干净克隆测试 138 个通过、release 构建成功;1h/1w 示例哈希与工作副本一致。
- 2026-11-16:M4 验收,172 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit、release 构建与 `scripts/check.sh --ci` 全部通过。
- BTCUSDT UM 1m 2023-06(43199 bar,`chunk_bars=5000`):整段与分块
  `data_hash` 同为 `d9dc81d8...`、`result_hash` 同为 `f648cd3e...`,2338 笔交易、89 次结算、0 条告警。
- BTC/ETH/BNB/SOL UM 1h 2024-01(744 bar,`chunk_bars=100`):整段与分块
  `data_hash` 同为 `da7521b2...`、`result_hash` 同为 `54259a53...`,198 笔交易、179 次结算、24 条告警。
- `fixtures/sweep.toml` 16 组网格整段与分块 `sweep_hash` 同为 `41d11ac9...`;
  CLI 契约测试覆盖 run 与 sweep 的全量/分块一致性。
- Binance 1h 与等价 CSV 的 v2 哈希一致:`data_hash = b527ca0a...`、
  `result_hash = c64051ce...`。
- 历史哈希重录后,`config_hash` 不变;S1/S2/S3/S4/S5/1h/1w 分别为
  `efbb00b9...`、`cd330ad9...`、`d84b7985...`、`9fe1c058...`、
  `41d11ac9...`/`4f0e7bd6...`、`7c680a05...`、`7c4c0da1...`。
- 43k bar 样本的进程峰值 RSS 由二进制基线主导(单次约 64 MB 与 69–75 MB),
  数据内存估算分别为 2.0 MiB 与每窗口 195.3 KiB;超大样本内存收益待目标机实测。
- 提交 `727f311` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(172 个)与 `cargo build --release --locked`;
  1m 与 4 标的 1h 的整段/分块哈希、spot 冒烟 `efbb00b9...` 均与工作副本一致。
- 2026-11-16:M5 验收,174 个测试、fmt、check、clippy、release 构建与
  `scripts/check.sh --ci` 全部通过。
- `fixtures/walkforward.toml`(180/60/60/1,`chunk_bars=50`):整段与分块
  `result_hash` 同为 `4f0e7bd6...`,3 个窗口、OOS 净值/指标、参数稳定性、
  DSR 与跳过窗口一致。
- BTCUSDT UM 1m 2023-06(43199 bar,`chunk_bars=5000`):整段与分块
  `result_hash` 同为 `0fd37671...`、`data_hash` 同为 `d9dc81d8...`,
  4 个窗口、1033 笔 OOS 交易与 42 次资金费结算一致。
- BTC/ETH/BNB/SOL UM 1h 2024-01(744 bar,`chunk_bars=100`):整段与分块
  `result_hash` 同为 `9981eeaf...`、`data_hash` 同为 `da7521b2...`,
  3 个窗口、86 笔交易与 90 次 OOS 资金费结算一致。
- 显式 `chunk_bars` 大于数据长度时回退整段,哈希与整段一致;
  自动分块路径由共享 `choose_chunk()` 单测覆盖。
- 1m walk-forward 分块耗时约为整段的 19.5 倍(2.706 s 对 0.139 s),
  以时间换内存;43k bar 样本 RSS 由装载峰值主导,不用于证明内存收益。
- 提交 `1a47bb6` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(174 个)与 `cargo build --release --locked`;
  夹具、1m 与 4 标的 1h 的整段/分块 `result_hash`、`data_hash`、
  窗口参数与 OOS 指标均与工作副本一致。
- 2026-11-16:M6 验收,175 个测试、fmt、check、clippy、nextest、doctest、
  rustdoc、deny、audit、release 构建与 `scripts/check.sh --ci` 全部通过。
- Binance spot BTCUSDT 1m 2023-05 至 2026-10(1843200 bar):
  整段峰值 RSS 从 2087.8 MB 降到 412.6 MB,分块从 2000.1 MB 降到 412.8 MB;
  `result_hash` 同为 `f7026895...`、`data_hash` 同为 `36ec7667...`,
  22097 笔交易与 44194 笔成交不变。
- 改造后的 `result.json` 除 `meta.generated_at` 外与改造前逐字段相同,
  文件大小同为 12,739,787 字节;`content_hash` 由旧实现逐字节对比测试兜底。
- S1/S2/S3/1h/1w、walk-forward 与 sweep 冒烟哈希保持不变:
  `efbb00b9...`、`cd330ad9...`、`d84b7985...`、`7c680a05...`、
  `7c4c0da1...`、`4f0e7bd6...`、`41d11ac9...`。
- BTCUSDT UM 1m 2023-06 与 4 标的 1h 2024-01 用 M6 二进制复跑整段/分块,
  `result_hash` 仍为 `f648cd3e...`、`54259a53...`,`data_hash` 仍为
  `d9dc81d8...`、`da7521b2...`。
- 改造后报告阶段不再抬高进程峰值;剩余约 413 MB 由归档规划阶段
  `plan_archive()` 的整段 `load_universe_reporting()` 主导。
- 提交 `5f4a963` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(175 个)与 `cargo build --release --locked`;
  大样本整段/分块复跑的 `result_hash`、`data_hash` 与工作副本一致。
- 2026-11-16:M7 验收,181 个测试、fmt、check、clippy、nextest、doctest、
  rustdoc、deny、audit、release 构建与 `scripts/check.sh --ci` 全部通过。
- Binance spot BTCUSDT 1m 2023-05 至 2026-10(1843200 bar):`validate`
  峰值 RSS 从 413.1 MB 降到 38.1 MB,耗时 0.825 s → 1.957 s;分块
  `run` 从 413.7 MB 降到 209.8 MB,耗时 2.486 s → 3.885 s;
  `data_hash` 与 `result_hash` 不变。
- 4 标的 UM 1h 2024-01..2024-03 的 M6/M7、整段/分块 `result_hash`
  同为 `7f823905...`、`data_hash` 同为 `376a13c5...`,589 笔交易、
  538 次结算与 67 条告警一致;Dukascopy EURUSD 1h 2023-05..2023-08
  的 3 个缺失月只进入诊断。
- 历史哈希保持不变:`efbb00b9...`、`cd330ad9...`、`d84b7985...`、
  `7c680a05...`、`7c4c0da1...`、`f648cd3e...`/`d9dc81d8...`、
  `54259a53...`/`da7521b2...`、`4f0e7bd6...`、`41d11ac9...`。
- 提交 `9782a68` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(181 个)与 `cargo build --release --locked`;
  大样本 `validate` 与多标的整段/分块哈希与工作副本一致。
- 2026-11-17:M8 验收,184 个测试、fmt、clippy、release 构建与
  `scripts/check.sh --ci` 全部通过。
- W1–W4 对比 M7 二进制:耗时 6.4→3.8 s、6.0→4.2 s、29→21 s、
  17.7→10.3 s;峰值 RSS 2850→975 MB、2700→720 MB、4250→1295 MB、
  6030→2645 MB。
- W1–W4 的 `sweep_hash` 分别为 `1bf1e216...`、`32f6ee94...`、
  `e19d0b73...`、`4a3f718e...`,与 M7 逐位一致。
- 历史哈希保持不变:`efbb00b9...`、`cd330ad9...`、`d84b7985...`、
  `7c680a05...`、`7c4c0da1...`、`f7026895...`/`36ec7667...`、
  `7f823905...`/`376a13c5...`、`4f0e7bd6...`、`41d11ac9...`。
- 提交 `d89b111` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(184 个)与 `cargo build --release --locked`;
  冒烟、walk-forward、sweep、1m 与 4 标的 1h 哈希与工作副本一致。
- 2026-11-17:M8-B 验收,185 个测试、fmt、clippy、release 构建与
  `scripts/check.sh --ci` 全部通过。
- W2/W3 对比 M8 二进制:中位耗时 4.38→3.67 s、20.73→15.00 s;
  W2 峰值 729→661 MB,W3 峰值基本持平。
- W2/W3 的 `sweep_hash` 仍为 `32f6ee94...`、`e19d0b73...`;历史哈希
  `efbb00b9...`、`cd330ad9...`、`d84b7985...`、`7c680a05...`、
  `7c4c0da1...`、`f7026895...`/`36ec7667...`、`7f823905...`/`376a13c5...`、
  `4f0e7bd6...`、`41d11ac9...` 全部不变。
- 提交 `2b8f032` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(185 个)与 `cargo build --release --locked`;
  冒烟、sweep、walk-forward、1m 与 4 标的 1h 哈希与工作副本一致。
- 2026-11-17:M9 验收,186 个测试、fmt、clippy、release 构建与
  `scripts/check.sh --ci` 全部通过。
- 合成基准对比 M8-B 二进制:64 标的 2.60 倍、256 标的 2.19 倍(空策略),
  单标的空策略持平;W4 端到端中位耗时 14.38→13.34 s,峰值基本持平。
- 历史哈希 `efbb00b9...`、`cd330ad9...`、`d84b7985...`、`7c680a05...`、
  `7c4c0da1...`、`f7026895...`/`36ec7667...`、`7f823905...`/`376a13c5...`、
  `4f0e7bd6...`、`41d11ac9...` 与 W4 的 `4a3f718e...` 全部不变。
- 提交 `e03d2d8` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(186 个)与 `cargo build --release --locked`;
  冒烟、sweep、walk-forward、1m 与 4 标的 1h 哈希与工作副本一致。
- 2026-11-17:M11-C 验收,208 个测试、fmt、clippy、release 构建与
  `scripts/check.sh --ci` 全部通过。
- 分析冒烟(`fixtures/s5_daily.csv`,400 根 1d bar,bootstrap 500 次):
  `schema_version` 1.3,`result_hash` 为
  `56fef2bc99124558f23d23c3589c85fbe30eed9e8e4832b9f6ec9770e3c74500`,
  `analysis_hash` 为
  `c1218836b04437fc047de62670d340896798896abfeb7fa16072bd03d247e901`;
  同一配置关闭 `[analysis]` 后仍为 schema 1.2 且 `result_hash` 逐位相同。
- 分析冒烟 1 线程与 2 线程的 `analysis_hash` 一致;release 二进制连续两次
  运行的 `result_hash` 与 `analysis_hash` 均一致;引擎 `config_hash`
  `c36534db...` 与 M9 一致。
- 2026-11-17:M11-A 验收,218 个测试、fmt、clippy、cargo doc 与 release 构建通过;
  `scripts/check.sh --ci` 除网络不可达的 `cargo deny` 外全部通过。
- M11-A:新增 `backtest-core::analysis::pbo`,实现 CSCV/PBO,覆盖 Sharpe 排名、
  logit 分位数与样本内最优标签;`N < min_trials` 与组合数过多时告警。
- M11-A:新增 `[analysis].pbo_blocks`(默认 10,偶数 `4..=20`)与
  `[analysis].min_trials`(默认 20,`>=2`);`analyze_sweep` 剔除长度不一致、空行与
  非有限收益的试验并告警。
- M11-A:`sweep` 启用分析时采集每个网格 trial 的 UTC 日收益,计算 PBO 并写入
  `sweep_summary.json` 的 `analysis`;schema 1.1→1.2,`sweep_hash` 与关闭分析时
  逐位相同。
- M11-A:新增 `metrics::daily_equity_returns`,按 UTC 日界聚合权益,支持日内
  周期压缩到日频;`EngineInput.collect_daily_returns` 关闭时零分配。
- M11-A:报告第 9 章新增 PBO 子节,含 PBO、组合数、试验数、观测数、logit
  分位数与“CSCV 近似”注记;`validate_report_json` 校验 `analysis.pbo`。
- M11-A:冒烟(`fixtures/s5_daily.csv`,16 组参数):关闭分析 schema 1.1;开启分析
  schema 1.2,`pbo=0.0`,16 个试验,252 个组合;1 线程与 2 线程
  `analysis_hash` 一致。
- 2026-11-17:M11-D 验收,143 个 core 测试、fmt、clippy、cargo doc 与 release 构建通过。
- M11-D:新增 `backtest-core::analysis::plateau`,计算二维目标值矩阵的平台得分、
  best、robust 与孤立尖峰判定;`plateau_tolerance` 默认 0.10。
- M11-D:`sweep` 启用分析时,基于 heatmap 的二维网格计算平台结果并写入
  `analysis.plateau`;报告第 9 章新增“参数平台”子节。
- M11-D:新增 `[analysis].plateau_tolerance`(正有限数);非正数配置被拒绝。
