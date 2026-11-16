# 008 分块流式加载与哈希 v2

## 背景

[需求规格](../01-需求规格.md) 的 S15/S18 要求分钟级数据可用,但此前加载器把
整段 OHLCV 放进内存:500 标的 × 10 年 1m 约 24 GB,超过 8 GB 预算。
M2/M3 又要求 CSV 与归档、启用/关闭资金费都能复现同一结果,而旧数据哈希
按“符号 → 行数 → 逐行”平铺,必须先知道行数并一次性按符号喂入,无法在
时间分块时保持同一结果。

## 决策

1. 装载开关:
   - `run.chunk_bars > 0` 强制分块;若大于等于数据长度则回退整段装载。
   - `run.chunk_bars = 0` 且 `memory_limit_gb > 0`:先规划数据并估算全量内存,
     仅当估算超过解析后的内存上限时自动分块。
   - 两者都为 0:整段装载,不额外扫描。
   - `chunk_bars` 不参与 `config_hash`。
2. 窗口语义:
   - 窗口按绝对下标 `[start, end)` 连续推进,不重叠、不留空洞。
   - 块首可带 `strategy.history_bars()` 根历史,默认等于 `warmup_bars()`;
     历史只供读取,不重复喂给策略、不重复记账。
   - `Universe.base_index` 记录窗口首根绝对下标;`bar()`/`try_bar()` 接收
     绝对下标;`window_start()`/`window_end()` 返回窗口边界。
3. 状态延续:
   - `EngineState` 携带现金、持仓、挂单、目标权重、权益、成交、交易、告警、
     资金费光标与下一根下标;窗口只重建只读 `Universe`。
   - 策略实例只构造一次,指标状态跨窗口保留。
   - 最后一块负责估值、未成交订单告警与指标汇总。
4. 自动窗口:
   - 并行 worker 数由线程解析得到;单 worker 预算 = 内存上限 / worker 数。
   - 窗口占单 worker 预算的一半,另一半留给归档装载峰值与结果对象。
   - 扣除时间线与资金费常驻,按 `bytes_per_bar()` 反推 bar 数,再减去历史。
   - 至少 1 根;不小于数据长度时回退整段。
5. 哈希 v2:
   - 标签 `quant-backtest/data/v2`,按标的名称、总行数、固定页
     `PAGE_BARS = 65_536` 的页行数与页摘要汇总。
   - 页大小与运行时窗口无关;同一内容不同窗口、不同喂入顺序得到同一
     `data_hash`。
   - 资金费段标签升级为 `quant-backtest/funding/v2`;未启用时不追加。
   - 修改页大小、字段顺序或标签必须升级版本号。
6. 范围:
   - 首版 `run`、`validate`、`sweep` 支持分块;`walkforward` 保持整段。
   - `sweep` 每个 trial 独立流式读取,窗口不在 trial 间共享。
   - 归档按窗口请求覆盖的自然月,但 `load_window()` 会克隆完整时间线与资金费;
     CSV 每窗口仍整文件扫描。首版以时间换内存,不做稀疏索引。

## 变更文件

- `crates/backtest-core/src/model.rs`:`Universe.base_index`、`data_hash`、
  绝对下标 `bar()`/`try_bar()`、`window_start()`/`window_end()`、
  `estimated_data_bytes()`。
- `crates/backtest-core/src/engine/mod.rs`:`EngineState`、`advance()`、
  `finish()`、`run_backtest_stream()` 与流式/全量一致性单测。
- `crates/backtest-core/src/strategy.rs`:`Strategy::history_bars()` 默认实现。
- `crates/backtest-core/src/config.rs`:`RunConfig.chunk_bars`、解析校验与
  不参与 `config_hash` 的单测。
- `crates/backtest-data/src/hash.rs`:v2 `DataHasher`、固定页汇总与哈希单测。
- `crates/backtest-data/src/lib.rs`:`DataPlan`、`plan_universe_reporting()`、
  `load_window()`、`bytes_per_bar()`、CSV 范围扫描、v2 全量哈希。
- `crates/backtest-data/src/archive.rs`:分块规划与窗口读取适配。
- `crates/backtest-cli/src/stream.rs`:装载规划、窗口选择、流式回放与单测。
- `crates/backtest-cli/src/lib.rs`:`Loaded::{Full,Stream}`、`prepare_data()`、
  规划输出、`run`/`validate` 接入。
- `crates/backtest-cli/src/sweep.rs`:`Data::{Full,Stream}`、`run_single()` 接入。
- `crates/backtest-cli/tests/cli_contract.rs`:
  `chunked_run_and_sweep_match_full_load`。
- `docs/01-需求规格.md`、`docs/02-调研与决策.md`、`docs/03-流程设计.md`、
  `CHANGELOG.md`、`STEP.d/14-chunking.md`。

## 验证

- 单元与集成:窗口规划、自动阈值、回退、绝对下标、越界窗口、CSV 规划与
  全量哈希一致、流式 vs 全量逐字段一致。
- CLI 契约:`run` 与 `sweep` 的全量/分块 `result_hash`、`sweep_hash` 与
  `data_hash` 一致;分块运行时打印“分块每窗口”。
- BTCUSDT UM 1m 2023-06,43199 bar,`chunk_bars=5000`:
  `result_hash` 同为
  `f648cd3ec7735ca5e9646898b307411494ea69d30651fe1533c2370da8d6f3d2`,
  `data_hash` 同为
  `d9dc81d8395e1d61d0e5e5f65275a6aa1a15286ea5c1fc81081a212129262750`;
  2338 笔交易、89 次资金费结算与告警一致。
- BTC/ETH/BNB/SOL UM 1h 2024-01,744 bar,`chunk_bars=100`:
  `result_hash` 同为
  `54259a53f3eb610ba442ff721ee489bed09187fcf263322edd509a769058d286`,
  `data_hash` 同为
  `da7521b26452e214f431cdb90c619ba4efa2a8e36804bc07e16d5caa49d0a262`。
- 完整门禁、真实数据复跑与干净克隆结果见 [M4 验收](../../STEP.d/14-chunking.md)。

## 历史哈希重录

v2 上线后,历史 `data_hash`、`result_hash`、`sweep_hash` 全部重录;
`config_hash` 未变。重录值:

| 配置/场景 | 旧值 | v2 值 |
| --- | --- | --- |
| `config/00.smoke.toml` result | `144d1118...` | `efbb00b9f70690e5238630c65dea13922621fff36059ec26367fee66e067b977` |
| `config/01.risk-smoke.toml` result | `8476c228...` | `cd330ad9d98802e9d3492a61da4d24fcf5b17f1e51cba8b718d8a72e7a25979e` |
| `config/02.portfolio-smoke.toml` result | `8796ce7f...` | `d84b79855eae8f066b0ec6024673f8df4cffed2b7029ff9c119c84bb3ba8b1be` |
| `config/03.sweep.toml` sweep | `3e92bf60...` | `9fe1c058bc62427818af5a011f6a6c51bf3f10eb0b35f5e9220bf038fcb1eec` |
| `fixtures/sweep.toml` sweep | `6edce9d9...` | `41d11ac9f979517f45bd137b80ca41ffd16cf50b02081c92387c6b171e61bfab` |
| `fixtures/walkforward.toml` result | `0bee99bb...` | `4f0e7bd642c68cf434f25b2d567f1aeeba3f72909a031b87cd96c19d8e9f05c5` |
| `config/04.intraday.toml` result | `68707c00...` | `7c680a05ae883f02dd8b8a4428a13afc570c86a4d7f16db2badf5a8210b30f86` |
| `config/05.weekly.toml` result | `fee172be...` | `7c4c0da1569dea4018d393ef115f5765493e9c9b367148b0c6d2b5efcde9002f` |
| Binance 1h / CSV result | `5787aa02...` | `c64051cebda7292b225defc3f45aaf240dd1489c75ac25b963f03af206db0bb8` |
| Binance 1m result | `4c82b97a...` | `6450e36249989ac7fdd234d63f8cff12a55f2bae6fc25e0dc03fd6d9824dabc9` |
| 4 标的 1h result | `669ad911...` | `5d6569d70b6935f5930ccb8fdc6b593dbfb7fb2724a0f4e44c1cdc4a35fe3734` |
| Dukascopy EURUSD result | `8516fb78...` | `751b4c205dbc19e9d24b765fb242af7b30b7bedb3d93cd301663e4f730a81f75` |
| BTCUSDT UM 1h funding result | `695e40ef...` | `a8d5b41e452fa133c65c02dc4f92ebb09b071222017c5fa06b183a96fdf90438` |
| 4 标的 UM 1h funding result | `766b917f...` | `54259a53f3eb610ba442ff721ee489bed09187fcf263322edd509a769058d286` |

同一场景的数据哈希也按 v2 重录,完整值见
[M4 验收](../../STEP.d/14-chunking.md) 与各历史验收文档。

## 限制与回退

- 归档窗口仍克隆完整时间线与资金费;CSV 每窗口重扫;分块增加耗时。
- `sweep` 未共享窗口;`walkforward` 首版仍整段装载。
- 43k bar 样本的进程峰值 RSS 由二进制基线主导,不能证明内存收益;
  超大样本需在目标机实测。
- 回退:优先缩小 `run.start/run.end` 或改用更粗周期;仍不足时降低并行
  worker 数。撤销分块只需把 `chunk_bars` 设为 0 且不设内存上限,但
  `data_hash` 仍是 v2,不会恢复旧哈希。
