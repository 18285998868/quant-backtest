# 10 S6 健壮性与交付验收

## 目标

验证 [03 流程设计](../docs/03-流程设计.md) 的 T6.1–T6.6:非法数据与非法配置的退出码、
walk-forward 极端参数、线程与内存边界、干净克隆复现、安全检查与 README 可执行性。

## 环境

- 构建机:CachyOS,i5-12400F(12 线程),32 GiB 内存。
- Rust 1.100.0,release 构建加 `--locked`。
- 冒烟数据:`config/00.smoke.toml`、`config/02.portfolio-smoke.toml`、
  `fixtures/sweep.toml`、`fixtures/walkforward.toml`。
- 依赖审计在线拉取 RustSec 公告库,共 1509 条公告。

## 用例

| 用例 | 证据 |
| --- | --- |
| T6.1 非法数据 | `invalid_data_fixtures_exit_with_data_error` 覆盖空文件、乱序、重复时间戳、负价、NaN、Inf、负成交量、缺列、非法时间戳、空 symbol 与缺失文件;全部退出码 3,stderr 含对应错误上下文,无 `panicked` |
| T6.2 全新克隆 | 在新临时目录 `git clone` 本仓库后,`cargo test --workspace --locked`、`cargo build --release --locked` 与 README 的 validate/run/portfolio/sweep/walkforward 全部退出码 0 |
| T6.3 复现 | `result_hash_is_stable_across_threads`:同配置 1/2 线程 `result_hash` 相同而 `meta.threads` 分别为 1 与 2;S1–S5 冒烟哈希与 S5 验收记录一致 |
| T6.4 安全 | 密钥与私钥模式扫描无命中;绝对路径扫描仅命中源码中的数据哈希域串;`cargo deny` 四类检查 ok;`cargo audit` 扫描 101 个依赖无漏洞报告 |
| T6.5 文档 | README 命令在干净克隆中逐条执行成功;README、STEP、验收文档的相对链接均指向真实文件 |
| T6.6 非法配置 | `invalid_config_fields_exit_with_config_error` 覆盖 `run.threads`、`run.memory_limit_gb`、未知字段与 5 类 walk-forward 越界;`cli_overrides_reject_invalid_threads_and_memory` 覆盖 CLI `--threads 0`、负内存与小于单 worker 预算;均退出码 2、含字段名、不 panic |

非法数据夹具位于 `fixtures/invalid/`。其中 `zero_volume.csv` 是合法输入,由
`zero_volume_is_a_valid_bar` 验证零成交量不报错;其余 10 个文件用于 T6.1。

## 健壮性加固

- walk-forward 的 `train_bars + embargo_bars`、窗口下标与最小数据长度改用 checked 算术;
  溢出返回 `ConfigError::Invalid`,字段为 `walk_forward`,消息含“溢出”。
- `plan_windows` 拒绝 `train_bars=0`、`test_bars=0` 与 `step_bars < test_bars`,
  不再依赖调用方预校验。
- `memory_limit_gb` 在配置层与 `resolve_workers` 双重校验:负数、NaN、Inf 均为配置错误;
  显式上限超过检测到的可用内存时写入 warning,但不静默修改用户值。
- `--threads` 超过检测值时保留显式值并告警,与设计契约一致。

## 门禁

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked --no-tests=warn
cargo test --workspace --doc --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features --locked
cargo deny check
cargo audit
cargo build --release --locked
bash scripts/check.sh --ci
```

验收结果:

- nextest:130 个测试,130 通过,0 跳过;比 S5 的 117 个增加 13 个。
- doctest:5 个 crate 均为 0 个文档测试,0 失败。
- fmt、check、clippy `-D warnings`、rustdoc `-D warnings`、release build 通过。
- `cargo deny check`:advisories、bans、licenses、sources 均为 ok;
  仅有无匹配许可证白名单的 `license-not-encountered` 告警与传递依赖重复告警。
- `cargo audit`:加载 1509 条公告,扫描 101 个依赖,无漏洞报告。
- `scripts/check.sh --ci` 退出码 0;脚本按设计跳过 bench 目标与 MSRV。

## 真实冒烟

```bash
target/release/quant-backtest validate --config config/00.smoke.toml
target/release/quant-backtest run --config config/00.smoke.toml --threads 1 --out out/s6-smoke-1t
target/release/quant-backtest run --config config/00.smoke.toml --threads 2 --out out/s6-smoke-2t
target/release/quant-backtest run --config config/01.risk-smoke.toml --threads 1 --out out/s6-risk
target/release/quant-backtest run --config config/02.portfolio-smoke.toml --threads 1 --out out/s6-portfolio
```

| 配置 | 结果 |
| --- | --- |
| `config/00.smoke.toml`,1 线程 | `result_hash=d771891de94d0082f31f496a173f3645c31256c70c3b6a6d29d26b72f837af1b` |
| `config/00.smoke.toml`,2 线程 | 同上,`meta.threads=2` |
| `config/01.risk-smoke.toml` | `result_hash=00c15e27a2e05e4ecaa5e9729c01639d153d7152c1aa59f71485d7b96151d802` |
| `config/02.portfolio-smoke.toml` | `result_hash=6375664e77d7f0e03dc85d51a7879d3462701fcef9aaf478c0b0fa77817995e0` |

以上哈希与 S5 验收的 S1–S3 回归表一致,数据、信号与成交语义未变。

## 干净克隆

在新临时目录执行:

```bash
git clone <repo> quant-backtest-clean
cd quant-backtest-clean
cargo test --workspace --locked
cargo build --release --locked
target/release/quant-backtest validate --config config/00.smoke.toml
target/release/quant-backtest run --config config/00.smoke.toml
target/release/quant-backtest run --config config/02.portfolio-smoke.toml
target/release/quant-backtest sweep --config fixtures/sweep.toml
target/release/quant-backtest walkforward --config fixtures/walkforward.toml
```

- 测试与 release 构建退出码 0。
- validate、run、portfolio、sweep、walkforward 全部退出码 0。
- 干净克隆的结果哈希:
  run `d771891d...`、portfolio `6375664e...`、
  sweep `6edce9d9...`、walkforward `0bee99bb...`,与 S5 验收记录一致。
- 克隆目录不含 `target/` 与 `out/`,构建与冒烟从零生成。

## 安全与交付

- `rg` 密钥模式(password、secret、api key、private key、token)无命中。
- 绝对路径模式无个人路径命中;唯一匹配是源码中的数据哈希域串,不是文件系统路径。
- 无邮箱、私钥块或构建机用户名写入仓库。
- 新增 [LICENSE-MIT](../LICENSE-MIT)、[LICENSE-APACHE](../LICENSE-APACHE) 与
  [THIRD_PARTY.md](../THIRD_PARTY.md);`Cargo.toml` 声明 `MIT OR Apache-2.0`。
- README 提供环境、构建、快速开始、测试门禁、文档与许可入口。

## 与设计基线的差异

- T6.1 的设计表写“Data/Config 错误”;缺列与 CSV 解析失败在实现中统一归为
  `DataError::Csv`,退出码 3,与数据错误语义一致。
- 非法配置表把 walk-forward 的 5 类越界都归入 `ConfigError::Invalid`,
  字段名保留 `walk_forward.*`,便于定位。
- T6.4 的绝对路径检查排除 `target/`、`out/` 与 `Cargo.lock`,避免构建产物干扰。

## 未运行项与限制

- 未跑多操作系统矩阵;干净克隆与冒烟只在 CachyOS x86-64 验证。
- `cargo audit` 需要网络;离线环境只能跑 `cargo deny` 的本地许可证与 bans 检查。
- 未重跑 `bench --scale`;S6 未改引擎热路径,性能基线沿用 S4/S5。
- 未跑 MSRV、Miri 与 semver-checks;项目未启用 bench target。
- 仓库没有配置 git remote,本阶段只做本地提交,无法推送。

## 退出条件

- T6.1–T6.6 通过。
- fmt/check/clippy/nextest/doctest/rustdoc/deny/audit/release/check 通过。
- 非法数据与非法配置退出码符合契约,无 panic。
- 干净克隆构建、测试与全部示例成功,哈希可复现。
- 安全工作区清洁,许可与第三方声明齐全。
