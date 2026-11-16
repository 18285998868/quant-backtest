# M6 报告流式哈希与写入验收

日期:2026-11-16

## 目标

验证报告阶段不再构建完整结果树与整段 JSON 字符串:

- `content_hash` 按旧 `serde_json::Value` 的字典序流式写入 SHA-256;
- `write_json` 用 `File + BufWriter` 与 `to_writer_pretty` 写出;
- `write_report` 只计算一次 `result_hash`,JSON 与 HTML 复用;
- 历史 `result_hash`、报告 schema 1.2 与 JSON 字段/键序/缩进不变;
- 1843200 bar 样本的报告阶段峰值不再主导进程 RSS。

## 范围与兼容

| 项 | 行为 |
| --- | --- |
| 规范顺序 | 顶层、`meta`、`metrics`、`funding` 与数组元素均按旧 `Value` 的字典序输出 |
| 排除字段 | `meta.generated_at`、`meta.threads`、`meta.engine_version` 不参与哈希 |
| `funding` | `None` 时省略,与 `skip_serializing_if` 一致 |
| 大数组 | 逐元素写入哈希器,不构建 `Value` 树或完整 JSON 字节 |
| JSON 写入 | `File + BufWriter` 配 `to_writer_pretty`;字段、键序、缩进与浮点格式不变 |
| 哈希复用 | `write_report` 只算一次;独立 `write_json`/`write_html` 各自计算 |
| 配置/CLI | 无新增参数或字段;`config_hash`、`data_hash` 不变 |
| 报告 | schema 1.2、HTML 10 章、字段与浮点格式不变 |

## 验收结果

| 用例 | 结果 |
| --- | --- |
| T12.1 旧实现一致 | 通过:规范流式字节与旧 `to_value` 参考实现逐字节相同,`content_hash` 与旧实现 SHA-256 相同 |
| T12.2 运行时字段 | 通过:修改 `generated_at`、`threads`、`engine_version` 后哈希不变 |
| T12.3 报告结构与写入 | 通过:`result.json` 除 `generated_at` 外与改造前逐字段相同,文件大小相同;schema 1.2 与 HTML 10 章不变 |
| T12.4 大样本峰值 | 通过:1843200 bar 整段/分块 `result_hash` 一致,峰值 RSS 从约 2 GB 降到约 413 MB |
| T12.5 历史哈希 | 通过:S1/S2/S3/1h/1w、walk-forward 与 sweep 冒烟哈希不变 |

### T12.1 旧实现字节一致

单测 `canonical_stream_matches_legacy_value_bytes` 覆盖:

- 样本结果同时包含一笔成交与资金费汇总;
- `CanonicalResult` 的流式输出与
  `serde_json::to_value` → 删除运行时字段 → `to_vec` 逐字节相同;
- `content_hash` 等于旧字节的 SHA-256;
- `funding = None` 时仍逐字节相同,确认省略规则一致。

### T12.2 运行时字段

`content_hash_ignores_runtime_fields` 修改生成时间、线程数与引擎版本,
哈希保持不变;规范序列化器不输出这三个字段。

### T12.3 报告结构与写入

- 大样本改造前后 `result.json` 除 `meta.generated_at` 外逐字段相同,
  文件大小同为 12,739,787 字节。
- `schema_version` 仍为 1.2;`report_kind`、`assumptions`、`warnings`、
  `funding`、`sweep`、`walk_forward` 等字段与顺序不变。
- `report.html` 仍为离线 10 章,资金费与 walk-forward 章节正常生成。
- `write_report` 只计算一次哈希;生成 JSON 与 HTML 使用同一 `result_hash`。

### T12.4 大样本

数据:Binance spot `BTCUSDT` 1m,2023-05-01 至 2026-10-31,1843200 bar;
整段与分块的唯一差异是 `chunk_bars`。

| 项 | 改造前整段 | 改造后整段 | 改造前分块 | 改造后分块 |
| --- | --- | --- | --- | --- |
| 耗时 | 3.633 s | 1.203 s | 4.848 s | 2.174 s |
| 峰值 RSS | 2087.8 MB | 412.6 MB | 2000.1 MB | 412.8 MB |
| `result_hash` | `f7026895...` | `f7026895...` | `f7026895...` | `f7026895...` |
| 交易 / 成交 | 22097 / 44194 | 22097 / 44194 | 22097 / 44194 | 22097 / 44194 |

完整值:

- `config_hash = fe6d6fcb40fbc6a646b0a351076c2f00e22eedff5b302ebb35f7dfcd7d860e47`
- `data_hash = 36ec7667768a435cfbbca1d1ef8c1bbe48a109b3b980d6927a84fe6edcefd72a`
- `result_hash = f702689588d2a9102ccb713d645af1a9c91da9d03c3d15507330daf5788135a4`

改造后整段与分块的报告阶段不再抬高进程峰值;剩余约 413 MB 由数据规划
阶段的归档整段装载主导。

### T12.5 历史哈希

| 用例 | 结果 |
| --- | --- |
| `config/00.smoke.toml` | `result_hash = efbb00b9...` |
| `config/01.risk-smoke.toml` | `result_hash = cd330ad9...` |
| `config/02.portfolio-smoke.toml` | `result_hash = d84b7985...` |
| `config/04.intraday.toml` | `result_hash = 7c680a05...` |
| `config/05.weekly.toml` | `result_hash = 7c4c0da1...` |
| BTCUSDT UM 1m 2023-06,43199 bar | 整段/分块 `result_hash = f648cd3e...`、`data_hash = d9dc81d8...` |
| BTC/ETH/BNB/SOL UM 1h 2024-01,744 bar | 整段/分块 `result_hash = 54259a53...`、`data_hash = da7521b2...` |
| `fixtures/walkforward.toml` | `result_hash = 4f0e7bd6...` |
| `fixtures/sweep.toml` | `sweep_hash = 41d11ac9...` |

BTCUSDT 1m 与 4 标的 1h 用 M6 二进制复跑整段/分块,结果与 M4 记录一致;
walk-forward 与 sweep 冒烟与 M5 记录一致,说明报告流式化没有改变
`result_hash`、`data_hash` 或 `sweep_hash`。旧的 `result.json` 文件大小与
字段级比较见 T12.3。

## 门禁

| 项 | 结果 |
| --- | --- |
| `cargo test --workspace --locked` | 175 个通过 |
| `cargo fmt --all --check` | 通过 |
| `cargo check --workspace --all-targets --locked` | 通过 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| `cargo nextest run --workspace --locked` | 175 个通过 |
| `cargo test --workspace --doc --locked` | 通过(0 个 doctest) |
| `RUSTDOCFLAGS=-D warnings cargo doc` | 通过 |
| `cargo deny check` | 通过 |
| `cargo audit` | 131 个依赖,无漏洞报告 |
| `cargo build --release --locked` | 通过 |
| `scripts/check.sh --ci` | 通过 |

## 干净克隆

提交 `待补` 克隆到 `data-reader` 同级的 `qb-m6-clean`,
用于确认 path 依赖与流式报告不依赖工作区残留:

- `cargo test --workspace --locked`:175 个通过;
- `cargo build --release --locked`:成功;
- 1843200 bar 整段/分块复跑:`result_hash`、`data_hash` 与工作副本一致;
- `fixtures/walkforward.toml` 与 `fixtures/sweep.toml` 的哈希与工作副本一致。

克隆目录保留为 `qb-m6-clean`,可用于后续检查。

## 限制

- 规范序列化器按当前字段手工维护;新增 `BacktestResult` 字段时必须同步
  更新,否则哈希会漏字段。对比测试与固定冒烟哈希用于兜底。
- 剩余约 413 MB 峰值来自 `plan_archive()` 的整段 `load_universe_reporting()`;
  要降到与窗口同阶,需要数据读取层提供列投影或流式迭代。
- `result.json` 的 `equity` 仍按下采样输出,`result_hash` 覆盖完整权益序列;
  两者语义不同,比较报告时需区分。
- 回退:恢复 `content_hash()` 的 `to_value` 实现即可;报告 schema 与
  对外接口不变,但大样本峰值会回到约 2 GB。
