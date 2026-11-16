# M2 归档数据源验收

日期:2026-11-16

## 目标

验证直接读取 `data-reader` 归档的完整链路:

- Binance spot/UM/CM klines 与 Dukascopy bid/ask 蜡烛;
- 归档开盘时刻正确转换为回测要求的收盘时刻;
- 跨日/跨周 bar 不被归档文件名日期窗口切掉;
- 同一内容 CSV 与归档得到相同 `data_hash`/`result_hash`;
- 缺失文件只报告,不静默补齐;
- 多标的时间线一致性与 1/2 线程确定性不退化。

## 支持矩阵

| provider | 市场/报价 | 周期 | 时间戳 |
| --- | --- | --- | --- |
| Binance | spot, futures/um, futures/cm | 1m/3m/5m/15m/30m/1h/2h/4h/6h/8h/12h/1d/3d/1w | `ts = close_time + 1` |
| Dukascopy | bid, ask | 1m/1h/1d | `ts = open_time + timeframe` |

配置示例:

```toml
[run]
initial_cash = 100000.0
start = "2024-01-01T00:00:00Z"
end = "2024-02-01T00:00:00Z"
output_dir = "out/binance_1h"
threads = 1
memory_limit_gb = 0
timeframe = "1h"

[source]
kind = "binance"
root = "../data/binance"
market = "spot"
```

Dukascopy 把 `[source]` 换成:

```toml
[source]
kind = "dukascopy"
root = "../data"
price_type = "bid"
```

`root` 下必须分别存在 `data/{market}/monthly|daily/klines` 与
`dukascopy/{symbol}`;两个仓库须位于同一父目录。

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
```

结果:151 个测试全部通过;fmt、check、clippy、doctest、rustdoc、deny、
audit 与 release 构建全部通过。

## 真实数据结果

| 场景 | 数据 | bar 数 | `data_hash` | `result_hash` |
| --- | --- | --- | --- | --- |
| Binance 单标的 1h | BTCUSDT 2024-01 | 744 | `d79c1706...` | `5787aa02...` |
| 等价 CSV | 同上 | 744 | `d79c1706...` | `5787aa02...` |
| Binance 单标的 1m | BTCUSDT 2023-06 | 43200 | `0374b43a...` | `4c82b97a...` |
| Binance 4 标的 1h | BTC/ETH/BNB/SOL 2024-01 | 744 | `fec3c66b...` | `669ad911...` |
| Dukascopy 单标的 1h | EURUSD 2023-07 | 743 | `307e4e3d...` | `8516fb78...` |

完整哈希:

- Binance 1h / CSV:`data_hash` =
  `d79c170636eb695209f98bccb514dff1b656daa38cab81ec089b991f2a45bc0e`,
  `result_hash` =
  `5787aa022674755b747c444c282d88a95a641cbaace0fbf94146bbe0482453f5`。
- Binance 1m:`data_hash` =
  `0374b43aa2151dea7661f018b3df58cc75a27d682995f323b02b5e059bd30988`。
- 4 标的 1h:`data_hash` =
  `fec3c66bc3e3c812917f076d1d8cd8be2f6c9e2347123cf8da8221da76b71516`。
- Dukascopy 1h:`data_hash` =
  `307e4e3d1d1c3feceeafd36bddb59e85d035d8bd6a09dace5ea32730cc907cd9`。

## 关键断言

1. Binance 1h 归档与等价 CSV 的 `data_hash`、`result_hash`、指标、逐笔
   交易和净值曲线完全一致。
2. Binance 1h 首根 bar 的 `ts = 2024-01-01T00:00:00Z`,来自 2023-12 归档;
   若只请求 2024-01 文件会丢这根 bar,说明起点前移一个周期有效。
3. Binance 1m 2023-06 恰好 43200 根,无缺口、无重复。
4. 4 标的 1h 全部 744 根且共享同一时间线;组合回测完成 41 笔交易。
5. Dukascopy EURUSD H1 输出 743 根,首根 `ts = 2023-07-01T01:00:00Z`;
   上月归档文件缺失被记为 `缺失=1`,没有补造 bar。
6. 同一配置重复运行、`--threads 1` 与 `--threads 2` 的 `result_hash`、
   指标和逐笔交易一致。

## 负向路径

- `run.data` 与 `[source]` 同时出现:`ConfigError::Invalid`,字段 `source`。
- 两者都缺失:`ConfigError::Invalid`,字段 `run.data`。
- provider 专属字段用错(如 Binance 写 `price_type`、Dukascopy 写 `market`)
  或周期不在支持矩阵:配置阶段报错。
- 归档根不存在、文件损坏、行非法:返回含 provider/root/symbol 的
  `DataError::Archive`,不 panic。
- 单标的在 `[start, end)` 内无 bar:返回 `DataError::EmptyRange`。

## 限制

- 只验证了 Binance spot 与 Dukascopy EURUSD H1;UM/CM 与 Dukascopy M1/D1
  由支持矩阵和上游测试覆盖,未做本地真实数据验收。
- 分钟级数据仍全量载入内存;1m 一年约 525600 根/标的,大组合需先看内存估算。
- 不做周期合成、复权、时区转换或缺失 bar 填补。
