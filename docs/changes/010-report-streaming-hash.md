# 010 报告流式哈希与写入

## 背景

M5 完成后,用 Binance spot BTCUSDT 1m 2023-05 至 2026-10 做 1843200 bar
大样本实测。整段与分块的 `result_hash`、交易与成交完全一致,但进程峰值
RSS 分别约 2088 MB 与 2000 MB;`validate` 只做数据规划时约 417 MB。
约 1.6 GB 的峰值来自报告阶段,而不是数据装载或回测引擎。

根因是 `content_hash()` 先把整个 `BacktestResult` 转成
`serde_json::Value` 树:

- 184 万个权益点各变成带 5 个键的对象;
- 再把整棵树按字典序写成约 100 MB 的紧凑 JSON 字节;
- `write_json()` 再构建一个约 13 MB 的 pretty 字符串。

`write_html()` 又重复计算一次 `content_hash()`。这些中间对象远大于结果
本身,在分钟级大样本上会把峰值推高一个数量级。

## 决策

1. 流式规范哈希:
   - 新增内部 `CanonicalResult` 与逐类型 `CanonicalItem` 实现;
   - 顶层对象、`meta`、`metrics`、`funding` 与各数组元素都按旧
     `serde_json::Value` 的字典序输出;
   - `meta.generated_at`、`meta.threads`、`meta.engine_version` 继续排除;
   - `funding = null` 继续省略,与 `skip_serializing_if` 一致;
   - 大数组逐元素写入 `BufWriter<HashWriter>`,哈希器直接消费字节,
     不再构建 `Value` 树或完整 JSON 字符串。
2. 流式 JSON 写入:
   - `write_json()` 改用 `File + BufWriter` 与
     `serde_json::to_writer_pretty()`;
   - 报告字段、键顺序、缩进与浮点格式保持不变,只是不再先拼接字符串。
3. 哈希复用:
   - `write_report()` 只计算一次 `result_hash`,同时传给 JSON 与 HTML;
   - 公开的 `write_json()`/`write_html()` 仍各自计算,保持独立调用语义。
4. 兼容:
   - `result_hash` 的输入字节、SHA-256 与十六进制输出完全不变;
   - 报告 schema 1.2、HTML 10 章、配置、CLI 与数据哈希均不变;
   - 新增单测保留旧 `to_value` 参考实现,逐字节比对规范输出。

## 替代方案

- 保留 `Value`,只把权益数组拆出来单独哈希:仍需为其余字段建树,且容易
  漏掉嵌套键序,兼容风险高于收益。
- 用 `preserve_order` 关闭字典序:会改变历史 `result_hash`,不可接受。
- 把大数组先序列化成 `RawValue` 再排序:需要为每个元素分配字符串,
  峰值仍随交易/权益数线性增长,只是常数更小。
- 直接按结构体声明顺序流式输出:实现最简单,但与旧字典序不同,所有历史
  `result_hash` 都会失效。

## 影响文件

- `crates/backtest-report/src/lib.rs`:规范序列化器、流式哈希、BufWriter
  写入、单次哈希复用与旧实现逐字节对比测试。
- `CHANGELOG.md`、`docs/01-需求规格.md`、`docs/02-调研与决策.md`、
  `docs/03-流程设计.md`、`STEP.md`、`STEP.d/16-report-streaming-hash.md`。

## 验证

大样本:Binance spot BTCUSDT 1m,2023-05-01 至 2026-10-31,1843200 bar,
`config_hash = fe6d6fcb...`,`data_hash = 36ec7667...`。

| 指标 | 改造前整段 | 改造后整段 | 改造前分块 | 改造后分块 |
| --- | --- | --- | --- | --- |
| 峰值 RSS | 2087.8 MB | 412.6 MB | 2000.1 MB | 412.8 MB |
| 耗时 | 3.633 s | 1.203 s | 4.848 s | 2.174 s |
| `result_hash` | `f7026895...` | `f7026895...` | `f7026895...` | `f7026895...` |
| 交易 / 成交 | 22097 / 44194 | 22097 / 44194 | 22097 / 44194 | 22097 / 44194 |

改造后的 `result.json` 除 `meta.generated_at` 外与改造前逐字段相同,
文件大小同为 12,739,787 字节。改造后整段与分块的报告阶段不再抬高
峰值;剩余约 413 MB 由数据规划阶段的归档整段装载主导。

历史冒烟哈希保持不变:

- `config/00.smoke.toml`: `efbb00b9...`
- `config/01.risk-smoke.toml`: `cd330ad9...`
- `config/02.portfolio-smoke.toml`: `d84b7985...`
- `config/04.intraday.toml`: `7c680a05...`
- `config/05.weekly.toml`: `7c4c0da1...`
- BTCUSDT UM 1m 2023-06: `result_hash = f648cd3e...`,`data_hash = d9dc81d8...`
- BTC/ETH/BNB/SOL UM 1h 2024-01: `result_hash = 54259a53...`,
  `data_hash = da7521b2...`
- `fixtures/walkforward.toml`: `4f0e7bd6...`
- `fixtures/sweep.toml`: `sweep_hash = 41d11ac9...`

完整门禁、干净克隆与验收细节见
[M6 验收](../../STEP.d/16-report-streaming-hash.md)。

## 限制与回退

- 规范序列化器按当前 `BacktestResult` 字段显式列出键;新增结果字段时必须
  同步更新,并由旧实现对比测试和固定哈希冒烟兜底。
- 剩余峰值来自 `plan_archive()` 按标的调用 `load_universe_reporting()`,
  即 `data-reader` 整段表、`RawBar` 与 `Universe` 共存。要降到与窗口
  同阶,需要数据读取层提供列投影或流式迭代,留作后续阶段。
- 回退:恢复 `content_hash()` 的 `to_value` 实现即可;报告 schema 与
  对外接口不变,但大样本峰值会回到约 2 GB。
