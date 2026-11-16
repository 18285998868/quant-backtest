# 003 参数扫描命令

## 动机

S4 需要在不复制引擎逻辑的前提下完成参数网格、并行回测、目标排序与 top-K 输出。
设计基线把扫描放在 `sweep` 模块并暴露 `run_sweep()`；但扫描需要构造具体策略，
而 `backtest-core` 不能反向依赖 `backtest-strategy`，否则形成循环依赖。

首版把编排放在 `backtest-cli` 的 `sweep` 子命令：CLI 已同时依赖 core、data、strategy 与 report，
可以在不改动引擎契约的情况下完成 S4。等 S5 的 walk-forward 需要复用扫描时，
再把纯网格展开、排序与哈希抽到 core，或引入只依赖 core 的扫描编排层。

## 影响范围

- `RunConfig` 新增可选 `sweep: Option<SweepConfig>`；缺省时行为不变。
- 新增 `[sweep]` 段：`objective` 默认 `sharpe`，`top_k` 默认 20，`oos_split` 默认 0.7；
  `[sweep.grid]` 至少一个参数，值只允许标量。
- `config_hash` 不包含 `[sweep]`；单个组合的参数覆盖会重新计算 `config_hash`。
- 新增 `quant-backtest sweep --config <sweep.toml> [--threads N] [--memory-limit-gb G] [--out DIR]`。
- 输出 `sweep.csv`（全部有效组合，按目标降序）与 `sweep_summary.json`（元数据、计数、top-K 完整曲线）。
- 扫描分两阶段：先并行只算指标，排序后仅重跑 top-K 保存权益、成交、交易、回撤、月收益与告警。
- 非法组合跳过并计数；全部非法时报错。类型错误仍按配置错误返回，不静默跳过。
- `sweep_hash` 排除生成时间、耗时、峰值内存、线程数与 CPU/内存检测值，只覆盖可复现字段。
- `bench` 新增 `--symbols`，吞吐按 symbol-bar 计，并输出每任务耗时与 1.25e9 步外推时间。
- `result.json` 的 `sweep` 字段仍为 `null`；独立扫描结果不写入单次回测报告，S5 再接入报告契约。

## 替代方案

1. 在 `backtest-core` 实现 `run_sweep()`。需要 core 依赖策略构造，形成循环；拆 trait 会增加首版抽象成本，暂不做。
2. 一次运行保留全部组合的完整曲线。1000 组 × 2500 bar 会放大内存，无法满足 T4.4 的 top-K 内存目标，弃用。
3. 单阶段只保留 top-K，不回写全量 CSV。无法审计未入选组合，也难核对目标排序，弃用。
4. 把线程数、耗时写入 `sweep_hash`。同一配置换线程会改变哈希，违反 T4.2 与 T4.8，弃用。
5. 只支持 `sharpe` 目标。CAGR/Calmar 是验收明确项，弃用。

## 回退计划

删除 `SweepConfig`、`[sweep]` 解析、`sweep` 子命令、`sweep.rs` 与 `bench --symbols` 即可；
不改变数据 schema、策略 trait、成交顺序或报告 `schema_version`。
