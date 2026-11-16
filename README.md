# quant-backtest

纯 Rust 的多周期、多标的、可做空策略回测引擎,支持直接读取 Binance/Dukascopy 归档,参数扫描与 walk-forward。

- 当前阶段:M6 报告流式哈希与写入
- 设计基线:[docs/03-流程设计.md](./docs/03-流程设计.md)
- 执行路径:[STEP.md](./STEP.md)
- 变更记录:[CHANGELOG.md](./CHANGELOG.md)

## 环境

- Linux x86-64
- Rust stable,最低版本见 [Cargo.toml](./Cargo.toml) 的 `rust-version`
- 数据源二选一:规范 CSV,或与 `data-reader` 同级克隆后直连归档
- 可选:`cargo-nextest`、`cargo-deny`、`cargo-audit` 用于完整门禁

归档直连通过 path 依赖引入 `data-reader` 的 crate,要求两个仓库位于同一父目录:

```text
workspace/
├── data-reader/
└── quant-backtest/
```

## 构建

```bash
cargo build --release --locked
```

## 快速开始

```bash
cargo run --release --locked -- validate --config config/00.smoke.toml
cargo run --release --locked -- run --config config/00.smoke.toml
cargo run --release --locked -- run --config config/02.portfolio-smoke.toml
cargo run --release --locked -- run --config config/04.intraday.toml
cargo run --release --locked -- run --config config/05.weekly.toml
cargo run --release --locked -- sweep --config fixtures/sweep.toml
cargo run --release --locked -- walkforward --config fixtures/walkforward.toml
```

`run` 输出 `result.json` 与 `report.html`;`sweep` 另输出 `sweep.csv` 与
`sweep_summary.json`;`walkforward` 输出逐窗 OOS 结果。报告为单文件离线 HTML,
固定 10 章,不依赖 CDN 或脚本。

直连归档时把 `run.data` 换成顶层 `[source]`(Binance/Dukascopy),配置与
时间戳语义见 [流程设计 §3.1 与 §3.7](./docs/03-流程设计.md)。

Binance `futures/um`/`futures/cm` 默认按归档资金费结算,结果与报告单列
资金费成本;`spot` 与 Dukascopy 默认关闭。需要关闭期货资金费时设置
`source.funding = false`;`spot` 或 Dukascopy 设 `funding = true` 会报配置错误。

分钟级大样本可设置 `run.chunk_bars`(正整数)强制按窗口装载,或设置
`run.memory_limit_gb > 0` 让全量估算超限时自动分块。窗口按绝对下标回放,
策略历史只读、引擎状态延续;`run`、`sweep` 与 `walkforward` 的整段/分块
`data_hash`、`result_hash` 与 `sweep_hash` 一致。契约见
[流程设计 §3.10](./docs/03-流程设计.md),验收见
[M4 分块流式加载](./STEP.d/14-chunking.md)、
[M5 walk-forward 分块](./STEP.d/15-walkforward-chunking.md) 与
[M6 报告流式哈希与写入](./STEP.d/16-report-streaming-hash.md)。

报告阶段也采用流式处理:`result_hash` 直接按规范键序写入 SHA-256,
`result.json` 用 `BufWriter` 输出,不再构建整棵 `serde_json::Value`。
1843200 bar 样本的报告阶段峰值从约 2 GB 降到约 413 MB,历史哈希不变。

## 测试与门禁

```bash
cargo test --workspace --locked
bash scripts/check.sh --ci
```

完整门禁包含 fmt、check、clippy、测试、doctest、rustdoc、deny、audit 与
release 构建;`cargo audit` 需要访问 RustSec 公告库。

## 文档

- [需求规格](./docs/01-需求规格.md)
- [调研与决策](./docs/02-调研与决策.md)
- [流程设计](./docs/03-流程设计.md)
- [S6 验收](./STEP.d/10-s6-acceptance.md)
- [M1 多周期验收](./STEP.d/11-m1-acceptance.md)
- [M2 归档数据源验收](./STEP.d/12-archive-source.md)
- [M3 资金费验收](./STEP.d/13-funding.md)
- [M4 分块流式加载验收](./STEP.d/14-chunking.md)
- [M5 walk-forward 分块验收](./STEP.d/15-walkforward-chunking.md)
- [M6 报告流式哈希与写入验收](./STEP.d/16-report-streaming-hash.md)

## 许可

MIT OR Apache-2.0。见 [LICENSE-MIT](./LICENSE-MIT)、
[LICENSE-APACHE](./LICENSE-APACHE) 与 [THIRD_PARTY.md](./THIRD_PARTY.md)。
