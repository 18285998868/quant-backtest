# M8-B 指标单遍扫描验收

日期:2026-11-17

## 目标

验证 metrics-only 热路径的单遍改造:

- `compute_metrics_with_period_sharpe()` 合并权益遍历,去掉收益与回撤
  两个临时向量;
- 指标累加顺序不变,`result_hash`、`sweep_hash` 与历史哈希逐位不变;
- 用新旧实现逐位对比测试锁定等价性;
- W2/W3 端到端耗时相对 M8 二进制下降。

## 范围与兼容

| 项 | 行为 |
| --- | --- |
| 权益扫描 | 一次遍历累计收益和/下行差平方和、最大回撤/回撤平方和、最长回撤段落、权益和与暴露 bar 数 |
| 标准差 | 因公式需要均值,保留第二次权益遍历;不改用平方和公式 |
| 临时分配 | 不再构造收益与回撤向量,单曲线约 20.6 MB 临时内存归零 |
| 完整结果 | 回撤段落明细与月度收益仍单独计算,不在 metrics-only 热路径 |
| 不变项 | 无新增配置/CLI/报告字段;`config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与 schema 1.1/1.2 不变 |
| 排除项 | 曲线降采样(P6)仍未实施,见变更 012 |

## 验收结果

| 用例 | 结果 |
| --- | --- |
| T15.1 单遍指标实现 | 通过:收益、回撤、段落、均价与暴露合并为一次权益遍历 |
| T15.2 逐位等价 | 通过:新旧实现对全部指标 `to_bits()` 相同 |
| T15.3 端到端性能 | 通过:W2 中位耗时 4.38→3.67 s,W3 中位 20.73→15.00 s |
| T15.4 历史哈希 | 通过:`result_hash`/`sweep_hash`/`data_hash` 与 M8 基线逐位一致 |

### T15.1 单遍指标实现

原实现每个组合需要:

- `equity_returns()` 一次权益遍历并分配收益向量;
- `mean()`、`sample_std()` 与下行波动各遍历一次收益;
- `series_drawdowns()` 一次权益遍历并分配回撤向量;
- 最大回撤与 `ulcer` 各遍历一次回撤;
- `all_drawdown_episodes()`、平均权益与暴露比例各遍历一次权益。

新实现第一次遍历权益时同时累计全部标量;第二次遍历只按同一
`previous > 0` 规则重算收益,用于样本标准差。`series_drawdowns()` 已删除。

### T15.2 逐位等价

新增单测 `fused_metrics_match_legacy_bits`:

- 测试模块保留旧实现 `legacy_metrics()` 与 `legacy_series_drawdowns()`;
- 对 `total_return`、`cagr`、`annual_volatility`、`sharpe`、`sortino`、
  `max_drawdown`、`max_drawdown_duration_days`、`calmar`、`ulcer`、
  `trades`、`win_rate`、`profit_factor`、`expectancy`、`total_fees`、
  `turnover`、`cost_drag`、`exposure` 与 `period_sharpe` 逐一比较;
- 浮点字段用 `to_bits()` 比较,区分 `-0.0` 与 `0.0`;
- 覆盖空曲线、单点、零权益、平台期、含恢复与未恢复回撤段落、
  5000 点曲线,以及含未平仓交易的输入。

### T15.3 端到端性能

基线为 M8 二进制(`/data/quant-backtest/target/release/quant-backtest`),
W2 交替跑三轮、W3 交替跑五轮,计时用 `getrusage` 包装脚本:

| 场景 | 组合数 | top_k | 线程 | M8 耗时 | M8-B 耗时 | M8 峰值 | M8-B 峰值 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W2 | 100 | 1 | 8 | 4.32–4.54 s | 3.51–3.81 s | 708–733 MB | 649–665 MB |
| W3 | 1000 | 3 | 12 | 19.8–21.2 s | 14.3–17.6 s | 1252–1339 MB | 1303–1309 MB |

按中位数计:

- W2 4.38 s → 3.67 s(约 16%),峰值 729 MB → 661 MB(约 9%);
- W3 20.73 s → 15.00 s(约 28%),峰值 1312 MB → 1307 MB(持平)。

### T15.4 历史哈希

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
4 标的样本仍为 589 笔交易、2184 个权益点。

## 测试与门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace --locked
cargo build --release --locked
bash scripts/check.sh --ci
```

- 测试:185 个通过,0 失败;比 M8 的 184 个增加 1 个。
- fmt、clippy、release 构建通过,无新增告警。

## 干净克隆

```bash
git clone /data/qb-m8b /data/qb-m8b-clean
cd /data/qb-m8b-clean
cargo test --workspace --locked
cargo build --release --locked
R=/data/m8bc-out
target/release/quant-backtest run --config config/00.smoke.toml --threads 1 --out "$R/00"
target/release/quant-backtest sweep --config fixtures/sweep.toml --out "$R/sweep"
target/release/quant-backtest walkforward --config fixtures/walkforward.toml --out "$R/wf"
target/release/quant-backtest run --config /tmp/qb-m8/bench/perf_run_1m.toml --out "$R/1m"
target/release/quant-backtest run --config /tmp/m8c_multi.toml --out "$R/multi"
```

- 提交 `2b8f032` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked`(185 个)与
  `cargo build --release --locked`。
- 冒烟 `result_hash = efbb00b9...`;`fixtures/sweep.toml` 的
  `sweep_hash = 41d11ac9...`;`fixtures/walkforward.toml` 的
  `result_hash = 4f0e7bd6...`。
- BTCUSDT spot 1m 大样本 `result_hash = f7026895...`、
  `data_hash = 36ec7667...`;4 标的 UM 1h 样本
  `result_hash = 7f823905...`、`data_hash = 376a13c5...`,
  589 笔交易、2184 个权益点,与工作副本一致。
