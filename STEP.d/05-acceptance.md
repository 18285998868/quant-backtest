# 05 S1 验收

## 目标

逐条验证 `docs/03-流程设计.md` 的 T1.1–T1.8,并完成门禁与真实冒烟。

## 用例

| 用例 | 证据 |
| --- | --- |
| T1.1 手算多头 | 单测断言 qty、cash、equity |
| T1.2 次日成交 | 单测断言成交价 90 |
| T1.3 缺失 bar | 数据+引擎集成测试:CSV 中 t6 缺失,断言 t7 开盘成交;单测覆盖 volume=0 顺延 |
| T1.4 成本 | 单测断言 PnL 989.5 |
| T1.5 确定性 | 两次运行内容哈希一致 |
| T1.6 吞吐 | `bench` 输出 >= 1e6 bar 步/秒(2 核) |
| T1.7 自动线程 | 单测 + `bench` 输出 |
| T1.8 周期解析 | 单测覆盖 1d/1h/1w/5x |

## 门禁

```bash
bash scripts/check.sh
cargo build --release
cargo run --release -- validate --config config/00.smoke.toml
cargo run --release -- run --config config/00.smoke.toml
```

## 真实冒烟

- 从空 `target/smoke-s1` 开始。
- release 二进制退出码 0。
- 检查 `result.json` 与 `report.html` 存在、JSON 可解析、HTML 无外链。
- 保留产物再跑一次,退出码仍为 0。

## 验收结果(2026-11-16)

```text
cargo fmt --all -- --check                                 通过
cargo test --workspace --all-targets                       44 passed
cargo clippy --workspace --all-targets --all-features      通过(-D warnings)
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps 通过
cargo build --release                                      通过
bash scripts/check.sh --ci                                 通过(deny/audit ok)
cargo run --release -- validate --config config/00.smoke.toml  通过
cargo run --release -- run --config config/00.smoke.toml       退出码 0
```

冒烟产物:`target/smoke-s1/result.json`、`target/smoke-s1/report.html`;JSON 可解析,HTML 无外链。二次运行 `result_hash` 一致:`efbb00b9f70690e5238630c65dea13922621fff36059ec26367fee66e067b977`。

吞吐基准(2 线程,门槛 1e6):空策略 50,175,123 bar 步/秒,双均线 36,376,679 bar 步/秒。

## 退出条件

- T1.1–T1.8 通过。
- fmt/check/clippy/test/doc 通过。
- 冒烟通过。
- 汇报已运行与未运行项。
