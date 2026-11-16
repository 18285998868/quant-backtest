# STEP 索引

本文件只做执行路径索引。阶段设计见 `docs/03-流程设计.md`，细节步骤见 `STEP.d/`。

## 入口

- 二进制:`quant-backtest`
- 配置:`run.toml`(见 `config/00.example.toml`)
- 冒烟配置:`config/00.smoke.toml`

## 命令

| 命令 | 作用 |
| --- | --- |
| `quant-backtest validate --config <run.toml>` | 校验配置与数据 |
| `quant-backtest run --config <run.toml>` | 单次回测,输出 JSON 与 HTML |
| `quant-backtest bench` | 合成负载吞吐基准 |
| `quant-backtest bench --scale` | 1/2/4/8 线程缩放基准 |

## 执行路径

```text
CLI
  -> 读取 TOML,校验并类型化
  -> 检测 CPU/内存,解析线程数
  -> 加载 CSV,校验 schema/排序/OHLC/时间线
  -> 构造策略
  -> 逐 bar: 开盘成交 -> 收盘估值 -> 策略信号 -> 下一根订单
  -> 指标/回撤/月度
  -> result.json + report.html
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
cargo run --release -- bench --scale
```

## 步骤

- [00 总览](./STEP.d/00-overview.md)
- [01 配置与数据](./STEP.d/01-config-data.md)
- [02 执行引擎](./STEP.d/02-engine.md)
- [03 指标与报告](./STEP.d/03-metrics-report.md)
- [04 CLI、系统检测与基准](./STEP.d/04-cli-system-bench.md)
- [05 S1 验收](./STEP.d/05-acceptance.md)
- [06 S2 风控验收](./STEP.d/06-s2-acceptance.md)
