# 01 配置与数据

## 目标

把 TOML 与 CSV 变成经过完整校验的 `RunConfig` 和 `Universe`。

## 输入输出

- 输入:`run.toml`、CSV。
- 输出:`RunConfig`、`Universe`、`config_hash`、`data_hash`。

## 实现

- 原始结构 `RawConfig` 只做反序列化;校验后生成 `RunConfig`。
- 路径按进程 cwd 解析。
- CSV 必需列:`ts,symbol,open,high,low,close,volume`。
- `ts` 接受 epoch 毫秒、RFC3339、`YYYY-MM-DD`。
- 文件必须按 `symbol, ts` 升序;重复、乱序、OHLC 非法直接报错。
- 单标的允许缺失 bar;多标的必须共享同一 `ts` 序列。
- 数据哈希覆盖标的、时间戳与 OHLCV,不覆盖文件字节。
- 一次回测只允许一种计价货币与一个年化因子。
- `xs_momentum` 的 `lookback`、`top_k`、`rebalance_bars` 必填;`weighting` 默认 `inverse_vol`,`vol_lookback` 默认等于 `lookback`,`vol_floor` 默认 `1e-4`,`gross` 默认 `1.0`。
- `gross` 与 `sma_cross` 的 `weight` 都受 `1/margin_rate` 上限约束;非法值在启动时报错。
- `[sweep]` 可选;`objective` 默认 `sharpe`,只支持 `sharpe/cagr/calmar`;`top_k` 默认 20 且必须大于 0;`oos_split` 默认 0.7 且必须在 `(0, 1)`。
- `[sweep.grid]` 至少一个参数;参数名必须是当前策略支持的参数;值必须是标量,列表不能为空。
- 扫描组合用 `with_strategy` 重新校验并重算 `config_hash`;非法组合跳过计数,全部非法时报错。
- `config_hash` 不包含 `[sweep]`;同一组策略参数的哈希与扫描目标、top_k 无关。

## 失败模式

| 情况 | 错误 |
| --- | --- |
| 缺字段/未知字段 | `ConfigError::Invalid` 或 TOML 解析错误 |
| 非 1d 周期 | `ConfigError::Unsupported` |
| 空文件 | `DataError::Empty` |
| 重复/乱序 | `DataError::Duplicate` / `Unsorted` |
| OHLC 非法 | `DataError::Ohlc` |
| 多标的时间线不一致 | `DataError::TimelineMismatch` |
| 非法 `[sweep]` 字段 | `ConfigError::Invalid`,含 `sweep.*` 字段名 |
| 网格参数不支持或值非标量 | `ConfigError::Invalid`,含 `sweep.grid.*` |

## 测试

- 合法 6 bar 夹具加载。
- 重复、乱序、负价、NaN、负成交量、缺列。
- 多标的时间线不一致与货币不一致。
- 同内容 CSV 两次加载 `data_hash` 一致。
- `xs_momentum` 合法配置、等权配置、参数哈希、缺失字段、非法 `lookback/top_k/rebalance_bars/gross/vol_lookback/vol_floor/weighting` 与超保证金上限。
- `loads_sweep_grid_and_applies_overrides`:网格加载、标量覆盖与覆盖后哈希变化。
- `with_strategy_rejects_invalid_combination`:覆盖后的非法策略组合。
- `rejects_invalid_sweep_sections`:非法 `top_k/oos_split`、未知参数、非标量网格值。

## 验收

`validate --config config/00.smoke.toml` 退出码 0,打印 `data_hash`、bar 数与时间范围。

## 依赖

无;被 02 与 04 使用。
