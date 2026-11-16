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

### 验收

- 2026-11-16:60 个测试、fmt、clippy、doc、deny、audit 与 release 冒烟全部通过。
- 缺失 bar 由数据加载加引擎集成测试覆盖;零成交量 bar 由引擎单测覆盖。
- S1 冒烟二次运行 `result_hash` 一致;2 线程空策略吞吐 50,175,123 bar 步/秒。
- S2 风险冒烟 `result_hash` 为 `8476c2288d3f99a60c9b0ce53f09427e8ac25cb8f3e521194eb15ede8bc4d571`,止损退出 1 笔。
- 2026-11-16:S3 验收,82 个测试、fmt、clippy、doc、deny/audit(离线)与 release 冒烟全部通过。
- S3 组合冒烟 2 个标的、4 笔交易、10 笔成交、0 条告警;二次运行 `result_hash` 一致:`8796ce7f400ccbd6a30592c2a98bfba0258b53f609e108a07767fc7a17e2a411`。
- S1/S2 回归哈希保持不变:`144d1118...`、`8476c228...`。
- 2026-11-16:S4 验收,92 个测试、fmt、clippy、doc、deny/audit(离线)与 release 冒烟全部通过。
- S4 扫描冒烟 27 组、18 有效、9 组非法跳过,`sweep_hash` 为 `3e92bf60...`,auto/1/2 线程一致。
- S1/S2/S3 回归哈希保持不变:`144d1118...`、`8476c228...`、`8796ce7f...`。
- 2026-11-16:S6 验收,130 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit 与 release build 全部通过。
- 10 个非法数据夹具退出码 3,非法配置与 CLI 覆盖退出码 2,均含字段上下文且无 panic。
- S1 冒烟 1/2 线程 `result_hash` 同为 `d771891d...`;S2/S3 回归哈希保持不变。
- 干净克隆的测试、release 构建与 validate/run/portfolio/sweep/walkforward 全部成功;
  sweep 与 walkforward 哈希分别为 `6edce9d9...`、`0bee99bb...`,与 S5 记录一致。
- 密钥与个人绝对路径扫描无命中;`cargo audit` 扫描 101 个依赖无漏洞报告。
- 2026-11-16:M2 验收,151 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit 与 release 构建全部通过。
- Binance BTCUSDT 1h 归档与等价 CSV 的 `data_hash`、`result_hash` 完全一致;
  1m 2023-06 为 43200 根,4 标的 1h 组合为 744 根,Dukascopy EURUSD H1 为 743 根。
- M2 归档冒烟 1/2 线程与重复运行 `result_hash` 一致;`cargo audit` 扫描 131 个依赖无漏洞报告。
- 提交 `22979ff` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked` 与 `cargo build --release --locked`。
- 2026-11-16:M1 验收,138 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit、release build 与 `scripts/check.sh --ci` 全部通过。
- 1h 冒烟 `result_hash` 为 `68707c00...`,年化因子 8760,2 笔交易;1w 冒烟 `result_hash` 为 `fee172be...`,年化因子 52。
- 1h 扫描 `sweep_hash` 为 `359ad35d...`(4 组/3 有效/1 跳过),walk-forward `result_hash` 为 `ca243a95...`(2 窗口/2 跳过)。
- S1 日线冒烟 1/2 线程 `result_hash` 保持 `d771891d...`,周期解锁未改变日线语义。
- 干净克隆测试 138 个通过、release 构建成功;1h/1w 示例哈希与工作副本一致。
