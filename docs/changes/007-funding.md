# 007 资金费与合约成本

## 背景

[需求规格](../01-需求规格.md) 的 S17/AC21 要求 Binance 期货按真实归档结算
资金费。M2 已支持 `futures/um`/`futures/cm` klines,但永续持仓的资金费仍被
忽略,结果对多头/空头成本存在系统性偏差;报告也没有资金费字段。

## 决策

1. 默认开关:Binance `futures/um`/`futures/cm` 默认启用;`spot` 与
   Dukascopy 默认关闭。`source.funding` 可显式覆盖。
   - `spot` 或 Dukascopy 设 `funding = true` 在配置阶段报
     `source.funding` 错误,不静默忽略。
2. 数据形态:每个标的保存稀疏 `FundingSeries { symbol, ts, rate }`,
   序列按 `ts` 严格递增;费率带符号,正数表示多头向空头支付。
3. 结算窗口:结算时刻落在 `(上一根 bar 收盘, 本根 bar 收盘]` 时,
   窗口内费率求和;首根 bar 向前放宽一个周期;名义价值用本根收盘价,
   现金变化为 `-qty × close × multiplier × rate`。
4. 归档时间戳:Binance `fundingRate` 的 `calc_time` 偶尔比整点晚 1–2ms,
   装载时向下取整到秒。否则这些结算会被分到下一根 bar,错用下一根收盘价。
5. 哈希:`config_hash` 不纳入资金费开关;启用资金费时 `data_hash` 追加
   `quant-backtest/funding/v1` 段;未启用时 `data_hash` 与旧版本一致。
   M4 把数据哈希升级为 v2,资金费段同步改为 `funding/v2`,历史哈希按 v2
   重录,见 [008](./008-chunked-streaming.md)。
6. 结果与报告:`BacktestResult.funding: Option<FundingSummary>`,
   含 `total_cost`、`paid`、`received`、`events` 与 `by_symbol`;
   未启用时 JSON 省略。HTML 成本章节单列资金费,并渲染按标的明细。
   报告 schema 由 1.1 升到 1.2。
7. walk-forward:跨窗口合并资金费汇总,`by_symbol` 按标的 ID 合并。

1h 及更短周期与逐次结算等价;日线及更长周期把窗口内多次结算合并到
bar 收盘,属于近似,在报告假设与验收文档中披露。

## 变更文件

- `crates/backtest-core/src/model.rs`:`FundingSeries`、`SymbolFunding`、
  `FundingSummary`、`Universe.funding`、`BacktestResult.funding`、
  内存估算。
- `crates/backtest-core/src/config.rs`:`RawSource.funding`、
  `BinanceSource.funding`、`BinanceMarket::has_funding()`、解析与校验。
- `crates/backtest-core/src/engine/mod.rs`:资金费光标、结算扣费、汇总与单测。
- `crates/backtest-core/src/error.rs`:`DataError::Funding`。
- `crates/backtest-data/src/archive.rs`:读取 `fundingRate` 月度归档、
  区间过滤、亚秒时间戳归一与诊断。
- `crates/backtest-data/src/lib.rs`:`finalize` 资金费校验/过滤、
  `data_hash` 追加资金费段。
- `crates/backtest-report/src/lib.rs`:schema 1.2、JSON `funding`、
  HTML 资金费行/明细表、假设文案与测试。
- `crates/backtest-cli/src/walkforward.rs`:跨窗口合并资金费。
- `crates/backtest-cli/src/lib.rs`:CLI 打印资金费汇总。
- `fixtures/report_case.json`:schema 1.2 与 `"funding": null`。
- `config/00.example.toml`、`README.md`、`STEP.md`、`docs/01`、`docs/02`、
  `docs/03`、`CHANGELOG.md`、`STEP.d/13-funding.md`。

## 验证

- 单标的 BTCUSDT UM 1h 2024-01:744 根 bar,93 次区间内结算,87 次实际扣费,
  净支出 `-34.92654974212712`;独立复算差 `1.8e-13`。
- 1/2 线程 `result_hash` 同为
  `a8d5b41e452fa133c65c02dc4f92ebb09b071222017c5fa06b183a96fdf90438`。
- 4 标的组合:179 次结算,`by_symbol` 成本与次数之和等于总账;
  1/2 线程哈希一致。
- `funding = false` 与 spot 冒烟回归通过;spot 结果哈希保持 `efbb00b9...`。
- 负向路径覆盖 spot/Dukascopy 误开、资金费乱序/非有限值与区间内无 bar。
- 完整门禁与干净克隆验收结果见 [M3 验收](../../STEP.d/13-funding.md)。

## 限制

- 只支持 Binance 期货月度 `fundingRate` 归档;其他交易所与日归档未验证。
- 日线及更长周期按 bar 收盘近似;bar 内多次结算、bar 内风控退出与结算的
  先后顺序不做逐笔还原。
- 不建模期货展期、融券费、隔夜利息、强平与保证金追缴。
