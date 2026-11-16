# 04 CLI、系统检测与基准

## 目标

把配置、数据、引擎、报告串成命令,并按机器自动决定线程数。

## 命令

| 命令 | 输入 | 输出 |
| --- | --- | --- |
| `validate` | `--config` | 校验结果、检测值、哈希 |
| `run` | `--config`、可选 `--threads`、`--memory-limit-gb`、`--out` | JSON、HTML、摘要 |
| `bench` | 可选 `--threads`、`--scale` | 吞吐、线程数、检测值 |

## 自动线程

1. `available_parallelism()`。
2. cgroup v2 `cpu.max`,取配额与检测值的较小值。
3. cgroup v2/v1 内存上限或 `/proc/meminfo` 的 `MemAvailable`。
4. 预留 `max(2 GB, 10%)`。
5. 单 worker 预算 64 MiB。
6. 显式 `--threads` 覆盖,超过检测值时告警但不静默修改。
7. `--memory-limit-gb` 低于单 worker 预算时返回配置错误。

## 基准

- 合成 500 个任务 × 2500 bar。
- 空策略与 SMA 各跑 3 次。
- 输出 bar 步/秒、线程数、检测到的 CPU/内存。
- `--scale` 跑 1/2/4/8 线程。

## 测试

- 合成 CPU=2、内存充足时 auto=2。
- `--threads 1` 覆盖为 1。
- 无 cgroup 时回退 `available_parallelism()` 并告警。
- 吞吐门槛 2 核 >= 1e6 bar 步/秒。

## 验收

`bench --scale` 输出各档吞吐;`run` 退出码 0 并打印产物路径。

## 依赖

依赖 01–03。
