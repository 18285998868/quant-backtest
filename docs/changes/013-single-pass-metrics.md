# 013 指标单遍扫描

## 背景

M8 引入 metrics-only 回放后,网格与样本外不再保留每个组合的完整结果,
但 `compute_metrics()` 对权益曲线仍然做约十次遍历,并分配两个与曲线等长
的 `Vec<f64>`:

- `equity_returns()` 构造收益向量,`mean()`、`sample_std()` 与下行波动
  再各遍历一次;
- `series_drawdowns()` 构造回撤向量,最大回撤与 `ulcer` 再各遍历一次;
- `all_drawdown_episodes()`、平均权益与暴露比例各自遍历权益。

W3 的 1000 组参数、单条曲线约 129 万点,重复遍历与临时分配成为
metrics-only 阶段的主要剩余成本。

## 决策

1. 合并权益扫描:
   - `compute_metrics_with_period_sharpe()` 用一次权益遍历同时累计收益
     个数、收益和与下行差平方和,峰值、最大回撤与回撤平方和,回撤段落
     的峰值时间,权益和与暴露 bar 数;
   - 最长回撤天数在段落恢复点直接取 `days_between`,遍历结束后再补
     未恢复段落。
2. 样本标准差保留第二次遍历:
   - 公式需要均值,改为 `sum(x^2)` 会改变浮点舍入;
   - 第二次遍历按同一 `previous > 0` 规则重算收益,不再分配收益向量。
3. 删除 `series_drawdowns()`;`ulcer` 改为单次扫描累计后除以权益点数。
4. 逐位不变:
   - 每个累加器的相加顺序与旧实现一致,只是合并到同一循环;
   - 新增 `fused_metrics_match_legacy_bits`,在同一模块保留旧实现,
     对全部指标按 `to_bits()` 比较;
   - 覆盖空曲线、单点、零权益、回撤段落、5000 点曲线与含未平仓交易
     的输入。
5. 不变项:
   - 不新增配置、CLI 参数或报告字段;
   - `config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与报告
     schema 不变;
   - 完整结果路径仍单独计算回撤段落明细与月度收益,metrics-only 路径
     本来也不产出这两项。

## 影响文件

- `crates/backtest-core/src/metrics/mod.rs`:单遍指标扫描与逐位等价测试。
- `CHANGELOG.md`、`STEP.md`、`STEP.d/19-single-pass-metrics.md`。

## 验证

基线为 M8 二进制(`/data/quant-backtest/target/release/quant-backtest`),
W2 与 W3 按 base/new 交替运行(W2 三轮、W3 五轮),计时用 `getrusage`
包装脚本:

| 场景 | M8 耗时 / 峰值 | M8-B 耗时 / 峰值 | `sweep_hash` |
| --- | --- | --- | --- |
| W2 | 4.32–4.54 s / 708–733 MB | 3.51–3.81 s / 649–665 MB | `32f6ee94...` |
| W3 | 19.8–21.2 s / 1252–1339 MB | 14.3–17.6 s / 1303–1309 MB | `e19d0b73...` |

按中位数计,W2 从 4.38 s 降到 3.67 s(约 16%),W3 从 20.73 s 降到
15.00 s(约 28%);W2 峰值下降约 9%,W3 峰值基本持平。

W2 与 W3 的 `sweep_hash` 与 M8 逐位一致。历史哈希、完整门禁与干净克隆
细节见 [M8-B 验收](../../STEP.d/19-single-pass-metrics.md)。

## 限制与回退

- 求样本标准差仍需第二次权益遍历;要压成单遍必须接受浮点舍入变化。
- 完整结果路径的 `drawdown_episodes()` 与 `monthly_returns()` 仍各自
  扫描一次;它们不在网格与样本外的热路径上,本轮未合并。
- 如需回退,恢复 `series_drawdowns()` 与逐项遍历即可,指标语义与哈希
  均不受影响。
