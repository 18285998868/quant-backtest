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
