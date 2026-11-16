# M3 资金费与合约成本验收

日期:2026-11-16

## 目标

让 Binance 期货回测按真实归档结算资金费,验证:

- 多空方向符号正确;
- 8h 结算不重不漏,与独立复算一致;
- `result_hash` 可复现,1/2 线程一致;
- spot 结果不受影响,期货可显式关闭;
- 报告单列资金费成本,按标的可对账。

## 结算规则

1. 仅 Binance `futures/um`/`futures/cm` 默认启用;`spot` 与 Dukascopy 默认关闭。
2. `source.funding = false` 关闭期货资金费;`spot` 或 Dukascopy 设
   `funding = true` 在配置阶段报 `source.funding` 错误。
3. 结算时刻落在 `(上一根 bar 收盘, 本根 bar 收盘]` 时,窗口内费率求和;
   首根 bar 的窗口向前放宽一个周期;不重不漏。
4. 现金变化为 `-qty × close × multiplier × rate`:正费率多头支付、空头收取,
   空头负 qty 自动反号。名义价值用本根收盘价。
5. 归档 `calc_time` 的 1–2ms 偏移向下取整到秒,避免被分到下一根 bar。
6. 启用资金费时 `data_hash` 追加 `quant-backtest/funding/v2` 段;
   未启用时不追加。`config_hash` 不纳入资金费开关。M4 将原 v1 段升级为
   v2,结算语义未变,历史哈希随数据哈希 v2 一并重录。

1h 及更短周期与逐次结算等价。日线及更长周期把窗口内多次结算合并到
bar 收盘,属于近似,见文末限制。

## 配置

```toml
[source]
kind = "binance"
root = "../data/binance"
market = "futures/um"
# funding = false  # 可选:关闭资金费
```

验收使用真实归档:

- klines:`data/binance/data/futures/um/monthly/klines/{symbol}/1h/`
- 资金费:`data/binance/data/futures/um/monthly/fundingRate/{symbol}/`

## 门禁

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked
cargo test --workspace --doc --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features --locked
cargo deny check
cargo audit
cargo build --release --locked
bash scripts/check.sh --ci
```

结果见“门禁结果”。

## 单标的真实数据

配置:BTCUSDT,`futures/um`,1h,2024-01,初始资金 100000,fee 5 bps,
slippage 2 bps,`sma_cross(6, 24)`,允许做空,`threads = 1`。

| 指标 | 值 |
| --- | --- |
| bar 数 | 744 |
| 读取资金费记录 | 96 条(2023-12 与 2024-01 两个归档) |
| 区间内结算 | 93 次 |
| 实际扣费事件 | 87 次(其余 6 次持仓为空) |
| 净支出 | `-34.92654974212712` |
| 付出 | `379.6266249664753` |
| 收到 | `414.5531747086024` |
| `config_hash` | `d96b74b5f665141900ff40953d6396d39e90367963583ab744b297a92fe0a2fd` |
| `data_hash` | `499b577cd620d60446bbfcd0f71e3bb1e6d319d3992f689e22e88fc09e1db2e6` |
| `result_hash` | `a8d5b41e452fa133c65c02dc4f92ebb09b071222017c5fa06b183a96fdf90438` |

其中 15 条 `calc_time` 带 1–2ms 偏移,装载时归一为整点。

### 独立复算

不调用回测引擎,只用结果 JSON 的逐笔交易、原始 klines 收盘价与原始
funding CSV 重放:

- 对每个结算时刻,找到覆盖该 bar 的交易,按
  `sign × qty × close × rate` 累加;
- 空头 `sign = -1`,多头 `sign = +1`;
- 区间 `[entry_ts, exit_ts)`,与引擎“入场 bar 计费、出场 bar 不计费”一致。

复算结果 `-34.926549742126944`,引擎 `-34.92654974212712`,差
`1.8e-13`,87 次事件完全一致。

首笔可手算:

| 字段 | 值 |
| --- | --- |
| 结算时刻 | 2024-01-03T00:00:00Z |
| 方向 | 空头 |
| 数量 | 2.209025835096679 |
| 收盘价 | 44979.8 |
| 费率 | 0.00021159 |
| 现金变化 | `-21.023908303080535`(收到) |

### 确定性

| 运行 | `threads` | `result_hash` |
| --- | --- | --- |
| 第 1 次 | 1 | `a8d5b41e...` |
| 第 2 次 | 2 | `a8d5b41e...` |

两次 `meta.threads` 分别为 1 与 2,资金费汇总、指标与逐笔交易一致。

## 关闭资金费与 spot 回归

`funding = false` 的同一配置:

| 字段 | 启用 | 关闭 |
| --- | --- | --- |
| `config_hash` | `d96b74b5...` | `d96b74b5...` |
| `data_hash` | `499b577c...` | `b61b24e5...` |
| `result_hash` | `a8d5b41e...` | `96c7425b...` |
| 总收益 | `-10.3308%` | `-10.3603%` |
| 资金费字段 | 有 | 无(JSON 省略) |

spot 冒烟 `config/00.smoke.toml` 在 M3 前后均为
`result_hash = efbb00b9f70690e5238630c65dea13922621fff36059ec26367fee66e067b977`,
说明现货路径未受影响。

历史说明:`144d1118...` 是 S1–S4 的 spot 基线;S5 新增 `cost_drag` 指标后
按 [S5 验收](./09-s5-acceptance.md) 变为 `d771891d...`,与 M3 无关。
M4 的 v2 哈希重录后当前值为 `efbb00b9...`。

## 多标的真实数据

配置:BTCUSDT/ETHUSDT/BNBUSDT/SOLUSDT,`futures/um`,1h,2024-01,
初始资金 1000000,`xs_momentum(lookback=20, top_k=2, rebalance_bars=5,
inverse_vol, gross=0.95)`,`threads = 1/2`。

| 标的 | 净支出 | 结算次数 |
| --- | --- | --- |
| BTCUSDT | `1186.9890130712902` | 57 |
| ETHUSDT | `481.00446992126905` | 38 |
| BNBUSDT | `255.86664304834034` | 43 |
| SOLUSDT | `1898.906015668905` | 41 |
| 合计 | `3822.766141709807` | 179 |

- `sum(by_symbol.cost) = 3822.7661417098043`,与 `total_cost` 差 `3e-12`;
- `sum(by_symbol.events) = 179 = events`;
- `paid - received = 3822.766141709807 = total_cost`;
- 1/2 线程 `result_hash` 同为
  `54259a53f3eb610ba442ff721ee489bed09187fcf263322edd509a769058d286`;
- `data_hash = da7521b26452e214f431cdb90c619ba4efa2a8e36804bc07e16d5caa49d0a262`。

该配置仍有 24 条保证金/现金裁剪告警,属于执行约束,不影响资金费总账与
`by_symbol` 的对账关系。

## 报告与 schema

- `BacktestResult.funding: Option<FundingSummary>`;未启用时序列化省略。
- `ReportJson` 的 `funding` 为 `null` 或汇总对象;schema 1.2。
- HTML 成本章节显示资金费净支出、付出/收到、结算次数与按标的明细。
- `fixtures/report_case.json` 已补 `"funding": null` 并通过 schema 校验。
- walk-forward 跨窗口合并 `total_cost/paid/received/events/by_symbol`。

## 负向路径

- `spot` + `funding = true`: `ConfigError::Invalid`,字段 `source.funding`,退出码 2。
- Dukascopy + `funding = true`: 同上。
- 期货 + `funding = false`: 配置通过,结果无 `funding` 字段。
- 资金费时间戳不严格递增或费率非有限值:`DataError::Funding`,退出码 3。
- 单标的区间内无 bar:仍按 `DataError::EmptyRange` 报错。

## 限制

- 只支持 Binance 期货月度 `fundingRate` 归档;其他交易所、日归档与
  4h 结算历史未单独验证。
- 1h 及更短 bar 与结算时刻对齐时精确;日线及更长周期把多次结算合并到
  bar 收盘,名义价值用该收盘价。
- bar 内风控退出与资金费的先后顺序不做逐笔还原;若结算发生在 bar 内
  风控退出之前,当前实现仍按退出后的空仓处理。该偏差在 1h 且无 bar 内
  止损时不存在。
- 不建模期货展期、融券费、隔夜利息、强平与保证金追缴。

## 门禁结果

2026-11-16 在 2 核 Linux 环境运行:

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | 通过 |
| `cargo nextest run --workspace --all-features --locked` | 159 个测试通过(9 个二进制,0 跳过) |
| `cargo test --workspace --doc --all-features --locked` | 通过(5 个 crate,0 个 doctest) |
| `RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps --all-features --locked` | 通过 |
| `cargo deny check` | advisories/bans/licenses/sources 全部 ok |
| `cargo audit` | 扫描 131 个依赖,无漏洞报告 |
| `cargo build --release --locked` | 通过 |
| `bash scripts/check.sh --ci` | 通过 |

门禁后重跑验收配置,哈希与正文一致:

| 配置 | 线程 | `result_hash` |
| --- | --- | --- |
| `fund_1h` | 1/2 | `a8d5b41e...` |
| `fund_multi` | 1/2 | `54259a53...` |
| `nofund_1h` | 1 | `96c7425b...` |
| `config/00.smoke.toml` | auto | `efbb00b9...` |

## 干净克隆

把提交 `558be63` 克隆到与 `data-reader` 同级的全新目录后验收:

- `cargo test --workspace --locked`:159 个通过,0 失败;
- `cargo build --release --locked`:成功;
- path 依赖 `../../../data-reader/crates/binance-core` 与锁文件在全新工作区
  可解析;
- `fund_1h` 克隆运行 `result_hash=a8d5b41e...`,资金费净支出
  `-34.92654974212712`、87 次,与工作副本一致;
- `config/00.smoke.toml` 克隆运行 `result_hash=efbb00b9...`,未产生资金费字段。

## 相关文档

- [流程设计 §3.1、§3.7、§3.9 与 M3](../docs/03-流程设计.md)
- [变更记录 007](../docs/changes/007-funding.md)
- [M2 归档数据源验收](./12-archive-source.md)
- [S5 指标与报告验收](./09-s5-acceptance.md)
