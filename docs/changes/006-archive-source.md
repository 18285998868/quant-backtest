# 006 归档数据源直连

## 背景

[需求规格](../01-需求规格.md) 的 S16/AC20 要求直接读取 `data-reader` 的
Binance/Dukascopy 归档,不再强制先导出 CSV。此前的加载器只接受规范 CSV,
数据准备需要额外一步,且容易在导出时改坏时间戳。

## 决策

1. 依赖方式:同级仓库 path 依赖,不复制解析代码,也不锁 git rev。
   - `binance-core` 与 `dukascopy-core` 均为 `version = "0.2.0"` + path,
     满足 `cargo deny` 的 wildcard 检查。
   - 两个仓库必须位于同一父目录;这是构建期约束。
2. 配置:新增顶层 `[source]`,与 `run.data` 二选一。
   - Binance:`kind/root/market`,market 为 `spot`、`futures/um`、`futures/cm`。
   - Dukascopy:`kind/root`,可选 `price_type = "bid" | "ask"`,默认 `bid`。
   - 周期在配置阶段按 provider 支持矩阵校验,避免运行到一半才报错。
3. 时间戳:归档按开盘时刻存储,回测契约要求收盘时刻。
   - Binance:`ts = close_time + 1`。
   - Dukascopy:`ts = open_time + timeframe`。
4. 日期范围:请求归档时把起点前移一个 `timeframe`,再按 `[start, end)`
   精确过滤,避免跨日/跨周 bar 被文件名日期切掉。
5. 缺失数据:归档缺失文件只进入诊断,不填补;单标的在请求区间内无 bar
   仍按 `EmptyRange` 报错。多标的时间线一致性要求不变。
6. 哈希:`config_hash` 不纳入数据源路径,`data_hash` 覆盖实际 bar 内容;
   同一内容 CSV 与归档得到相同哈希。

## 变更文件

- `crates/backtest-core/src/config.rs`:新增 `DataInput`、`SourceConfig` 与
  provider 配置,`RunConfig.data_path` 改为 `RunConfig.data`。
- `crates/backtest-core/src/error.rs`:新增归档、空归档与时间边界错误。
- `crates/backtest-data/src/lib.rs`:拆分 CSV 读取与统一 `finalize`,
  新增 `LoadSource`/`LoadDiagnostics`/`load_universe_reporting`。
- `crates/backtest-data/src/archive.rs`:Binance/Dukascopy 读取、时间戳转换、
  日期窗口与诊断信息。
- `crates/backtest-data/Cargo.toml`:path 依赖与版本约束。
- `crates/backtest-cli/src/lib.rs`:按 `config.data` 分派并打印 `数据源:` 摘要。
- `crates/backtest-cli/src/sweep.rs`、`walkforward.rs`:测试 helper 适配。
- `crates/backtest-data/tests/missing_bar_fill.rs`:适配新入口。
- `deny.toml`:允许 `Zlib`。
- `README.md`、`THIRD_PARTY.md`、`STEP.md`、`docs/01-需求规格.md`、
  `docs/02-调研与决策.md`、`docs/03-流程设计.md`、`CHANGELOG.md`。

## 验证

- 151 个测试、fmt、check、clippy、nextest、doctest、rustdoc、deny、audit
  与 release 构建全部通过。
- Binance BTCUSDT 1h(2024-01)与等价 CSV:`data_hash` 同为
  `b527ca0a7b3ff8a20510d92a55d1fc8af6c268f8d4cbbfa628020fef51885ffd`,
  `result_hash` 同为
  `c64051cebda7292b225defc3f45aaf240dd1489c75ac25b963f03af206db0bb8`。
- Binance 1m(2023-06)43200 根 bar;4 标的 1h 组合 744 根 bar。
- Dukascopy EURUSD H1(2023-07)743 根 bar;缺失的上月归档文件进入诊断,
  不静默补齐。
- 1/2 线程、重复运行 `result_hash` 一致。
- 提交 `22979ff` 的干净克隆(与 `data-reader` 同级)通过
  `cargo test --workspace --locked` 与 `cargo build --release --locked`。
- 详细证据见 [M2 验收](../../STEP.d/12-archive-source.md)。

## 限制

- 只支持 Binance klines 与 Dukascopy `1m/1h/1d`;不做周期合成、复权或时区转换。
- 上游 crate 版本按 path 跟随 `data-reader` HEAD;上游 breaking change 需要
  同步升级 `version` 与适配代码。
- 分钟级数据仍全量载入内存,分块加载未实现。
