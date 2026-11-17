# M8 扫描指标与装载性能验收

日期:2026-11-17

## 目标

验证网格扫描与多标的装载的性能改造:

- `sweep_hash` 流式计算,不再构建 summary `serde_json::Value` 树;
- 网格与 OOS 使用 metrics-only 回放,不保留每个组合的完整结果;
- 归档按标的并行装载,`finalize()` 不再按字符串重新分桶;
- 快速 `monthly_returns()` 与流式 `sweep_summary.json` 写入;
- `data_hash`、`result_hash`、`sweep_hash`、报告 schema 与历史哈希不变。

## 范围与兼容

| 项 | 行为 |
| --- | --- |
| summary 哈希 | 按旧 `Value` 字典序逐字段写入 SHA-256,字节等价由单测兜底 |
| 网格与 OOS | 只保留 metrics、逐期 Sharpe、告警;top-K 重跑完整结果 |
| 整段与分块 | 共用 `compute_metrics_with_period_sharpe()`,逐期 Sharpe 由同一收益序列导出 |
| 装载 | 单标的行为不变;多标的按标的 `par_iter()`,错误按标的顺序取第一个 |
| 分桶 | `RawBar` 不再携带 `symbol`,下标与 `LoadOptions::instruments` 对齐 |
| 月度收益 | 单遍沿月边界推进,结果与旧 `BTreeMap` 实现逐字段一致 |
| 不变项 | 无新增配置/CLI/报告字段;`config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与 schema 1.1/1.2 不变 |
| 排除项 | 曲线降采样会改变 `sweep_hash`,留待 schema 升级 |

## 验收结果

| 用例 | 结果 |
| --- | --- |
| T14.1 流式 summary 哈希 | 通过:与旧 `serde_json::Value` 实现逐字节相同;W1–W4 `sweep_hash` 与 M7 一致 |
| T14.2 metrics-only 回放 | 通过:整段/分块指标、逐期 Sharpe、告警与完整结果完全一致 |
| T14.3 快速月度收益 | 通过:与旧 `BTreeMap` 实现逐字段一致 |
| T14.4 并行装载与分桶 | 通过:CSV/归档单测全绿,多标的错误顺序与串行路径一致 |
| T14.5 端到端性能 | 通过:W1–W4 耗时降 27%–42%,峰值 RSS 降 56%–73% |
| T14.6 历史哈希 | 通过:S1/S2/S3/1h/1w、walk-forward、sweep、1m 与 4 标的 1h 哈希全部不变 |

### T14.1 流式 summary 哈希

新增单测 `streaming_summary_hash_matches_legacy_bytes`:

- 对同一 `SweepSummary` 分别调用新的 `canonical_hash()` 与旧
  `serde_json::json!` + `to_vec` 实现;
- `oos` 与 `dsr` 同时存在、以及两者都为 `None` 两种情况都逐字节相同;
- 覆盖 `SweepCounts`、`SweepTopResult`、`OosReport` 与
  `DeflatedSharpe` 的规范字段顺序。

W1–W4 的 `sweep_hash` 与 M7 基线完全一致,说明改造没有改变任何
哈希输入。

### T14.2 metrics-only 回放

新增 `BacktestStats` 与:

- `compute_metrics_with_period_sharpe()`:复用 `compute_metrics` 的收益
  序列,同时返回未年化逐期 Sharpe;
- `run_backtest_stats()`:整段 metrics-only;
- `run_backtest_stream_stats()`:分块 metrics-only,与
  `run_backtest_stream()` 共用 `next_stream_window()` 与
  `StreamMode`。

单测 `stats_mode_matches_full_result` 用同一策略脚本对比完整结果:
`metrics` 逐字段相同,`period_sharpe` 与
`period_sharpe(equity_returns(...))` 相同,告警列表相同。

`run_grid()` 与 OOS 抽样改为调用 `run_single_stats()`;top-K 选择后仍然
用 `run_selected()` 生成完整曲线,报告内容不变。

### T14.3 快速月度收益

单测 `fast_monthly_returns_match_legacy` 对代表性权益序列同时运行新实现
与旧 `BTreeMap<i32, f64>` 实现,逐月 `year/month/return_pct` 完全一致。
新实现只保留一个当前月累计值,不再为每个权益点计算年月键。

### T14.4 并行装载与分桶

- `RawBar` 去掉 `symbol: String`,`RawBars` 直接按标的下标分桶;
- `finalize()` 就地对每个桶校验重复/乱序、过滤 `[start, end)` 并构造
  `BarSeries`,不再经历 `BTreeMap<String, Vec<RawBar>>`;
- Binance 与 Dukascopy 的按标的装载改为 `par_iter()`,并行结果先按
  标的顺序展开成 `Vec<Result<_>>`,再取第一个错误;
- CSV 扫描仍保留全局符号分组检查,`symbol` 借用记录,不再逐行克隆。

`backtest-data` 的 28 个单测全部通过,覆盖缺失标的、空区间、重复/乱序、
未知符号、时间线不一致与资金费校验。

### T14.5 端到端性能

基线为 M7 二进制,每场景交替跑两轮,计时用 `getrusage` 包装脚本:

| 场景 | 标的 | 组合数 | top_k | 线程 | M7 耗时 | M8 耗时 | M7 峰值 | M8 峰值 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| W1 | 1 | 10 | 2 | 1 | 6.09–6.11 s | 3.82–3.83 s | 2848–2853 MB | 965–986 MB |
| W2 | 1 | 100 | 1 | 8 | 5.98–6.08 s | 4.24 s | 2695–2703 MB | 707–737 MB |
| W3 | 1 | 1000 | 3 | 12 | 28.7–29.4 s | 20.9–21.4 s | 4249–4265 MB | 1279–1310 MB |
| W4 | 10 | 10 | 2 | 8 | 17.2–18.2 s | 10.25–10.41 s | 6015–6051 MB | 2591–2700 MB |

产物哈希:

| 场景 | `sweep_hash` |
| --- | --- |
| W1 | `1bf1e2166d945e77749532ee271b7d2cef628cd9709ba7235b452b9a6e709837` |
| W2 | `32f6ee94f67938c0f7cd702ae4ca82728e5f932ffd763099b54756679bfe98f2` |
| W3 | `e19d0b7343dc7826c9c3b61415fa79383f61c2dbfca50141a2948b805205b9ed` |
| W4 | `4a3f718e0a77679558c9814776faa3c52ade4ad6bb707ff027f79f657e28ef4a` |

W1 的峰值仍约 0.96 GB,主要来自 top-K 完整权益曲线与其结果哈希;
W4 的 10 标的装载由串行改为并行,端到端减少约 7–8 s。

### T14.6 历史哈希

| 样本 | `result_hash` | `data_hash` |
| --- | --- | --- |
| `config/00.smoke.toml`,1 线程 | `efbb00b9f70690e5238630c65dea13922621fff36059ec26367fee66e067b977` | — |
| `config/01.risk-smoke.toml` | `cd330ad9d98802e9d3492a61da4d24fcf5b17f1e51cba8b718d8a72e7a25979e` | — |
| `config/02.portfolio-smoke.toml` | `d84b79855eae8f066b0ec6024673f8df4cffed2b7029ff9c119c84bb3ba8b1be` | — |
| `config/04.intraday.toml` | `7c680a05ae883f02dd8b8a4428a13afc570c86a4d7f16db2badf5a8210b30f86` | — |
| `config/05.weekly.toml` | `7c4c0da1569dea4018d393ef115f5765493e9c9b367148b0c6d2b5efcde9002f` | — |
| `fixtures/walkforward.toml` | `4f0e7bd642c68cf434f25b2d567f1aeeba3f72909a031b87cd96c19d8e9f05c5` | — |
| BTCUSDT spot 1m 2023-05..2026-10 | `f702689588d2a9102ccb713d645af1a9c91da9d03c3d15507330daf5788135a4` | `36ec7667768a435cfbbca1d1ef8c1bbe48a109b3b980d6927a84fe6edcefd72a` |
| BTC/ETH/BNB/SOL UM 1h 2024-01..2024-03 | `7f8239054af618a5d8c764d994bad64f1c42517193ff2a44463ffd1ba6e0e102` | `376a13c56bf6eb7d5c297d25fd9ef208ab0e7f7a1caadc1d21881ddb15d6cabb` |

`fixtures/sweep.toml` 的 `sweep_hash` 为
`41d11ac9f979517f45bd137b80ca41ffd16cf50b02081c92387c6b171e61bfab`,
与 M7 一致。4 标的样本保持 589 笔交易、2184 个权益点。

## 测试与门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace --locked
cargo build --release --locked
bash scripts/check.sh --ci
```

- 测试:184 个通过,0 失败;比 M7 的 181 个增加 3 个。
- fmt、clippy、release 构建通过,无新增告警。
- 原型环境变量 `QB_TIME`、`QB_SKIP_CURVES`、`QB_SKIP_SUMMARY_HASH`
  未进入提交代码。

## 干净克隆

```bash
git clone /data/qb-m8 /data/qb-m8-clean
cd /data/qb-m8-clean
cargo test --workspace --locked
cargo build --release --locked
target/release/quant-backtest run --config config/00.smoke.toml --threads 1 --out /tmp/m8c-00
target/release/quant-backtest sweep --config fixtures/sweep.toml --out /tmp/m8c-sweep
target/release/quant-backtest walkforward --config fixtures/walkforward.toml --out /tmp/m8c-wf
target/release/quant-backtest run --config /tmp/qb-m8/bench/perf_run_1m.toml --out /tmp/m8c-1m
```

- 提交 `d89b111` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(184 个)与
  `cargo build --release --locked`。
- 冒烟 `result_hash = efbb00b9...`;`fixtures/sweep.toml` 的
  `sweep_hash = 41d11ac9...`;`fixtures/walkforward.toml` 的
  `result_hash = 4f0e7bd6...`。
- BTCUSDT spot 1m 大样本 `result_hash = f7026895...`、
  `data_hash = 36ec7667...`;4 标的 UM 1h 样本
  `result_hash = 7f823905...`、`data_hash = 376a13c5...`。
- 克隆运行的周期、交易数与权益点数与工作副本一致。
