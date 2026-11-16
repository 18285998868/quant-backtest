# 08 S4 验收

## 目标

验证 `docs/03-流程设计.md` 的 T4.1–T4.9:参数网格、并行确定性、top-K、内存门禁与缩放基准。

## 用例

| 用例 | 证据 |
| --- | --- |
| T4.1 串并行一致 | `serial_and_parallel_metrics_match`:100 组参数,1 线程与 2 线程逐任务 `params`、`config_hash`、`sharpe`、`cagr`、`total_return` 完全相等,顺序一致 |
| T4.2 确定性 | `repeated_sweeps_have_same_hash`;冒烟配置 auto/1/2 线程的 `sweep_hash` 均为 `3e92bf60...` |
| T4.3 非法组合 | `invalid_combinations_are_skipped`、`all_invalid_combinations_are_an_error`;冒烟 27 组中 18 有效、9 组 `fast >= slow` 被跳过 |
| T4.4 内存 | 1000 组、top_k=20、6 bar:仅保留 20 条权益曲线,120 个权益点,保留字节估算 6,240,峰值 RSS 41,312,256 |
| T4.5 缩放基准 | 1000 任务 × 2500 bar:2 线程 SMA 28,994,861 symbol-bar/s,1.25e9 步外推 0.7 分钟;8 线程 84,871,264,0.2 分钟 |
| T4.6 任务开销 | 500k 空任务、1 bar、1 线程:3,197,679 bar 步/秒,每任务 0.313 µs |
| T4.7 单次全量 | 500 标的 × 2500 bar、1 线程:SMA 12,732.332 µs/次,98,175,260 symbol-bar/s |
| T4.8 自动线程 | 本机 12 核,auto 解析为 12;auto、1、2 线程的 `sweep_hash` 一致,指标与排序一致 |
| T4.9 内存上限 | `--memory-limit-gb 0.01` 退出码 2,错误含“小于单 worker 预算 64 MiB”,未启动回测 |

## 额外覆盖

- `loads_sweep_grid_and_applies_overrides`:网格加载、标量覆盖、覆盖后 `config_hash` 变化。
- `with_strategy_rejects_invalid_combination`:覆盖后 `fast > slow` 被拒绝。
- `rejects_invalid_sweep_sections`:非法 `top_k`、`oos_split`、未知参数、非标量网格值。
- `top_k_keeps_only_requested_curves`:top-K 只保留请求数量,权益点统计一致。
- `write_outputs_creates_csv_and_summary`:`sweep.csv` 与可解析的 `sweep_summary.json`。
- `parses_peak_rss`:`/proc/self/status` 的 `VmHWM` 解析。
- `bench --symbols`:多标的合成负载、symbol-bar 吞吐、每任务耗时与外推时间。

## 门禁

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps
cargo build --release
bash scripts/check.sh --ci
```

验收结果:

- `cargo test --workspace --all-targets`:92 passed。
- fmt、clippy、doc、release build 均通过。
- `scripts/check.sh --ci` 退出码 0;deny/audit 使用本地 advisory 库离线执行,在线拉取因远程 TLS 失败未改变检查结论。

## 真实冒烟

- 从空 `target/smoke-s4` 开始,使用 `config/03.sweep.toml` 与 `fixtures/sma_6bars.csv`。
- 27 组参数,18 有效,9 组非法跳过,0 条策略告警;目标 `cagr`,top_k=3。
- 配置哈希 `37bbb0c2bfeefb5046f1c5833e7f477749a97efdbfd265180d99f1e42a3541a8`,数据哈希 `a8809713cdf982353411fad97d2ea0d356908da939419b84c4e27147995f6556`。
- `sweep_hash` 为 `3e92bf6083eb48f0f6695f5c93fdb9c576955e7c24bc77f73f1af5a64dc08e1e`,auto、1、2 线程一致。
- top 3 结果均含权益、成交、交易、回撤、月收益与告警;第一名参数 `fast=1, slow=2, weight=0.99`,CAGR 1.025053,Sharpe 7.799573。
- `sweep.csv` 行数包含全部 18 个有效组合;`sweep_summary.json` 可解析,top 3 各 6 个权益点。
- 峰值 RSS 5,054,464 字节;保留权益点 18 个,保留字节估算 936。

## 性能实测

环境:CachyOS,i5-12400F(12 线程),32 GiB 内存,release 构建。

T4.5 缩放:

| 线程 | 空策略 symbol-bar/s | SMA symbol-bar/s | SMA 外推 |
| --- | --- | --- | --- |
| 1 | 21,720,697 | 14,120,632 | 1.5 分钟 |
| 2 | 43,688,063 | 28,994,861 | 0.7 分钟 |
| 4 | 82,575,457 | 54,596,604 | 0.4 分钟 |
| 8 | 133,462,551 | 84,871,264 | 0.2 分钟 |

T4.4 与 T4.6–T4.9 的关键值见用例表。T4.4 的 1000 组扫描耗时 13 ms;T4.6 的 500k 空任务在 1 秒内完成;T4.7 单次全量远低于 2 秒目标。

## 与设计基线的差异

- 首版扫描编排在 `backtest-cli` 的 `sweep` 子命令,不在 `backtest-core` 暴露 `run_sweep()`;原因与回退见 [003-sweep-command.md](../docs/changes/003-sweep-command.md)。
- `sweep_summary.json` 是独立产物;`result.json` 的 `sweep` 字段仍为 `null`,S5 再接入报告契约。
- `sweep.csv` 写全部有效组合;`sweep_summary.json` 只写 top-K 完整曲线。
- `top_k` 元数据记录实际保留数量,即 `min(配置值, 有效组合数)`。
- `oos_split` 在 S4 只校验并记录,样本外切分留给 S5。
- `sweep_hash` 排除线程数、生成时间、耗时、峰值内存与 CPU/内存检测值;`config_hash` 不包含 `[sweep]`。

## S1–S3 回归

- S1 `config/00.smoke.toml`:`144d1118f69392cf1e2369f8b0375037257d5e9eda9bbe59153f64ccf8ee5714`。
- S2 `config/01.risk-smoke.toml`:`8476c2288d3f99a60c9b0ce53f09427e8ac25cb8f3e521194eb15ede8bc4d571`。
- S3 `config/02.portfolio-smoke.toml`:`8796ce7f400ccbd6a30592c2a98bfba0258b53f609e108a07767fc7a17e2a411`。

## 退出条件

- T4.1–T4.9 通过。
- fmt/clippy/test/doc/release/check 通过。
- S4 冒烟通过,auto/1/2 线程哈希一致。
- S1/S2/S3 回归哈希不变。
- 汇报已运行与未运行项。
