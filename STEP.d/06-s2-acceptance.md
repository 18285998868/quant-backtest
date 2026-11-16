# 06 S2 验收

## 目标

验证 `docs/03-流程设计.md` 的 T2.1–T2.11:止损/止盈、跳空、同 bar 冲突、部分成交、保证金、做空会计。

## 用例

| 用例 | 证据 |
| --- | --- |
| T2.1 多头止损跳空 | `t2_1_long_stop_gaps_to_open`,成交价 92 |
| T2.2 多头止损非跳空 | `t2_2_long_stop_fills_at_trigger_price`,成交价 95 |
| T2.3 空头止损跳空 | `t2_3_short_stop_gaps_to_open`,成交价 108 |
| T2.4 同 bar 双触发 | `t2_4_ambiguous_bar_uses_stop_and_warns`,止损 95 且 warning 含 `ambiguous_bar=true` |
| T2.5 收盘触发 | `t2_5_close_trigger_fills_at_next_open`,下一根 open 93 |
| T2.6 部分成交 | `t2_6_participation_partially_fills_and_defers`,5 根各 100 |
| T2.7 保证金/敞口 | `t2_7_gross_exposure_above_margin_limit_is_rejected`,返回 ExecutionError |
| T2.8 做空 PnL | `t2_8_short_pnl_and_cash_are_consistent`,PnL 与现金一致 |
| T2.9 零成交量 | `t2_9_zero_volume_defers_stop_to_next_bar`,下一根成交 93 |
| T2.10 有利跳空止盈 | `t2_10_take_profit_gaps_to_open`,成交价 110 |
| T2.11 费用裁剪 | `t2_11_fees_clip_quantity_and_keep_cash_non_negative`,qty < 1000 且 cash >= 0 |

## 额外覆盖

- `close_trigger_ignores_intrabar_gap`:Close 模式不因盘中跳空误触发。
- `adding_to_position_keeps_original_stop`:加减仓不重置风险价。
- `loads_risk_legs_and_hashes_them`:风险配置参与配置哈希。
- `rejects_invalid_risk_legs`:非法 distance 与 trigger 明确报错。

## 门禁

```bash
bash scripts/check.sh
cargo build --release
cargo run --release -- validate --config config/01.risk-smoke.toml
cargo run --release -- run --config config/01.risk-smoke.toml
```

## 真实冒烟

- 从空 `target/smoke-s2` 开始。
- `fixtures/risk_8bars.csv` 中 SMA 在 t5 开盘 102 入场,止损价 96.9。
- t6 开盘 94 跳空触发止损,成交价 94。
- 产物 `result.json` 与 `report.html` 存在,JSON 可解析,HTML 无外链。
- 保留产物再跑一次,`result_hash` 一致。

## 验收结果(2026-11-16)

```text
cargo fmt --all -- --check                                 通过
cargo test --workspace --all-targets                       60 passed
cargo clippy --workspace --all-targets --all-features      通过(-D warnings)
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps 通过
cargo build --release                                      通过
bash scripts/check.sh --ci                                 通过(deny/audit ok)
```

后续说明:S2 验收时为 60 个测试;S3 新增用例后当前全量为 82 passed,见 `./07-s3-acceptance.md`。

S2 冒烟:

- `config/01.risk-smoke.toml` 校验通过。
- 2020-01-06 开盘 102 入场,止损价 96.9;2020-01-07 开盘 94 跳空触发 5% 止损,trade PnL `-7764.705882352941`。
- 1 笔已平仓交易,2 笔成交,退出原因 `stop_loss`。
- 二次运行 `result_hash` 一致:`cd330ad9d98802e9d3492a61da4d24fcf5b17f1e51cba8b718d8a72e7a25979e`。
- HTML 无外链。

S1 回归:

- `config/00.smoke.toml` 仍通过,`result_hash` 保持 `efbb00b9f70690e5238630c65dea13922621fff36059ec26367fee66e067b977`。

## 退出条件

- T2.1–T2.11 通过。
- fmt/clippy/test/doc 通过。
- 风险冒烟通过。
- 汇报已运行与未运行项。
