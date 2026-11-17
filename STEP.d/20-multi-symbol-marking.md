# M9 多标的收盘价标记验收

日期:2026-11-17

## 目标

验证多标的 mark/bar 热路径改造:

- `Universe` 新增只读收盘价的 `close()` 与 `try_close()`;
- 引擎标记与资金费结算不再为每根 bar 构造 `BarView`;
- 内置策略只读收盘价列;
- 全部历史哈希逐位不变;
- 多标的合成吞吐相对 M8-B 明显提升,单标的路径不回退。

## 范围与兼容

| 项 | 行为 |
| --- | --- |
| 新增接口 | `Universe::close(symbol, index)`、`Universe::try_close(symbol, index)` |
| 下标语义 | 与 `bar()`/`try_bar()` 一致:减 `base_index`,越界时 panic 或返回 `None` |
| 替换点 | `mark_close()`、`apply_funding()`、`SmaCross::signal()`、`XsMomentum::record()` |
| 保留点 | 止损/止盈、开盘成交、成交量裁剪、保证金投影仍读 OHLCV |
| 标记语义 | 未持仓标的的 `position_views.mark` 仍每根 bar 更新,策略可见行为不变 |
| 不变项 | 无新增配置/CLI/报告字段;`config_hash`、`data_hash`、`result_hash`、`sweep_hash` 与 schema 不变 |

## 验收结果

| 用例 | 结果 |
| --- | --- |
| T16.1 收盘价访问器 | 通过:窗口偏移下 `close`/`try_close` 与 `bar`/`try_bar` 逐值一致 |
| T16.2 逐位等价 | 通过:`result_hash`/`sweep_hash` 与 M8-B 基线逐位一致 |
| T16.3 端到端吞吐 | 通过:64 标的 2.33–2.60 倍,256 标的 2.07–2.19 倍,单标的空策略持平 |
| T16.4 历史哈希 | 通过:8 组历史样本与 W4 扫描哈希全部不变 |

### T16.1 收盘价访问器

`Universe::close()` 与 `try_close()` 直接索引 `BarSeries::close`,
只读一个 `f64`:

- 绝对下标先减 `base_index` 得到窗口内下标,与 `bar()` 的定位方式相同;
- `close()` 用切片索引,越界 panic,与 `bar()` 一致;
- `try_close()` 用 `checked_sub`、`get(symbol)` 与 `get(local)`,
  越界或标的缺失返回 `None`,与 `try_bar()` 一致。

新增单测 `close_matches_bar_field_on_windowed_universe`:

- 构造 `base_index = 2` 的两标的窗口,逐根 bar 比较
  `close() == bar().close`、`try_close() == try_bar().map(|bar| bar.close)`;
- 覆盖窗口起点之前、窗口末尾之后与非法标的三类 `None` 情况。

### T16.2 逐位等价

改造只替换读取路径,不改变任何算术表达式、累加顺序或浮点舍入,
因此 `result_hash`、`sweep_hash` 与 `data_hash` 必须逐位不变。
全部样本在 T16.4 中与 M8-B 基线逐位比对通过。

### T16.3 端到端吞吐

合成基准命令:

```bash
target/release/quant-backtest bench --threads 4 --symbols <N>
```

基线为 M8-B 二进制(`/data/quant-backtest/target/release/quant-backtest`),
M9 二进制来自 `m9` 工作区,两者交替运行;每轮 `bench` 内含 3 次重复,
表中取 5 轮(256 标的为 3 轮)的中位数。单位为 bar步/秒:

| 场景 | M8-B | M9 | 变化 |
| --- | --- | --- | --- |
| 1 标的·空策略 | 102.4M | 97.9M | 持平 |
| 1 标的·双均线 | 49.4M | 58.5M | +18% |
| 10 标的·空策略 | 257.2M | 370.4M | +44% |
| 10 标的·双均线 | 221.6M | 309.3M | +40% |
| 64 标的·空策略 | 190.9M | 495.5M | 2.60 倍 |
| 64 标的·双均线 | 177.8M | 414.5M | 2.33 倍 |
| 256 标的·空策略 | 174.5M | 381.4M | 2.19 倍 |
| 256 标的·双均线 | 168.1M | 347.9M | 2.07 倍 |

W4 端到端(10 标的、1m、10 组参数,`threads = 8`,交替 3 轮):

| 项 | M8-B | M9 |
| --- | --- | --- |
| 耗时中位数 | 14.38 s | 13.34 s |
| 耗时范围 | 14.20–15.06 s | 12.51–14.28 s |
| 峰值 RSS 中位数 | 2660 MB | 2700 MB |

W4 的 Engine 份额被装载与 summary 写入摊薄,端到端约 -7%;峰值差异在
噪声范围内。改造后 64 标的单线程 perf 中,`Universe::bar` 从 47%
降到 6%。

### T16.4 历史哈希

| 样本 | `result_hash` | `data_hash` |
| --- | --- | --- |
| `config/00.smoke.toml`,1 线程 | `efbb00b9f70690e5238630c65dea13922621fff36059ec26367fee66e067b977` | — |
| `config/01.risk-smoke.toml` | `cd330ad9d98802e9d3492a61da4d24fcf5b17f1e51cba8b718d8a72e7a25979e` | — |
| `config/02.portfolio-smoke.toml` | `d84b79855eae8f066b0ec6024673f8df4cffed2b7029ff9c119c84bb3ba8b1be` | — |
| `config/04.intraday.toml` | `7c680a05ae883f02dd8b8a4428a13afc570c86a4d7f16db2badf5a8210b30f86` | — |
| `config/05.weekly.toml` | `7c4c0da1569dea4018d393ef115f5765493e9c9b367148b0c6d2b5efcde9002f` | — |
| `fixtures/walkforward.toml` | `4f0e7bd642c68cf434f25b2d567f1aeeba3f72909a031b87cd96c19d8e9f05c5` | — |
| BTCUSDT spot 1m 2023-05..2026-10 | `f702689588d2a9102ccb713d645af1a9c91da9d03c3d15507330daf5788135a4` | `36ec7667768a435cfbbca1d1ef8c1bbe48a109b3b980d6927a84fe6edcefd72a` |
| BTC/ETH/BNB/SOL UM 1h 2024-01..2024-03 | `7f8239054af618a5d8c764d994bad64f1c42517193ff2a44463ffd1ba6e0e102` | `376a13c56bf6eb7d5c297d25fd9ef208ab0e7f7a1caadc1d21881ddb15d6cabb` |

`fixtures/sweep.toml` 的 `sweep_hash` 为
`41d11ac9f979517f45bd137b80ca41ffd16cf50b02081c92387c6b171e61bfab`;
W4 的 `sweep_hash` 为
`4a3f718e0a77679558c9814776faa3c52ade4ad6bb707ff027f79f657e28ef4a`,
两者都与 M8-B 一致。

## 测试与门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace --locked
cargo build --release --locked
bash scripts/check.sh --ci
```

- 测试:186 个通过,0 失败;比 M8-B 的 185 个增加 1 个。
- fmt、clippy、release 构建通过,无新增告警。

## 干净克隆

待补:合并后在干净克隆上复跑测试、构建与历史哈希。
