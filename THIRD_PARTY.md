# 第三方依赖与许可

本项目以 MIT 或 Apache-2.0 双许可发布,见 [LICENSE-MIT](./LICENSE-MIT) 与
[LICENSE-APACHE](./LICENSE-APACHE)。

## 直接依赖

| crate | 版本 | 许可证 |
| --- | --- | --- |
| binance-core | 0.2.0 | MIT |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| clap | 4.6.7 | MIT OR Apache-2.0 |
| csv | 1.4.0 | Unlicense/MIT |
| dukascopy-core | 0.2.0 | MIT |
| rayon | 1.12.0 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.154 | MIT OR Apache-2.0 |
| sha2 | 0.11.1 | MIT OR Apache-2.0 |
| tempfile | 3.27.0 | MIT OR Apache-2.0 |
| thiserror | 2.0.21 | MIT OR Apache-2.0 |
| toml | 1.2.1+spec-1.1.0 | MIT OR Apache-2.0 |

`tempfile` 只用于测试。`binance-core` 与 `dukascopy-core` 通过 path 依赖
引入同级仓库 `data-reader`,固定提交为 `a844ea02e7fd43b73f9e5b9fe5d619e6cbe8ea21`
(2026-11-16),两者均为 MIT。完整传递依赖及精确版本见 [Cargo.lock](./Cargo.lock)。

## 许可检查

`deny.toml` 允许的许可证集合为 MIT、Apache-2.0、BSD-3-Clause、ISC、
Unicode-3.0 与 Zlib。`cargo deny check` 对当前 `Cargo.lock` 的 131 个依赖执行
advisories、bans、licenses 与 sources 检查,结果通过;未被使用的许可证白名单
只产生 `license-not-encountered` 告警。

Rust 工具链本身采用 MIT/Apache-2.0 许可。报告中的内联 SVG/CSS 由本项目生成,
不包含第三方图表库或 CDN 资源。
