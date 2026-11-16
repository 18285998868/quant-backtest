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

### 验收

- 2026-11-16:60 个测试、fmt、clippy、doc、deny、audit 与 release 冒烟全部通过。
- 缺失 bar 由数据加载加引擎集成测试覆盖;零成交量 bar 由引擎单测覆盖。
- S1 冒烟二次运行 `result_hash` 一致;2 线程空策略吞吐 50,175,123 bar 步/秒。
- S2 风险冒烟 `result_hash` 为 `8476c2288d3f99a60c9b0ce53f09427e8ac25cb8f3e521194eb15ede8bc4d571`,止损退出 1 笔。
