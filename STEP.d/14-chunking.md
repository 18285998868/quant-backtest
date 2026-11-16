# M4 分块流式加载验收

日期:2026-11-16

## 目标

验证分钟级数据按窗口装载的完整链路:

- `run.chunk_bars` 显式分块,或 `memory_limit_gb` 超限时自动分块;
- 窗口按绝对下标连续推进,策略历史只读、不重复喂入;
- 引擎状态跨窗口延续,资金费不重复结算;
- 同一数据整段与分块的 `data_hash`、`result_hash`、`sweep_hash`、
  指标、成交、交易、资金费与告警一致;
- v2 固定页哈希在 CSV/归档、不同窗口与资金费开关下可复现;
- 历史哈希按 v2 重录后 `config_hash` 不变,旧验收配置得到新值。

## 开关与窗口

| 项 | 行为 |
| --- | --- |
| `run.chunk_bars > 0` | 强制分块,窗口上限为该值;不小于数据长度时回退整段 |
| `run.chunk_bars = 0` 且 `memory_limit_gb > 0` | 规划后估算全量内存,超限才分块 |
| 两者都为 0 | 整段装载,不额外扫描 |
| 窗口边界 | `[start, end)` 绝对下标,连续、不重叠、不留空洞 |
| 策略历史 | 块首可带 `strategy.history_bars()` 根,只读、不重放 |
| 状态延续 | `EngineState` 携带现金、持仓、挂单、目标权重、权益、成交、交易、告警、资金费光标与下一根下标 |
| 支持命令 | `run`、`validate`、`sweep`;`walkforward` 首版整段装载 |
| `config_hash` | 不含 `chunk_bars`;改窗口大小不改变配置哈希 |

`chunk_bars` 与内存估算的 CLI 输出示例:

```text
数据内存:全量约 2.0 MiB,分块每窗口 5000 根/约 195.3 KiB,另加策略历史
```

## 验收结果

| 用例 | 结果 |
| --- | --- |
| T10.1 单标的 1m | 通过:整段与分块 `data_hash`、`result_hash`、指标、成交、交易、资金费与告警逐项一致 |
| T10.2 多标的 1h | 通过:整段与分块哈希、资金费 `by_symbol`、交易与 24 条告警一致 |
| T10.3 sweep | 通过:16 组网格整段与分块 `sweep_hash`、top 结果 `result_hash`、`data_hash` 一致 |
| T10.4 自动与回退 | 通过:显式值优先;`chunk_bars >= 数据长度` 回退整段;自动模式只在估算超限时启用 |
| T10.5 窗口边界 | 通过:绝对下标、历史只读、首个窗口未覆盖起点与窗口间空洞返回 Config 错误 |
| T10.6 v2 哈希 | 通过:CSV/归档同内容同哈希;跨页摘要覆盖所有页;窗口大小不改变 `data_hash` |
| T10.7 资金费与告警 | 通过:4 标的 1h 资金费 179 次结算、`by_symbol` 与总账一致,窗口切换不重复结算 |

### T10.1 单标的 1m

数据:Binance UM `BTCUSDT` 1m,2023-06,归档 43200 根 bar,规范时间线
43199 根(首根为 00:01 收盘);`threads=1`,`chunk_bars=5000`。

| 项 | 整段 | 分块 |
| --- | --- | --- |
| `data_hash` | `d9dc81d8...` | `d9dc81d8...` |
| `result_hash` | `f648cd3e...` | `f648cd3e...` |
| 数据内存估算 | 约 2.0 MiB | 每窗口 5000 根 / 约 195.3 KiB |
| 耗时 | 约 0.2 s | 约 0.4 s |
| 进程峰值 RSS | 约 64 MB | 约 69–75 MB |

完整值:

- `data_hash = d9dc81d8395e1d61d0e5e5f65275a6aa1a15286ea5c1fc81081a212129262750`
- `result_hash = f648cd3ec7735ca5e9646898b307411494ea69d30651fe1533c2370da8d6f3d2`
- 2338 笔交易、89 次资金费结算、净支出 `-37.8525589895126`、
  告警 0 条;剔除 `meta` 后两份 `result.json` 逐字段相等。

43k bar 样本的峰值 RSS 由二进制与报告基线主导,单次测量有波动;
分块的内存收益应看数据内存估算与更大样本,耗时增加来自按窗口重读归档。
分块模式 1/2 线程的 `result_hash` 一致。

### T10.2 多标的 1h

数据:Binance UM `BTC/ETH/BNB/SOL` 1h,2024-01,744 根共享时间线;
`xs_momentum`、逆波动率、`chunk_bars=100`、`threads=1`。

| 项 | 整段 | 分块 |
| --- | --- | --- |
| `data_hash` | `da7521b2...` | `da7521b2...` |
| `result_hash` | `54259a53...` | `54259a53...` |
| 数据内存估算 | 约 127.9 KiB | 每窗口 100 根 / 约 15.6 KiB |
| 耗时 | 约 0.02 s | 约 0.06 s |
| 进程峰值 RSS | 约 13–14 MB | 约 13–14 MB |

两份结果均含 198 笔交易、179 次资金费结算与 24 条保证金/资金不足告警;
`by_symbol` 成本之和 `3822.766141709804` 与总账
`3822.766141709807` 在浮点误差内一致。

### T10.3 参数扫描

`fixtures/sweep.toml`(4×4 网格)在 `fixtures/s5_daily.csv` 上整段与
`chunk_bars=50` 分块:

- `sweep_hash` 同为 `41d11ac9f979517f45bd137b80ca41ffd16cf50b02081c92387c6b171e61bfab`;
- top 结果 `result_hash` 同为 `79b4891ede6e6baeb0b556a8a2d70c2775d7d7353f5cad393333c9a11b1a9053`;
- 16 组全部有效,2 条告警;
- `crates/backtest-cli/tests/cli_contract.rs` 的
  `chunked_run_and_sweep_match_full_load` 同时覆盖 `run` 与 `sweep`。

### T10.4 自动窗口与回退

单元测试覆盖 `config/04.intraday.toml` 之外的纯函数路径:

- `explicit_chunk_is_clamped_to_data_length`:`chunk_bars` 大于等于数据长度
  返回整段;
- `auto_chunk_requires_explicit_memory_limit_and_overflow`:无显式内存上限
  或未超限时不启用自动分块;
- `auto_chunk_accounts_for_history_and_workers`:worker 越多窗口越小,
  策略历史从窗口预算中扣除;
- `chunk_bars_is_optional_and_not_hashed`:默认 0,显式值与 `config_hash`
  无关。

真实 1m 运行使用 `chunk_bars=5000 < 43199`,窗口数 9,不触发回退。

### T10.5 窗口边界

- `plan_matches_full_load_and_windows_keep_absolute_indexes`:规划与整段
  装载得到同一时间线,窗口 `base_index` 为绝对下标;
- `load_window_rejects_out_of_range_chunks`:越界窗口返回 Config 错误;
- `streaming_windows_match_full_run` 与
  `streaming_with_warmup_start_matches_full_run`:带历史与预热起点时
  结果逐字段一致,历史不重复喂入;
- `stream_rejects_first_window_after_start_index` 与
  `stream_rejects_gap_between_windows`:首窗未覆盖起点、窗口间空洞返回
  Config 错误,不 panic。

### T10.6 v2 数据哈希

哈希契约见[流程设计 §3.10](../docs/03-流程设计.md):

- 标签 `quant-backtest/data/v2`,按标的名、总行数、固定页
  `PAGE_BARS = 65_536` 的页行数与页摘要汇总;
- 资金费段标签 `quant-backtest/funding/v2`,未启用时不追加;
- CSV 与归档同内容得到同一 `data_hash`:Binance BTCUSDT 1h 2024-01
  两者均为 `b527ca0a...`,`result_hash` 均为 `c64051ce...`;
- `hash_is_independent_of_symbol_feed_order` 与
  `hash_covers_rows_past_first_page` 覆盖符号喂入顺序与跨页摘要;
- 整段与分块的 `data_hash` 相同,见 T10.1/T10.2。

### T10.7 资金费与告警

- 4 标的 1h 分块与整段的 `funding.events = 179`、`paid`、`received`、
  `total_cost` 与 `by_symbol` 逐字段一致;
- 1m 单标的 89 次结算,分块与整段净支出同为 `-37.8525589895126`;
- 引擎单测 `streaming_windows_match_full_run` 在含资金费与告警的数据上
  比较 `equity`、`fills`、`trades`、`metrics`、`drawdowns`、`monthly`、
  `funding`、`warnings` 与关键 `meta` 字段;
- 窗口切换只移动资金费光标,不重新结算。

## 历史哈希重录

v2 上线后,旧 `data_hash`/`result_hash`/`sweep_hash` 全部失效并重录;
`config_hash` 未变。重录对照表见
[008 分块流式加载与哈希 v2](../docs/changes/008-chunked-streaming.md)。

旧验收配置的新值:

| 配置 | `result_hash` 或 `sweep_hash` |
| --- | --- |
| `config/00.smoke.toml` | `efbb00b9...` |
| `config/01.risk-smoke.toml` | `cd330ad9...` |
| `config/02.portfolio-smoke.toml` | `d84b7985...` |
| `config/03.sweep.toml` | `9fe1c058...` |
| `fixtures/sweep.toml` | `41d11ac9...` |
| `fixtures/walkforward.toml` | `4f0e7bd6...` |
| `config/04.intraday.toml` | `7c680a05...` |
| `config/05.weekly.toml` | `7c4c0da1...` |

## 门禁

| 项目 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| `cargo clippy ... -D warnings` | 通过 |
| `cargo test --workspace --all-targets --locked` | 172 个测试通过,0 失败 |
| `cargo nextest run --workspace --all-features --locked` | 通过 |
| `cargo test --workspace --doc --all-features --locked` | 通过 |
| `RUSTDOCFLAGS=-D warnings cargo doc ...` | 通过 |
| `cargo deny check` | advisories/bans/licenses/sources 全部 ok |
| `cargo audit` | 扫描 131 个依赖,无漏洞报告 |
| `cargo build --release --locked` | 通过 |
| `bash scripts/check.sh --ci` | 通过 |

## 干净克隆

提交后从最终提交克隆到与 `data-reader` 同级的新目录,运行
`cargo test --workspace --locked`、`cargo build --release --locked`,
并复跑 1m 与 4 标的 1h 的整段/分块配置;哈希与本文一致。结果补记在
`CHANGELOG.md` 的 M4 验收条目。

## 限制

- 归档窗口仍克隆完整时间线与资金费;CSV 每窗口整文件重扫,分块以时间
  换内存。
- `sweep` 每个 trial 独立流式读取,窗口不在 trial 间共享。
- `walkforward` 首版仍整段装载。
- 43k bar 样本的进程峰值 RSS 由二进制基线主导,内存收益需在超大样本
  与目标机上实测;必要时先缩小 `run.start/run.end` 或改用更粗周期。
