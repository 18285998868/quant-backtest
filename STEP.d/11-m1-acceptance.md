# 11 M1 多周期验收

## 目标

验证 [03 流程设计](../docs/03-流程设计.md) 的 T7.1–T7.8 与
[需求规格](../docs/01-需求规格.md) 的 AC18:小时、分钟与周线无需修改引擎即可运行,
年化因子与报告正确,扫描与 walk-forward 保持周期语义。

## 环境

- 构建机:CachyOS,i5-12400F(12 线程),32 GiB 内存。
- Rust 1.100.0,release 构建加 `--locked`。
- 小时夹具:`fixtures/intraday_1h.csv`,24 根合成小时线,周期 `1h`。
- 周线夹具:`fixtures/weekly.csv`,16 根合成周线,周期 `1w`。
- 冒烟配置:`config/04.intraday.toml`、`config/05.weekly.toml`。

## 用例

| 用例 | 证据 |
| --- | --- |
| T7.1 小时线 | `hourly_run_uses_hourly_annualization`:24 根 1h 数据运行成功,`meta.timeframe=1h`,`periods_per_year=8760`,2 笔交易、4 笔成交,报告显示“周期 1h” |
| T7.2 周线 | `weekly_run_uses_weekly_annualization`:16 根 1w 数据运行成功,`meta.timeframe=1w`,`periods_per_year=52`,16 个权益点,报告显示“周期 1w” |
| T7.3 年化因子 | `periods_per_year_follows_asset_class_and_timeframe` 覆盖 1m/5m/15m/30m/1h/4h/1w × 加密/美股/外汇,见下表 |
| T7.4 报告 | `writes_parseable_offline_json_and_html` 断言假设不再写死日线、月度说明为“复合 bar 收益”;1h 冒烟报告含“周期 1h” |
| T7.5 覆盖 | `periods_per_year_override_wins_over_timeframe`:1h 配置覆盖为 100 后,`RunConfig::periods_per_year()=100` |
| T7.6 非法周期 | `invalid_timeframe_exits_with_config_error`:`5x` 退出码 2,stderr 含 `timeframe`,无 panic |
| T7.7 扫描与 walk-forward | `intraday_sweep_and_walkforward_keep_timeframe`:1h 配置下两者 `result.json` 的 `meta.timeframe` 均为 `1h`,walk-forward 窗口非空 |
| T7.8 内存估算 | `estimated_data_bytes_counts_timeline_and_ohlcv` 与 `data_memory_warning_only_fires_above_limit`;CLI 输出“数据内存:约 1.1 KiB”,超限时向 stderr 告警 |

## 年化因子

| 周期 | 加密 | 美股 | 外汇 |
| --- | --- | --- | --- |
| 1m | 525600 | 98280 | 362880 |
| 5m | 105120 | 19656 | 72576 |
| 15m | 35040 | 6552 | 24192 |
| 30m | 17520 | 3276 | 12096 |
| 1h | 8760 | 1638 | 6048 |
| 4h | 2190 | 410 | 1512 |
| 1w | 52 | 52 | 52 |

推导公式为“资产类别年 bar 数 ÷ 周期数量”,四舍五入后至少为 1。组合内所有标的
推导值必须一致,否则配置报错;`periods_per_year` 可覆盖。

## 真实冒烟

```bash
target/release/quant-backtest run --config config/04.intraday.toml --threads 1 --out out/m1-1h
target/release/quant-backtest run --config config/05.weekly.toml --threads 1 --out out/m1-1w
```

| 配置 | 结果 |
| --- | --- |
| 1h | `result_hash=68707c00e452fd4606e39c44fefc9cacac64bcfd3f29e4dd9e0e601dc847f629` |
| 1w | `result_hash=fee172be94551b25f905713ca399a5798bea43a09360e07a2e6e827de3f937f3` |

1h 冒烟结果:总收益 1.98%,CAGR 175760.97%,最大回撤 1.92%,2 笔交易。
周线窗口短,CAGR 只用于验证公式,不代表策略有效性。

## 扫描与 walk-forward

在 24 根 1h 夹具上追加 2×2 网格与 3 窗口配置:

```toml
[sweep]
objective = "sharpe"
top_k = 2
oos_split = 0.7

[sweep.grid]
fast = [2, 3]
slow = [3, 4]

[walk_forward]
train_bars = 8
test_bars = 4
step_bars = 4
embargo_bars = 1
warmup_bars = 5
```

结果:

- 扫描 4 组,3 有效,1 组因 `fast >= slow` 跳过;`sweep_hash` 为
  `359ad35db505637b4c91aadced18cdde8a36f8b535838aaac44da98496e17385`。
- OOS 切分 0.7,切分下标 17,OOS 年化因子 8760,排名相关 1.0。
- walk-forward 2 个有效窗口、2 个跳过;跳过原因为首窗训练预热不足与尾窗超界。
- 窗口 0 train 4..12、test 13..17;窗口 1 train 8..16、test 17..21。
- 两窗最优参数均为 `fast=2, slow=3`;OOS 年化因子 8760。
- walk-forward `result_hash` 为
  `ca243a958171966f2bb16e0910e6591bd72090639dffb5617bd36527edd90492`。

## 内存估算

`Universe::estimated_data_bytes()` 按时间线 8 字节/bar 与每个标的 5 个 `f64`
列估算,不含 `Vec`、分配器和配置开销。24 根 1h 单标的显示约 1.1 KiB。
超过解析后的内存上限时 CLI 输出:

```text
警告: 数据常驻内存约 X,超过内存上限 Y;分钟级数据请缩小 run.start/run.end 或改用更粗周期
```

该告警只提示,不阻止运行;分块与按标的加载未实现。

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

- nextest:138 个测试,138 通过,0 跳过;比 S6 的 130 个增加 8 个。
- doctest:5 个 crate 均为 0 个文档测试,0 失败。
- fmt、check、clippy `-D warnings`、rustdoc `-D warnings`、release build 通过。
- `cargo deny check`:advisories、bans、licenses、sources 均为 ok。
- `cargo audit`:加载 1509 条公告,扫描 101 个依赖,无漏洞报告。
- `scripts/check.sh --ci` 退出码 0。

## 干净克隆

```bash
git clone /data/quant-backtest /tmp/m1-clean
cd /tmp/m1-clean
cargo test --workspace --locked
cargo build --release --locked
target/release/quant-backtest run --config config/04.intraday.toml --threads 1 --out /tmp/m1-clone-1h
target/release/quant-backtest run --config config/05.weekly.toml --threads 1 --out /tmp/m1-clone-1w
```

- 测试:138 个通过,0 失败;release 构建成功。
- 1h 克隆运行 `result_hash=68707c00e452fd4606e39c44fefc9cacac64bcfd3f29e4dd9e0e601dc847f629`。
- 1w 克隆运行 `result_hash=fee172be94551b25f905713ca399a5798bea43a09360e07a2e6e827de3f937f3`。
- 克隆运行的周期、年化因子与权益点数与工作副本一致。

## 回归

| 配置 | `result_hash` |
| --- | --- |
| `config/00.smoke.toml`,1 线程 | `d771891de94d0082f31f496a173f3645c31256c70c3b6a6d29d26b72f837af1b` |
| `config/00.smoke.toml`,2 线程 | 同上 |
| `config/04.intraday.toml` | `68707c00e452fd4606e39c44fefc9cacac64bcfd3f29e4dd9e0e601dc847f629` |
| `config/05.weekly.toml` | `fee172be94551b25f905713ca399a5798bea43a09360e07a2e6e827de3f937f3` |

S1 日线哈希与 S6 验收一致,说明周期解锁未改变日线语义与成交。

## 与设计基线的差异

- M1 只实现核心多周期:不做严格 bar 间隔校验、不建模交易时段/时区/日历,
  分钟级数据仍全量入内存。
- 年化因子沿用设计表;1w 对三类资产统一 52,不做 365/7 的 52.14 修正。
- 报告假设改为“周期由配置声明”,不再区分日线专用文案。

## 未运行项与限制

- 未用真实分钟数据或生产数据验收;1h/1w 均为合成夹具。
- 未跑 500 标的 × 10 年 1m 的 24 GB 内存场景;分块加载未实现。
- 未验证美股/外汇会话边界、半日市、时区与夏令时。
- 未跑多操作系统、MSRV、Miri 与 semver-checks。

## 退出条件

- T7.1–T7.8 通过。
- fmt/check/clippy/nextest/doctest/rustdoc/deny/audit/release/check 通过。
- 1h/1w 真实冒烟成功,日线回归哈希不变。
- 内存估算与告警、README、变更说明与验收文档齐全。
