# quant-backtest

纯 Rust 的日线级、多标的、可做空策略回测引擎。

- 当前阶段:S4 参数扫描与缩放基准
- 设计基线:[docs/03-流程设计.md](./docs/03-流程设计.md)
- 执行路径:[STEP.md](./STEP.md)
- 变更记录:[CHANGELOG.md](./CHANGELOG.md)

## 构建

```bash
cargo build --release
```

## 运行

```bash
cargo run --release -- validate --config config/00.smoke.toml
cargo run --release -- run --config config/00.smoke.toml
cargo run --release -- run --config config/02.portfolio-smoke.toml
cargo run --release -- sweep --config config/03.sweep.toml
cargo run --release -- bench --scale
```

## 测试

```bash
cargo test
bash scripts/check.sh
```

## 文档

- [需求规格](./docs/01-需求规格.md)
- [调研与决策](./docs/02-调研与决策.md)
- [流程设计](./docs/03-流程设计.md)

## 许可

MIT OR Apache-2.0
