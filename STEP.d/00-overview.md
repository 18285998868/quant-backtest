# 00 总览

S1 的目标是打通单标的、日线、双均线的端到端链路,同时把多标的与做空的会计结构预留好。

## 数据流

| 阶段 | 输入 | 输出 |
| --- | --- | --- |
| 配置 | TOML | `RunConfig` |
| 数据 | CSV + `RunConfig` | `Universe` + `data_hash` |
| 回测 | `Universe` + `Strategy` | `BacktestResult` |
| 指标 | 净值/成交/交易 | `Metrics`、回撤段、月收益 |
| 报告 | `BacktestResult` | `result.json`、`report.html` |

## 不变量

- 信号只读取当前与历史 bar。
- 收盘生成目标权重,下一根可用 bar 开盘成交。
- 成交后 `cash >= 0` 且 `equity >= required_margin`。
- 结果与线程数无关。
- 报告中不出现 NaN/Inf。

## 失败模式

| 阶段 | 失败 | 处理 |
| --- | --- | --- |
| 配置 | 缺字段、越界、未知策略 | `ConfigError`,退出码 2 |
| 数据 | 空、乱序、重复、OHLC 非法、时间线不一致 | `DataError`,退出码 3 |
| 执行 | 非法权重、总敞口超限、账本不变量破坏 | `ExecutionError`,退出码 1 |
| 报告 | 目录/写入/序列化失败 | `ReportError`,退出码 1 |

## 依赖关系

01 配置与数据不依赖引擎;02 引擎依赖 01;03 指标与报告依赖 02 的结果类型;04 CLI 编排 01–03;05 只做验收,不引入新功能。
