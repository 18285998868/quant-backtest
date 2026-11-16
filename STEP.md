# STEP 索引

本文件只做执行路径索引。阶段设计见 `docs/03-流程设计.md`，细节步骤见 `STEP.d/`。

## 入口

- 二进制:`quant-backtest`
- 配置:`run.toml`(见 `config/00.example.toml`)
- 归档数据源:`[source]` 段,与 `run.data` 二选一(见 [流程设计 §3.1](./docs/03-流程设计.md))
- 冒烟配置:`config/00.smoke.toml`
- 风控冒烟:`config/01.risk-smoke.toml`
- 组合冒烟:`config/02.portfolio-smoke.toml`
- 扫描冒烟:`config/03.sweep.toml`
- 小时线冒烟:`config/04.intraday.toml`
- 周线冒烟:`config/05.weekly.toml`
- 样本外冒烟:`fixtures/sweep.toml`
- walk-forward 冒烟:`fixtures/walkforward.toml`

## 命令

| 命令 | 作用 |
| --- | --- |
| `quant-backtest validate --config <run.toml>` | 校验配置与数据 |
| `quant-backtest run --config <run.toml>` | 单次回测,输出 JSON 与 HTML |
| `quant-backtest sweep --config <sweep.toml>` | 参数扫描 + 70/30 样本外,输出 `sweep.csv`、`sweep_summary.json`、`result.json`、`report.html` |
| `quant-backtest walkforward --config <walkforward.toml>` | 逐窗选参与 OOS 拼接,输出 `result.json` 与 `report.html` |
| `quant-backtest bench` | 合成负载吞吐基准 |
| `quant-backtest bench --scale` | 1/2/4/8 线程缩放基准 |

## 执行路径

```text
CLI
  -> 读取 TOML,校验并类型化
  -> 检测 CPU/内存,解析线程数
  -> 加载 CSV 或 data-reader 归档,校验 schema/排序/OHLC/时间线
  -> 构造策略
  -> 逐 bar: 开盘成交 -> 收盘估值 -> 策略信号 -> 下一根订单
  -> 指标/回撤/月度
  -> result.json + report.html

sweep
  -> 展开 [sweep.grid]
  -> 样本内 [0, split) 并行跑全部组合,只保留指标
  -> 按目标排序
  -> 重跑 top-K,写完整曲线
  -> 样本外 [split, len) 带预热重跑前 100 名
  -> 排名相关、DSR、边际效应、热力图
  -> 写 sweep.csv + sweep_summary.json + result.json + report.html

walkforward
  -> 按 [walk_forward] 切分 rolling/anchored 窗口
  -> 每窗训练集网格选参(可用预热)
  -> 测试集带预热重跑最优参数
  -> 测试段收益复利拼接
  -> 参数稳定性、DSR、窗口/空档/跳过统计
  -> 写 result.json + report.html
```

## 退出码

| 码 | 含义 |
| --- | --- |
| 0 | 成功 |
| 1 | 运行时/执行/报告错误 |
| 2 | 用法或配置错误 |
| 3 | 数据校验错误 |

## 复现

```bash
cargo test
cargo run --release -- validate --config config/00.smoke.toml
cargo run --release -- run --config config/00.smoke.toml
cargo run --release -- run --config config/01.risk-smoke.toml
cargo run --release -- run --config config/02.portfolio-smoke.toml
cargo run --release -- run --config config/04.intraday.toml
cargo run --release -- run --config config/05.weekly.toml
cargo run --release -- sweep --config config/03.sweep.toml
cargo run --release -- sweep --config fixtures/sweep.toml
cargo run --release -- walkforward --config fixtures/walkforward.toml
cargo run --release -- bench --scale
```

直连 Binance/Dukascopy 归档时,把配置中的 `run.data` 换成顶层 `[source]`;
Binance 期货默认结算资金费,可用 `source.funding = false` 关闭。
分钟级数据可设 `run.chunk_bars` 或 `run.memory_limit_gb` 按窗口装载,
命令不变;整段与分块结果哈希一致。
验收证据见 [M2 归档数据源](./STEP.d/12-archive-source.md)、
[M3 资金费](./STEP.d/13-funding.md)、
[M4 分块流式加载](./STEP.d/14-chunking.md) 与
[M5 walk-forward 分块](./STEP.d/15-walkforward-chunking.md)。

## 步骤

- [00 总览](./STEP.d/00-overview.md)
- [01 配置与数据](./STEP.d/01-config-data.md)
- [02 执行引擎](./STEP.d/02-engine.md)
- [03 指标与报告](./STEP.d/03-metrics-report.md)
- [04 CLI、系统检测与基准](./STEP.d/04-cli-system-bench.md)
- [05 S1 验收](./STEP.d/05-acceptance.md)
- [06 S2 风控验收](./STEP.d/06-s2-acceptance.md)
- [07 S3 组合验收](./STEP.d/07-s3-acceptance.md)
- [08 S4 扫描验收](./STEP.d/08-s4-acceptance.md)
- [09 S5 指标与报告验收](./STEP.d/09-s5-acceptance.md)
- [10 S6 健壮性与交付验收](./STEP.d/10-s6-acceptance.md)
- [11 M1 多周期验收](./STEP.d/11-m1-acceptance.md)
- [12 M2 归档数据源验收](./STEP.d/12-archive-source.md)
- [13 M3 资金费验收](./STEP.d/13-funding.md)
- [14 M4 分块流式加载验收](./STEP.d/14-chunking.md)
- [15 M5 walk-forward 分块验收](./STEP.d/15-walkforward-chunking.md)
