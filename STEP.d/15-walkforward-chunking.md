# M5 walk-forward 分块验收

日期:2026-11-16

## 目标

验证 `walkforward` 与 `run`/`sweep` 共用 M4 分块契约:

- `run.chunk_bars` 显式分块,或 `memory_limit_gb` 超限时自动分块;
- 训练网格与 OOS 最优参数回测都只按窗口装载,不要求整段 `Universe`;
- 窗口规划、时间戳与 `data_hash` 从 `DataPlan`/`Universe` 统一读取;
- 整段与分块的窗口参数、OOS 净值与指标、参数稳定性、DSR、交易、
  资金费、告警、跳过窗口与缺口逐项一致;
- 不新增配置、CLI 参数、报告字段或哈希版本。

## 开关与范围

| 项 | 行为 |
| --- | --- |
| 入口 | `cmd_walkforward` 复用 `prepare_data`,规则与 `run`/`sweep` 相同 |
| `run.chunk_bars > 0` | 强制分块,窗口上限为该值;不小于数据长度时回退整段 |
| `run.chunk_bars = 0` 且 `memory_limit_gb > 0` | 全量估算超限才自动分块 |
| 训练 | `sweep::run_grid` 在训练区间内逐窗口装载 |
| OOS 测试 | 最优参数通过 `sweep::run_single` 在测试区间内逐窗口装载 |
| 窗口语义 | 训练与测试窗口仍独立重置账户;不跨窗口延续 |
| 数据元信息 | `Data::len()`/`timeline()`/`data_hash()` 来自 `Universe` 或 `DataPlan` |
| `config_hash` | 不含 `chunk_bars`;整段与分块相同 |
| 报告 | schema 1.2、字段与 `result_hash` 语义不变 |

## 验收结果

| 用例 | 结果 |
| --- | --- |
| T11.1 夹具整段/分块 | 通过:`result_hash`、3 个窗口参数、OOS 净值/指标、参数稳定性、DSR、1 个跳过窗口与缺口一致 |
| T11.2 单标的 1m | 通过:4 个窗口、1033 笔 OOS 交易、资金费汇总与全部哈希一致 |
| T11.3 多标的 1h | 通过:3 个窗口、86 笔交易、90 次 OOS 资金费结算与全部哈希一致 |
| T11.4 自动与回退 | 通过:共享 `choose_chunk()` 单测覆盖显式/自动/回退;显式超大窗口回退整段后哈希一致 |
| T11.5 报告与哈希 | 通过:JSON 字段、`walk_forward` 段与 schema 1.2 不变;整段/分块 `data_hash`、`result_hash` 相同 |

比对时逐一检查:

- `result_hash` 与 `meta.data_hash`;
- `walk_forward.windows`(含最优参数、IS/OOS 指标、窗口时间戳与告警);
- `walk_forward.oos_equity`、`oos_metrics`;
- `walk_forward.param_stability`、`skipped_windows`、`gaps`、`dsr`。

### T11.1 夹具

数据:`fixtures/s5_daily.csv`;训练 180 / 测试 60 / 步长 60 / embargo 1 /
warmup 40;4×4 网格;`chunk_bars=50`。

| 项 | 整段 | 分块 |
| --- | --- | --- |
| `config_hash` | `dcbc275d...` | `dcbc275d...` |
| `data_hash` | `3f0d2e5d...` | `3f0d2e5d...` |
| `result_hash` | `4f0e7bd6...` | `4f0e7bd6...` |
| 窗口 / 跳过 | 3 / 1 | 3 / 1 |
| OOS 收益 / Sharpe / MaxDD | 0.337320 / 7.739821 / 0.028678 | 相同 |
| 交易 / DSR | 1 / 0.994343 (N=48) | 相同 |
| 每窗最优参数 | `fast=3, slow=13` | 相同 |
| 耗时 / 峰值 RSS | 0.002 s / 13.9 MB | 0.010 s / 13.6 MB |

完整值:

- `data_hash = 3f0d2e5de1953b2b07645ded74313c82fce96e9d446919412d3fe7d15adb8ddf`
- `result_hash = 4f0e7bd642c68cf434f25b2d567f1aeeba3f72909a031b87cd96c19d8e9f05c5`

`result_hash` 与 M4 记录一致,说明接入分块没有改变 walk-forward 结果。

### T11.2 单标的 1m

数据:Binance UM `BTCUSDT` 1m,2023-06,43199 bar;训练 20000 / 测试 5000 /
步长 5000 / embargo 1 / warmup 30;2×2 网格;`chunk_bars=5000`。

| 项 | 整段 | 分块 |
| --- | --- | --- |
| `config_hash` | `d3bacc66...` | `d3bacc66...` |
| `data_hash` | `d9dc81d8...` | `d9dc81d8...` |
| `result_hash` | `0fd37671...` | `0fd37671...` |
| 窗口 / 跳过 | 4 / 1 | 4 / 1 |
| OOS 收益 / Sharpe / MaxDD | -0.733149 / -70.712910 / 0.734021 | 相同 |
| 交易 / DSR | 1033 / 0.000000 (N=16) | 相同 |
| 每窗最优参数 | `fast=6, slow=24` | 相同 |
| 资金费 | 区间 89 次,跨 4 个 OOS 窗口合计 42 次 | 相同 |
| 耗时 / 峰值 RSS | 0.139 s / 46.8 MB | 2.706 s / 56.8 MB |

完整值:

- `data_hash = d9dc81d8395e1d61d0e5e5f65275a6aa1a15286ea5c1fc81081a212129262750`
- `result_hash = 0fd37671cbea833727de97b29053b68d5539fe2553f5587e6154b642dd30b3fe`

分块慢约 19.5 倍,来自每个训练组合、每个窗口重复读取归档;
这是分块模式的预期代价。

### T11.3 多标的 1h

数据:Binance UM `BTC/ETH/BNB/SOL` 1h,2024-01,744 bar;训练 360 / 测试 120 /
步长 120 / embargo 1 / warmup 40;`lookback` 2 组网格;`chunk_bars=100`。

| 项 | 整段 | 分块 |
| --- | --- | --- |
| `config_hash` | `ff4e0c44...` | `ff4e0c44...` |
| `data_hash` | `da7521b2...` | `da7521b2...` |
| `result_hash` | `9981eeaf...` | `9981eeaf...` |
| 窗口 / 跳过 | 3 / 1 | 3 / 1 |
| OOS 收益 / Sharpe / MaxDD | 0.017554 / 1.321218 / 0.047659 | 相同 |
| 交易 / DSR | 86 / 0.343683 (N=6) | 相同 |
| 每窗最优参数 | `lookback=20` | 相同 |
| 资金费 | 区间 93 次,跨 3 个 OOS 窗口合计 90 次 | 相同 |
| 耗时 / 峰值 RSS | 0.030 s / 13.7 MB | 0.164 s / 13.5 MB |

完整值:

- `data_hash = da7521b26452e214f431cdb90c619ba4efa2a8e36804bc07e16d5caa49d0a262`
- `result_hash = 9981eeaf3272683229545c07c2607a4623638b7a68f1376a2a8f43bb51609818`

### T11.4 自动与回退

- `choose_chunk()` 单测覆盖:显式值优先;`chunk_bars >= 数据长度` 回退;
  内存上限未超时不自动分块;超限时按 worker 数缩小并扣除历史。
- `walkforward` 与 `run`/`sweep` 共用 `prepare_data`,没有独立分支。
- 显式 `chunk_bars=1000000`(大于 744)的 walkforward 输出没有
  “分块每窗口”,`result_hash` 与整段同为 `9981eeaf...`。
- 自动分块需要全量估算超过单 worker 预算;现有 1m/1h 样本均小于
  最低预算,因此自动路径由共享单测覆盖,不做真实数据触发。

### T11.5 报告与哈希

- 整段与分块 `result.json` 均为 schema 1.2,顶层字段相同;
  `walk_forward` 段除运行时 `elapsed_ms` 外逐字段一致。
- `report.html` 仍为离线 10 章,walk-forward 章节、窗口表与参数稳定性
  图正常生成。
- `data_hash`、`result_hash`、`config_hash` 与整段相同,未新增哈希段或
  版本号。
- `fixtures/walkforward.toml` 的 `result_hash` 保持
  `4f0e7bd6...`,与 M4 验收记录一致。

## 门禁

| 项 | 结果 |
| --- | --- |
| `cargo test --workspace --locked` | 174 个通过 |
| `cargo fmt --all --check` | 通过 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo nextest run --workspace --locked` | 174 个通过 |
| `cargo test --workspace --doc --locked` | 通过(0 个 doctest) |
| `RUSTDOCFLAGS=-D warnings cargo doc` | 通过 |
| `cargo deny check` | 通过 |
| `cargo audit` | 131 个依赖,无漏洞报告 |
| `cargo build --release --locked` | 通过 |
| `scripts/check.sh --ci` | 通过 |

## 干净克隆

提交 `1a47bb6` 克隆到 `/data/qb-m5-clean`(与 `/data/data-reader` 同级),
用于确认 path 依赖与分块 walk-forward 不依赖工作区残留:

- `cargo test --workspace --locked`:174 个通过;
- `cargo build --release --locked`:成功;
- `fixtures/walkforward.toml`、BTCUSDT 1m、4 标的 1h 的整段/分块
  `result_hash`、`data_hash`、窗口参数与 OOS 指标均与工作副本一致;
- 夹具 `result_hash = 4f0e7bd6...`、1m `0fd37671...`、
  4 标的 `9981eeaf...`,与工作副本相同。

克隆目录保留在 `/data/qb-m5-clean`,可用于后续检查。

## 限制

- 扫描次数约为“窗口数 × 网格组合数”;1m 样本分块比整段慢约 19.5 倍。
- 43k bar 样本的进程 RSS 由二进制与归档装载峰值主导,分块 RSS 反而
  略高(46.8 MB 对 56.8 MB);内存收益需在更大样本上衡量。
- 归档窗口仍克隆完整时间线与资金费;CSV 每窗口仍整文件扫描。
- 回退:把 `chunk_bars` 设为 0 且不设内存上限,恢复整段装载,哈希不变。
