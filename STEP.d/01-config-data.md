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

## 失败模式

| 情况 | 错误 |
| --- | --- |
| 缺字段/未知字段 | `ConfigError::Invalid` 或 TOML 解析错误 |
| 非 1d 周期 | `ConfigError::Unsupported` |
| 空文件 | `DataError::Empty` |
| 重复/乱序 | `DataError::Duplicate` / `Unsorted` |
| OHLC 非法 | `DataError::Ohlc` |
| 多标的时间线不一致 | `DataError::TimelineMismatch` |

## 测试

- 合法 6 bar 夹具加载。
- 重复、乱序、负价、NaN、负成交量、缺列。
- 多标的时间线不一致与货币不一致。
- 同内容 CSV 两次加载 `data_hash` 一致。

## 验收

`validate --config config/00.smoke.toml` 退出码 0,打印 `data_hash`、bar 数与时间范围。

## 依赖

无;被 02 与 04 使用。
