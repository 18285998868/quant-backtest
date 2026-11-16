# M7 归档按月窗口规划验收

日期:2026-11-16

## 目标

验证归档规划阶段不再整段驻留全部 OHLCV:

- Binance/Dukascopy 归档在起止有界且周期短于一天时按 UTC 月分窗;
- 每窗只保留单标的单月 `RawBar`,直接喂给 v2 页哈希;
- 资金费一次性装载后按最终时间线过滤;
- 1843200 bar 样本的 `validate` 峰值与单窗口同阶;
- `data_hash`、`result_hash`、历史冒烟哈希与报告不变。

## 范围与兼容

| 项 | 行为 |
| --- | --- |
| 启用条件 | 归档入口、`start`/`end` 有界、`start < end`、周期短于一天 |
| 回退条件 | 无界区间、日线及以上周期、月窗口数 `<= 1`、参考标的无数据 |
| 月界归属 | 左开右闭;上一窗保留 `ts == month_start` 的 bar,下一窗从 `month_start + timeframe` 请求 |
| 窗口过滤 | 请求范围按日粒度放宽;精确过滤为 `[min_ts, max_ts]` |
| 资金费 | 完整区间一次装载,按 `(首根开盘, 末根收盘]` 过滤,与 `finalize()` 一致 |
| 诊断 | 按标的累计月窗口统计,仍为单行 `数据源:` |
| 错误 | 单标的逐窗重复/乱序;非参考标的无数据返回 `MissingSymbolData` |
| 不变项 | 不修改 `data-reader`;无新增配置/CLI/报告字段;`config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与 schema 1.2 不变 |

## 验收结果

| 用例 | 结果 |
| --- | --- |
| T13.1 月窗口切分 | 通过:窗口覆盖 `[start, end)`、无重叠无空洞;月初 bar 归属上一窗;部分首尾月、整月结束、跨年与闰年单测通过 |
| T13.2 大样本峰值 | 通过:1843200 bar `validate` 峰值 413.1 → 38.1 MB;分块 `run` 413.7 → 209.8 MB;哈希不变 |
| T13.3 多标的与资金费 | 通过:4 标的 1h 三个月样本 M6/M7、整段/分块 `result_hash` 与 `data_hash` 相同,589 笔交易、538 次结算、67 条告警一致 |
| T13.4 Dukascopy 跨月 | 通过:2023-05..2023-08 仅 7 月有文件,3 个缺失月进入诊断;743 根 bar 与 M6 及 M2 七月记录一致 |
| T13.5 历史哈希 | 通过:S1/S2/S3/1h/1w、walk-forward、sweep、1m 与 4 标的 1h 哈希全部不变 |

### T13.1 月窗口切分

单测覆盖:

- `plan_windows_partition_months_without_gaps`:三个整月窗口连续,
  上一窗 `max_ts` 与下一窗 `min_ts` 相差 1;
- `plan_windows_keeps_single_month_in_one_window`:单月区间只生成一个窗口,
  触发整段回退;
- `plan_windows_handles_partial_first_and_last_months`:首尾非整月时
  窗口边界与时间线一致;
- `plan_windows_excludes_month_boundary_equal_to_end`:`end` 恰好是月初时
  不生成空窗口;
- `next_month_start_rolls_over_year`:12 月跨年与闰年 2 月正确;
- `accumulates_binance_stats`:月窗口统计逐字段累加。

### T13.2 大样本

数据:Binance spot `BTCUSDT` 1m,2023-05-01 至 2026-10-31,1843200 bar,
`config_hash = fe6d6fcb...`、`data_hash = 36ec7667...`。

| 场景 | M6 峰值 / 耗时 | M7 峰值 / 耗时 | `result_hash` |
| --- | --- | --- | --- |
| `validate` | 413.1 MB / 0.825 s | 38.1 MB / 1.957 s | 不适用 |
| `run` 分块,`chunk_bars=100000` | 413.7 MB / 2.486 s | 209.8 MB / 3.885 s | `f7026895...` |
| `run` 整段,`chunk_bars=0` | 412.1 MB / 1.425 s | 412.1 MB / 1.425 s | `f7026895...` |

完整值:

- `data_hash = 36ec7667768a435cfbbca1d1ef8c1bbe48a109b3b980d6927a84fe6edcefd72a`
- `result_hash = f702689588d2a9102ccb713d645af1a9c91da9d03c3d15507330daf5788135a4`
- 22097 笔交易、44194 笔成交不变。

规划阶段的峰值从约 413 MB 降到 38.1 MB;分块运行剩余约 210 MB 由
184 万权益点与交易结果对象主导。整段运行不启用窗口规划,峰值保持 M6
水平。诊断仍为
`1844640 根 bar(daily=6, monthly=43, 跳过=0, 缺口=0)`,
说明 42 个月窗口没有重复读取月文件。

月窗口重新打开归档带来固定开销:`validate` 从 0.825 s 增至 1.957 s,
分块 `run` 从 2.486 s 增至 3.885 s;这是用时间换内存,结果不变。

### T13.3 多标的与资金费

样本:BTC/ETH/BNB/SOL UM 1h,2024-01-01 至 2024-04-01,2184 bar,
`config_hash = 758631f1...`。

| 二进制 / 装载 | `result_hash` | `data_hash` | 交易 | 权益点 | 结算 |
| --- | --- | --- | --- | --- | --- |
| M6 整段 | `7f823905...` | `376a13c5...` | 589 | 2184 | 538 |
| M6 分块 | `7f823905...` | `376a13c5...` | 589 | 2184 | 538 |
| M7 整段 | `7f823905...` | `376a13c5...` | 589 | 2184 | 538 |
| M7 分块 | `7f823905...` | `376a13c5...` | 589 | 2184 | 538 |

资金费 `total_cost = 30456.909149332725`,`paid = 49381.43563885609`,
`received = 18924.526489523367`,按标的 152/130/131/125 次;
67 条保证金裁剪告警在四种组合中一致。

诊断按标的合并三个月窗口:
`2208 根 bar(daily=0, monthly=4, 跳过=0, 缺口=0)`,
资金费 `276 次结算(monthly=4, 跳过=0, 缺口=0, 亚秒修正=38)`,
区间内 273 次,与 M6 输出相同。

### T13.4 Dukascopy 跨月

样本:EURUSD bid 1h,2023-05-01 至 2023-08-01;归档只有 2023-06,
其余 3 个月缺失。月窗口路径输出:

- `744 根 bar(文件 1/4, 缺失=3, 跳过=0, 缺口=0 段/0 根)`;
- 精确过滤后 743 根,`2023-07-01T01:00:00Z` 至 `2023-07-31T23:00:00Z`;
- `data_hash = 7958c2a5608663481689e4368abaaccd899a0d7d2bb62016a9f40eb5c42f2b71`,
  与 M2 的 2023-07 单月记录一致;
- M6 与 M7 的 `result_hash` 同为 `ff2ceb1f...`,19 笔交易、743 个权益点。

缺失月只进入诊断,不补造 bar;月初 bar 转换后的收盘时刻被 `end`
正确排除。

### T13.5 历史哈希

| 用例 | 结果 |
| --- | --- |
| `config/00.smoke.toml` | `efbb00b9...` |
| `config/01.risk-smoke.toml` | `cd330ad9...` |
| `config/02.portfolio-smoke.toml` | `d84b7985...` |
| `config/04.intraday.toml` | `7c680a05...` |
| `config/05.weekly.toml` | `7c4c0da1...` |
| BTCUSDT UM 1m 2023-06,43199 bar | `result_hash = f648cd3e...`,`data_hash = d9dc81d8...`,2338 笔交易、89 次结算 |
| BTC/ETH/BNB/SOL UM 1h 2024-01,744 bar | `result_hash = 54259a53...`,`data_hash = da7521b2...`,198 笔交易、179 次结算、24 条告警 |
| `fixtures/walkforward.toml` | `result_hash = 4f0e7bd6...` |
| `fixtures/sweep.toml` | `sweep_hash = 41d11ac9...` |

## 门禁

| 项 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo nextest run --workspace --locked` | 181 个通过 |
| `cargo test --workspace --doc --locked` | 通过(0 个 doctest) |
| `RUSTDOCFLAGS=-D warnings cargo doc` | 通过 |
| `cargo deny check` | 通过 |
| `cargo audit` | 无漏洞报告 |
| `cargo build --release --locked` | 通过 |
| `scripts/check.sh --ci` | 通过 |

## 干净克隆

提交后克隆到 `data-reader` 同级的 `qb-m7-clean`:

- `cargo test --workspace --locked`:181 个通过;
- `cargo build --release --locked`:成功;
- 1843200 bar `validate`:`data_hash = 36ec7667...`,诊断与工作副本一致;
- 4 标的 1h 三个月样本:`result_hash = 7f823905...`,
  `data_hash = 376a13c5...`;
- `fixtures/walkforward.toml` 与 `fixtures/sweep.toml` 分别为
  `4f0e7bd6...`、`41d11ac9...`。

## 限制

- 月窗口按需重新打开归档,规划耗时随月数增加;极长区间需关注文件打开
  次数与总耗时。
- 日线及以上周期仍整段装载;`load_window()` 仍重新读取每个分块,
  剩余峰值由结果对象主导。
- 月窗口只在归档路径启用,CSV 规划保持原实现。
- 回退:`plan_archive()` 直接调用 `plan_archive_full()` 即可恢复 M6
  行为,哈希与报告不变,峰值回到约 413 MB。
