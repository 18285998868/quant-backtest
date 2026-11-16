# 011 归档按月窗口规划

## 背景

M6 把报告阶段的峰值降下来后,1843200 bar 的 Binance spot BTCUSDT 1m
样本仍受数据规划阶段主导:整段 `run` 峰值约 412 MB,分块约 413 MB,
而最终时间线只占约 14.7 MB。

根因是 `plan_archive()` 对每个标的调用 `load_universe_reporting()`,
一次装载完整区间:

- `data-reader` 返回整段 `Table`,含全部 kline 列;
- 适配层再构造整段 `RawBar`;
- `finalize()` 再构造整段 `BarSeries` 与时间线。

三份数据在规划期间共存,峰值与总 bar 数成正比,而不是与单窗口大小
成正比。M6 变更文档已把这一项列为剩余限制。

## 决策

1. 仅在归档入口启用月窗口:
   - `start` 与 `end` 都有界;
   - `start < end`;
   - `timeframe.duration_ms() < 1 day`。
2. `plan_archive()` 分派到两条路径:
   - `plan_archive_windows()`:按 UTC 月逐窗读取,只保留单标的单月
     OHLCV,直接喂给页哈希,不构造整段 `RawBar` 或 `Universe`;
   - `plan_archive_full()`:保留 M6 的整段行为,用于无界区间、日线及以上
     周期、窗口数 `<= 1` 与参考标的无数据时的回退。
3. 月界归属:
   - 窗口区间为左开右闭,上一窗包含 `ts == month_start` 的 bar;
   - 下一窗从 `month_start + timeframe` 开始请求归档,精确过滤从
     `month_start + 1` 起,避免重复扫描上一月文件;
   - 窗口请求范围与精确过滤范围分开记录,归档日期窗口仍按日粒度向外
     放宽一个周期。
4. 资金费:
   - 一次性装载完整请求区间,窗口内不再重复读取;
   - 规划完成后按最终时间线过滤 `(首根开盘, 末根收盘]`,与
     `finalize()` 的过滤条件逐字对齐;
   - 资金费仍参与 v2 `data_hash` 的 `funding/v2` 段。
5. 校验:
   - 每个标的内部逐窗检查重复与乱序;
   - 参考标的(0 号)建立时间线,其余标的逐根对齐;
   - 非参考标的在区间内完全没有 bar 时返回 `MissingSymbolData`,
     与整段路径一致。
6. 诊断:
   - 新增 `BinanceBarStats`、`BinanceFundingStats`、`DukascopyBarStats`
     与 `BarStats`,跨月累计文件数、跳过行、缺口与亚秒修正;
   - 每个标的仍只打印一行 `数据源:`,多个月窗口合并统计;
   - 资金费仍打印总行数、文件数与区间内结算数,顺序与 M6 一致。
7. 不变项:
   - 不修改 `data-reader`;
   - 不新增配置、CLI 参数或报告字段;
   - `config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与报告
     schema 1.2 不变;
   - CSV 规划仍走原有 `plan_csv()`。

## 替代方案

- 给 `data-reader` 增加流式迭代或列投影:最彻底,但需要改上游接口,
  超出本阶段范围;月窗口只依赖现有 `date_range`。
- 按日分窗:峰值更低,但归档文件打开次数增加约 30 倍,耗时不可接受。
- 保留整段装载,只降低并行度:不解决峰值随区间线性增长的问题。
- 先落盘中间 CSV 再按窗口读:多一次磁盘往返,且改变诊断与失败语义。

## 影响文件

- `crates/backtest-data/src/lib.rs`:`plan_archive()` 分派、
  `plan_archive_windows()`、`plan_archive_full()`、`plan_windows()`、
  `month_boundaries()`、`next_month_start()`、`PlanWindow` 与单元测试。
- `crates/backtest-data/src/archive.rs`:`load_bars_window()`、
  `load_funding()`、结构化统计与装载函数重构。
- `CHANGELOG.md`、`docs/01-需求规格.md`、`docs/03-流程设计.md`、
  `STEP.md`、`STEP.d/17-monthly-archive-planning.md`。

## 验证

大样本:Binance spot BTCUSDT 1m,2023-05-01 至 2026-10-31,1843200 bar。

| 场景 | M6 峰值 / 耗时 | M7 峰值 / 耗时 | 哈希 |
| --- | --- | --- | --- |
| `validate` | 413.1 MB / 0.825 s | 38.1 MB / 1.957 s | `data_hash = 36ec7667...` |
| `run` 分块 | 413.7 MB / 2.486 s | 209.8 MB / 3.885 s | `result_hash = f7026895...` |
| `run` 整段 | 412.1 MB / 1.425 s | 412.1 MB / 1.425 s | `result_hash = f7026895...` |

`validate` 的峰值下降约 11 倍;分块 `run` 的剩余峰值来自 184 万权益点、
22097 笔交易与 44194 笔成交,不再由归档规划主导。整段 `run` 不经过
窗口规划,峰值保持 M6 水平。

月窗口正确性用 M6 二进制做 A/B:

| 样本 | M6 `result_hash` / `data_hash` | M7 `result_hash` / `data_hash` |
| --- | --- | --- |
| BTC/ETH/BNB/SOL UM 1h 2024-01..2024-03,`chunk_bars=100` | `7f823905...` / `376a13c5...` | `7f823905...` / `376a13c5...` |
| Dukascopy EURUSD 1h 2023-05..2023-08,仅 7 月有文件 | `ff2ceb1f...` / `7958c2a5...` | `ff2ceb1f...` / `7958c2a5...` |

多标的样本含 538 次资金费结算与 67 条告警,589 笔交易;
Dukascopy 样本的 3 个缺失月只进入诊断。两者整段/分块与 M6/M7 均逐项一致。

历史哈希保持不变:

- `config/00.smoke.toml`: `efbb00b9...`
- `config/01.risk-smoke.toml`: `cd330ad9...`
- `config/02.portfolio-smoke.toml`: `d84b7985...`
- `config/04.intraday.toml`: `7c680a05...`
- `config/05.weekly.toml`: `7c4c0da1...`
- BTCUSDT UM 1m 2023-06: `result_hash = f648cd3e...`,`data_hash = d9dc81d8...`
- BTC/ETH/BNB/SOL UM 1h 2024-01: `result_hash = 54259a53...`,
  `data_hash = da7521b2...`
- `fixtures/walkforward.toml`: `4f0e7bd6...`
- `fixtures/sweep.toml`: `sweep_hash = 41d11ac9...`

完整门禁、干净克隆与验收细节见
[M7 验收](../../STEP.d/17-monthly-archive-planning.md)。

## 限制与回退

- 月窗口会为每个月重新打开归档文件,`validate` 与分块 `run` 的规划
  阶段耗时约增加 1 到 1.4 秒;这是用时间换内存,不改变结果。
- 日线及以上周期仍走整段路径;分钟与小时归档的区间越长,收益越明显。
- `load_window()` 仍按分块请求重新装载归档,剩余峰值由结果对象与
  分块策略决定,窗口规划不改变这部分。
- 窗口数等于月数,极长区间会产生数百次文件打开;当前样本最多 42 个
  月窗口,未观察到文件句柄或耗时异常。
- 回退:`plan_archive()` 直接调用 `plan_archive_full()` 即可恢复 M6
  行为,哈希与报告不变,峰值回到约 413 MB。
