# 02 执行引擎

## 目标

实现收盘信号、下一根开盘成交、目标权重差、成本、手数与现金/保证金约束。

## 输入输出

- 输入:`Universe`、`EngineInput`、`dyn Strategy`。
- 输出:`BacktestResult`(未计算指标前的原始净值/成交/交易)。

## 执行顺序

每个 bar:

1. 开盘:处理上一根收盘排队的订单,按开盘价加滑点成交。
2. 盘中:S1 不做止损/止盈。
3. 收盘:按 close 估值,更新权益与回撤。
4. 收盘:调用策略,校验目标权重,生成下一根订单。

## 规则

- 目标名义价值 = `weight × 信号 bar 收盘权益`。
- 目标数量 = 目标名义价值 / `(成交价 × multiplier)`,向下取整到 `lot_size`。
- 买入价 = `open × (1 + slippage)`,卖出价 = `open × (1 - slippage)`。
- 手续费 = `abs(notional) × fee_bps / 10000 + fee_fixed`。
- 现金不足时裁剪买入数量;不足一个交易单位则拒绝并告警。
- 成交量参与率限制导致部分成交时,剩余目标顺延到下一根。
- 策略总目标敞口超过 `1/margin_rate` 时返回 `ExecutionError`。
- 未出现在策略输出中的标的保持原目标;策略只在信号变化时输出。

## 账户

- `equity = cash + Σ(qty × mark × multiplier)`。
- `gross_exposure = Σ(abs(qty × mark × multiplier))`。
- `required_margin = Σ(abs(qty × mark × multiplier) × margin_rate)`。
- 成交后必须满足 `cash >= 0` 与 `equity >= required_margin`。

## 测试

- T1.1 手算多头。
- T1.2 次日开盘价与收盘价不同。
- T1.3 缺失 bar 顺延。
- T1.4 买卖各 5 bps,净 PnL 989.5。
- 空头开平仓现金流。
- 权重非有限、总敞口超限。

## 验收

T1.1–T1.4 全部通过;同输入两次运行结果哈希一致。

## 依赖

依赖 01 的 `Universe` 与 `Strategy` 契约。
