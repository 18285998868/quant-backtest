# 014 多标的收盘价标记

## 背景

合成基准的单线程 perf 剖析(64 标的、2500 bar、500 任务)显示:

- `<Engine>::advance` 占 49%,`<Universe>::bar` 占 47%;
- `mark_close()` 每根 bar 对全部标的各调用一次 `bar()`,只取 `close`;
- `apply_funding()` 结算资金费时同样只取 `close`;
- `SmaCross` 与 `XsMomentum` 每根 bar 每标的调用一次 `try_bar()`,
  也只读 `close`。

`bar()` 每次都要构造 56 字节的 `BarView`,并从 ts/open/high/low/close/
volume 六个分列数组各取一个元素。标的数增大后,这些列分散在六个独立
分配中,单次调用的缓存与边界检查成本远高于读取一个 `f64`。

标记循环的调用次数是 `标的数 × bar 数`,不随持仓数变化。在
500 标的 × 2500 bar × 1000 组参数的目标下,它是唯一随标的数线性
增长的常驻成本,也是本轮唯一仍有规模价值的优化点。

## 决策

1. 新增 `Universe::close(symbol, index)` 与
   `Universe::try_close(symbol, index)`:
   - 直接按列取值,只读一个 `f64`;
   - 下标语义与 `bar()`/`try_bar()` 一致:都减去 `base_index`,
     越界时 `close` panic、`try_close` 返回 `None`。
2. 替换只读收盘价的调用点:
   - `Engine::mark_close()` 的权益标记;
   - `Engine::apply_funding()` 的资金费结算价;
   - `SmaCross::signal()` 与 `XsMomentum::record()` 的收盘价。
3. 保留其余 `bar()` 调用点:
   - 止损/止盈触发需要 high/low/open;
   - 开盘成交、成交量裁剪与保证金投影需要 open/volume;
   - 它们只在持仓或成交时触发,不随标的数在每根 bar 上线性增长。
4. 不变项:
   - 不改变任何算术表达式、累加顺序或浮点舍入;
   - 不新增配置、CLI 参数或报告字段;
   - `config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与报告
     schema 不变;
   - 未持仓标的的 `position_views.mark` 仍然每根 bar 更新,不引入
     active set,策略可见语义不变。

## 影响文件

- `crates/backtest-core/src/model.rs`:`close()`、`try_close()` 与
  窗口偏移下的等价性单测。
- `crates/backtest-core/src/engine/mod.rs`:`mark_close()` 与
  `apply_funding()` 改用 `close()`。
- `crates/backtest-strategy/src/lib.rs`:`SmaCross` 与 `XsMomentum`
  改用 `try_close()`。
- `docs/03-流程设计.md`:`Universe` 接口补 `close()`/`try_close()`。
- `CHANGELOG.md`、`STEP.md`、`STEP.d/20-multi-symbol-marking.md`。

## 验证

基线为 M8-B 二进制(`/data/quant-backtest/target/release/quant-backtest`),
M9 二进制来自同一台机器的 `m9` 工作区。合成基准与 W4 交替运行,
每次 `bench` 内含 3 次重复,下表取 5 轮(256 标的为 3 轮)的中位数:

| 场景(4 线程) | M8-B | M9 | 变化 |
| --- | --- | --- | --- |
| 1 标的·空策略 | 102.4M | 97.9M | 持平(噪声) |
| 1 标的·双均线 | 49.4M | 58.5M | 约 +18% |
| 10 标的·空策略 | 257.2M | 370.4M | 约 +44% |
| 10 标的·双均线 | 221.6M | 309.3M | 约 +40% |
| 64 标的·空策略 | 190.9M | 495.5M | 2.60 倍 |
| 64 标的·双均线 | 177.8M | 414.5M | 2.33 倍 |
| 256 标的·空策略 | 174.5M | 381.4M | 2.19 倍 |
| 256 标的·双均线 | 168.1M | 347.9M | 2.07 倍 |

单位为 bar步/秒,即每秒回放的 symbol-bar 数。单标的空策略不变符合
预期:每根 bar 只有一次标记调用。单标的双均线的提升来自策略侧不再
构造 `BarView`。

W4(10 标的、1m、10 组参数)端到端:

| 项 | M8-B | M9 |
| --- | --- | --- |
| 耗时中位数 | 14.38 s | 13.34 s |
| 耗时范围 | 14.20–15.06 s | 12.51–14.28 s |
| 峰值 RSS 中位数 | 2660 MB | 2700 MB |

W4 的耗时由装载、网格回放与 summary 写入共同决定,引擎标记只占一部分,
因此端到端约 -7%;峰值差异在噪声范围内。

改造后同一基准的 perf 剖析中,`Universe::bar` 从 47% 降到 6%,
其余时间进入 `Engine::advance` 内联的标记与策略循环。

逐位回归:全部历史 `result_hash`/`sweep_hash` 保持不变,详见
[M9 验收](../../STEP.d/20-multi-symbol-marking.md)。

## 限制与回退

- `close()` 仍做两次边界检查;要再降低需要按标的缓存标记价格或引入
  active set,后者会改变未持仓标的最新价语义,需要单独决策。
- 保证金投影 `margin_feasible()` 在保证金约束生效时仍对全部标的
  逐个读 open,最坏是每个成交 `60 × 标的数` 次;该路径只在杠杆多标的
  换仓 bar 上放大,本轮未改。
- 回退只需把四处调用恢复为 `bar()`/`try_bar()` 取 `close`,
  哈希与语义不受影响。
