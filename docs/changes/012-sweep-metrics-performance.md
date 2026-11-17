# 012 扫描指标与装载性能

## 背景

M7 完成后,用四组 Binance spot 1m 样本做性能剖析:

| 场景 | 标的 | 组合数 | top_k | 线程 | 产物 JSON |
| --- | --- | --- | --- | --- | --- |
| W1 | 1 | 10 | 2 | 1 | 596 MB |
| W2 | 1 | 100 | 1 | 8 | 274 MB |
| W3 | 1 | 1000 | 3 | 12 | 823 MB |
| W4 | 10 | 10 | 2 | 8 | 989 MB |

实测量化出三个瓶颈:

- `sweep_hash` 先把整个 `sweep_summary` 转成 `serde_json::Value` 树再
  序列化,596 MB 的产物在 W1 里比整个网格计算还贵;
- 网格与样本外评估调用完整 `run_backtest`,为每个组合保留 equity、
  fills、trades 与报告元数据,这些数据随后被丢弃;
- 归档装载按标的串行,`finalize()` 用 `BTreeMap<String, Vec<RawBar>>`
  重新分桶,每根 bar 还要克隆一次 `symbol` 字符串。

## 决策

1. 流式 summary 哈希:
   - `backtest-report` 公开 `CanonicalItem`、`CanonicalRef`、
     `CanonicalSlice` 与 `canonical_hash()`;
   - `SweepSummary`、`SweepCounts`、`SweepTopResult`、`OosReport` 与
     `DeflatedSharpe` 按旧 `serde_json::Value` 的字典序逐字段写入
     SHA-256,不再构建 `Value` 树;
   - `sweep_hash`、报告 schema 1.1/1.2 与结果字段完全不变。
2. 原生 metrics-only 回放:
   - 新增 `BacktestStats`、`compute_metrics_with_period_sharpe()`、
     `run_backtest_stats()` 与 `run_backtest_stream_stats()`;
   - `run_grid` 与 OOS 只保留指标、逐期 Sharpe 与告警,top-K 仍然重跑
     完整结果;
   - 整段与分块共用同一套 `compute_metrics` 收益序列,逐期 Sharpe 不再
     额外遍历一次权益曲线。
3. 装载并行与去字符串分桶:
   - `RawBar` 去掉 `symbol: String`,改为按标的下标的
     `RawBars = Vec<Vec<RawBar>>`;
   - Binance 与 Dukascopy 按标的 `par_iter()` 装载,错误仍按标的顺序
     取第一个,保证多标的失败时与串行路径报错一致;
   - `finalize()` 就地校验重复/乱序、过滤 `[start, end)` 并直接构造
     `BarSeries`,不再重新分桶。
4. 快速月度收益:
   - `monthly_returns()` 沿月边界推进,去掉每点一次
     `BTreeMap` 查找与插入;与旧实现逐字段一致。
5. 流式 summary 写入:
   - `sweep_summary.json` 改为 `File` + `BufWriter` +
     `to_writer_pretty`,不再先构造完整 pretty 字符串。
6. 不变项:
   - 不新增配置、CLI 参数或报告字段;
   - `config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与报告
     schema 不变;
   - 曲线降采样(P6)不在本阶段范围,因为会改变 `sweep_hash`。

## 影响文件

- `crates/backtest-report/src/lib.rs`:`canonical_hash()` 与公开规范
  序列化接口。
- `crates/backtest-cli/src/sweep.rs`:规范 summary 哈希、metrics-only
  网格与 OOS、流式 summary 写入。
- `crates/backtest-cli/src/stream.rs`:分块 metrics-only 回放。
- `crates/backtest-core/src/engine/mod.rs`:`BacktestStats`、
  `run_backtest_stats()`、`run_backtest_stream_stats()`。
- `crates/backtest-core/src/metrics/mod.rs`:
  `compute_metrics_with_period_sharpe()` 与快速 `monthly_returns()`。
- `crates/backtest-data/src/lib.rs`:去字符串 `RawBars` 与 `finalize()`
  就地构造。
- `crates/backtest-data/src/archive.rs`:按标的并行装载。
- `CHANGELOG.md`、`STEP.md`、`STEP.d/18-sweep-metrics-performance.md`。

## 验证

基线为 M7 二进制(`/data/qb-m7-clean`),每场景交替跑两轮:

| 场景 | M7 耗时 / 峰值 | M8 耗时 / 峰值 | `sweep_hash` |
| --- | --- | --- | --- |
| W1 | 6.09–6.11 s / 2848–2853 MB | 3.82–3.83 s / 965–986 MB | `1bf1e216...` |
| W2 | 5.98–6.08 s / 2695–2703 MB | 4.24 s / 707–737 MB | `32f6ee94...` |
| W3 | 28.7–29.4 s / 4249–4265 MB | 20.9–21.4 s / 1279–1310 MB | `e19d0b73...` |
| W4 | 17.2–18.2 s / 6015–6051 MB | 10.25–10.41 s / 2591–2700 MB | `4a3f718e...` |

W4 的 10 标的装载从约 6.6 s 的串行累加降为并行读取;W1/W2/W3 的峰值
下降主要来自不再保留每个组合的完整结果与 summary `Value` 树。

历史哈希全部保持不变:

- `config/00.smoke.toml`: `efbb00b9...`
- `config/01.risk-smoke.toml`: `cd330ad9...`
- `config/02.portfolio-smoke.toml`: `d84b7985...`
- `config/04.intraday.toml`: `7c680a05...`
- `config/05.weekly.toml`: `7c4c0da1...`
- `fixtures/sweep.toml`: `sweep_hash = 41d11ac9...`
- `fixtures/walkforward.toml`: `result_hash = 4f0e7bd6...`
- BTCUSDT spot 1m 2023-05..2026-10:`result_hash = f7026895...`,
  `data_hash = 36ec7667...`
- BTC/ETH/BNB/SOL UM 1h 2024-01..2024-03:`result_hash = 7f823905...`,
  `data_hash = 376a13c5...`

完整门禁、干净克隆与验收细节见
[M8 验收](../../STEP.d/18-sweep-metrics-performance.md)。

## 限制与回退

- `sweep_summary.json` 仍包含 top-K 的完整权益曲线,产物大小由 top_k
  与采样点数决定;把曲线降到 `run` 的 2 万点会改变 `sweep_hash`,
  需要 schema 升级与显式接受。
- 并行装载的峰值取决于标的数与归档解码缓冲;单标的路径不受影响。
- metrics-only 回放不计算回撤段、月度收益与资金费汇总,因此只可用于
  网格与 OOS 的指标排序;任何写入报告的路径仍然走完整结果。
