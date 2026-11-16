# 009 walk-forward 分块流式

## 背景

M4 让 `run`、`validate` 与 `sweep` 支持按窗口装载,但 `walkforward` 仍在
命令入口整段 `load_universe()`,分钟级大样本做 walk-forward 时会重新遇到
内存上限。M4 已冻结窗口、引擎状态延续与 v2 哈希契约,walk-forward 只是
多了一层“训练选参 + OOS 测试”编排,可以复用同一 `sweep::Data` 路径。

## 决策

1. 入口:
   - `cmd_walkforward` 改为调用 `prepare_data`,与 `run`/`sweep` 共用
     `run.chunk_bars`、`memory_limit_gb` 自动分块与回退规则。
   - 规划时并行度取 worker 数,策略历史取网格最大 `warmup_bars()`;
     分块窗口预算与 `sweep` 相同。
   - 不新增 CLI 参数或配置字段。
2. 数据抽象:
   - `walkforward::execute` 接收 `sweep::Data`,不再接收 `&Universe`。
   - 窗口规划使用 `Data::len()`;窗口时间戳使用 `Data::timeline()`;
     `RunMeta.data_hash` 使用 `Data::data_hash()`。
   - `Data::Full` 与 `Data::Stream` 的 `len`/`timeline`/`data_hash` 来自
     `Universe` 或 `DataPlan`,两者在 M4 已保证同一内容同一哈希。
3. 训练与 OOS:
   - 训练网格继续调用 `sweep::run_grid`,自然继承 `Data::Full`/`Stream`。
   - OOS 最优参数回测从手工 `build` + `run_backtest` 改为
     `sweep::run_single`,与 `sweep` 的装载路径一致。
   - 每个训练窗口与测试窗口仍独立流式读取、独立重置账户;
     walk-forward 语义不变。
4. 兼容:
   - 报告 schema、字段、`config_hash`、`data_hash`、`result_hash` 均不变。
   - `walk_forward.elapsed_ms` 仍是非确定性运行时字段。
   - 分块仅改变峰值内存与耗时,不改变结果。

## 替代方案

- 只在训练阶段分块、OOS 仍整段:实现更少,但 OOS 净值与成交仍会保留
  整段数据,无法解决主要内存压力。
- 按 walk-forward 窗口整体装载(每窗一次 `load_window`):窗口内部不再切块,
  仍可能超过单 worker 预算;且与 `run`/`sweep` 的自动窗口规则不一致。
- 为 walkforward 单独复制一套流式编排:会产生第二套窗口与状态语义,
  违背 M4 的单一契约。

## 影响文件

- `crates/backtest-cli/src/walkforward.rs`:接收 `sweep::Data` 与 `Grid`,
  训练/OOS 统一走 `run_single`;新增整段/分块一致性单测。
- `crates/backtest-cli/src/sweep.rs`:`Data::timeline()`。
- `crates/backtest-cli/src/lib.rs`:`cmd_walkforward` 接入 `prepare_data`。
- `crates/backtest-cli/tests/cli_contract.rs`:
  `chunked_walkforward_matches_full_load`。
- `docs/01-需求规格.md`、`docs/02-调研与决策.md`、`docs/03-流程设计.md`、
  `CHANGELOG.md`、`STEP.md`、`STEP.d/15-walkforward-chunking.md`。

## 验证

- 夹具 `fixtures/s5_daily.csv`,180/60/60/1,`chunk_bars=50`:
  整段与分块 `result_hash` 同为
  `4f0e7bd642c68cf434f25b2d567f1aeeba3f72909a031b87cd96c19d8e9f05c5`,
  窗口参数、OOS 净值/指标、参数稳定性、DSR、跳过窗口与缺口一致。
- BTCUSDT UM 1m 2023-06,43199 bar,`chunk_bars=5000`:
  `result_hash` 同为
  `0fd37671cbea833727de97b29053b68d5539fe2553f5587e6154b642dd30b3fe`,
  `data_hash` 同为
  `d9dc81d8395e1d61d0e5e5f65275a6aa1a15286ea5c1fc81081a212129262750`。
- BTC/ETH/BNB/SOL UM 1h 2024-01,744 bar,`chunk_bars=100`:
  `result_hash` 同为
  `9981eeaf3272683229545c07c2607a4623638b7a68f1376a2a8f43bb51609818`,
  `data_hash` 同为
  `da7521b26452e214f431cdb90c619ba4efa2a8e36804bc07e16d5caa49d0a262`。
- 完整门禁、耗时/RSS 与干净克隆见
  [M5 验收](../../STEP.d/15-walkforward-chunking.md)。

## 限制与回退

- 每个训练组合和每个窗口独立读取,扫描次数约为“窗口数 × 组合数”;
  1m 样本实测分块比整段慢约 19 倍,以时间换内存。
- 43k bar 样本的进程 RSS 由二进制与归档装载峰值主导,分块不一定更低;
  内存收益需在更大样本上衡量。
- 归档窗口仍克隆完整时间线与资金费,CSV 每窗口仍整文件扫描。
- 回退:把 `chunk_bars` 设为 0 且不设内存上限,即恢复整段装载;
  也可减少网格组合或窗口数。哈希与报告不变。
