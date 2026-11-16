# 005 多周期支持

## 背景

[需求规格](../01-需求规格.md) 的 AC18 要求周期契约可用,但 S1–S6 只允许
`timeframe = "1d"`。M1 解锁小时、分钟与周线,并补齐年化、报告和回归验证。

## 决策

1. 配置接受 `1m/5m/15m/30m/1h/4h/1d/1w` 及同单位正整数周期。
   引擎本就按 bar 事件循环,收盘信号、下一根开盘成交、风控与成本逻辑不需要改。
2. 周期由 `run.toml` 声明,不从 bar 间隔推断,也不做严格间隔校验。
   单标的允许缺失 bar,美股/外汇的会话边界还会让最后一根 bar 短于声明周期;
   用间隔反推周期会误伤合法数据。数据提供方必须保证一个文件只有一个周期。
3. 年化因子继续用 `periods_per_year × asset_class` 推导,`periods_per_year`
   可覆盖。推导值:
   - 1m:加密 525600,美股 98280,外汇 362880。
   - 1h:加密 8760,美股 1638,外汇 6048。
   - 4h:加密 2190,美股 410,外汇 1512。
   - 1w:统一 52。
4. 报告使用实际 `meta.timeframe`;假设与月度说明不再写死日线。
5. 数据仍全量载入内存。CLI 输出 `Universe::estimated_data_bytes()` 的粗略估算,
   超过解析后的内存上限时向 stderr 告警;不实现分块/按标的加载。

## 变更文件

- `crates/backtest-core/src/config.rs`:移除 `1d` 限制,新增周期接受与覆盖测试。
- `crates/backtest-core/src/model.rs`:补全各周期年化测试,新增内存估算。
- `crates/backtest-cli/src/lib.rs`:输出数据内存估算与超限告警。
- `crates/backtest-report/src/lib.rs`:更新假设与月度说明。
- `config/04.intraday.toml`、`config/05.weekly.toml`:1h/1w 冒烟配置。
- `fixtures/intraday_1h.csv`、`fixtures/weekly.csv`:合成夹具。
- `crates/backtest-cli/tests/timeframe_contract.rs`:周期契约集成测试。
- `docs/03-流程设计.md`、`docs/01-需求规格.md`、`STEP.md`、`README.md`、`CHANGELOG.md`。

## 验证

- `timeframe_contract` 覆盖 1h/1w 运行、年化因子、报告、非法周期与
  1h 扫描/walk-forward。
- 1h 冒烟产生 2 笔交易、4 笔成交,`periods_per_year=8760`。
- 1w 冒烟产生 16 个权益点,`periods_per_year=52`。
- 详细证据见 [M1 验收](../../STEP.d/11-m1-acceptance.md)。

## 限制

- 未实现分钟级分块/按标的加载;大文件仍可能 OOM,只有估算与告警。
- 不建模交易所日历、时区转换与会话边界,输入必须是 UTC 且一个文件一个周期。
- 不校验 bar 间隔与声明周期严格一致;错误声明会得到错误的年化因子。
- 混合资产类别的年化因子必须一致,否则配置直接拒绝。
