# 04 CLI、系统检测与基准

## 目标

把配置、数据、引擎、报告串成命令,并按机器自动决定线程数。

## 命令

| 命令 | 输入 | 输出 |
| --- | --- | --- |
| `validate` | `--config` | 校验结果、检测值、哈希 |
| `run` | `--config`、可选 `--threads`、`--memory-limit-gb`、`--out` | JSON、HTML、摘要 |
| `sweep` | `--config`、可选 `--threads`、`--memory-limit-gb`、`--out` | `sweep.csv`、`sweep_summary.json`、摘要 |
| `bench` | 可选 `--threads`、`--scale`、`--tasks`、`--bars`、`--symbols` | 吞吐、线程数、检测值 |

## 自动线程

1. `available_parallelism()`。
2. cgroup v2 `cpu.max`,取配额与检测值的较小值。
3. cgroup v2/v1 内存上限或 `/proc/meminfo` 的 `MemAvailable`。
4. 预留 `max(2 GB, 10%)`。
5. 单 worker 预算 64 MiB。
6. 显式 `--threads` 覆盖,超过检测值时告警但不静默修改。
7. `--memory-limit-gb` 低于单 worker 预算时返回配置错误。

## 参数扫描

- 读取 `[sweep]`,按键序展开笛卡尔积;值只支持标量。
- 先并行跑全部组合,只保留指标、参数、配置哈希与告警数。
- 按 `sharpe/cagr/calmar` 降序排序,同值按组合展开顺序。
- 只重跑 top-K,保存权益、成交、交易、回撤、月收益与告警,写入 `sweep_summary.json`。
- 全部有效组合写入 `sweep.csv`,列含目标值、指标、告警数、配置哈希与 `param:*`。
- 非法组合跳过并计数;全部非法返回配置错误。
- `sweep_hash` 只覆盖 schema、目标、top_k、oos_split、config_hash、data_hash、grid、counts 与 top-K 结果,不覆盖线程数、生成时间、耗时与峰值内存。
- `--threads` 与 `--memory-limit-gb` 覆盖配置;内存不足一个 worker 时在启动前报错。

## 基准

- 合成任务 × bar × 标的;空策略与 SMA 各跑 3 次。
- 输出 symbol-bar/秒、线程数、检测到的 CPU/内存、每任务耗时与 1.25e9 步外推时间。
- `--scale` 跑 1/2/4/8 线程。

## 测试

- 合成 CPU=2、内存充足时 auto=2。
- `--threads 1` 覆盖为 1。
- 无 cgroup 时回退 `available_parallelism()` 并告警。
- 吞吐门槛 2 核 >= 1e6 bar 步/秒。
- 扫描串并行指标一致、重复哈希一致、top-K 数量与输出文件。
- 非法组合跳过、全部非法报错、低内存上限报配置错误。

## 验收

`bench --scale` 输出各档吞吐;`sweep --config config/03.sweep.toml` 输出两份产物与 `sweep_hash`;`run` 退出码 0 并打印产物路径。

## 依赖

依赖 01–03。
