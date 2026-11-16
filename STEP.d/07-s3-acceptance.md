# 07 S3 验收

## 目标

验证 `docs/03-流程设计.md` 的 T3.1–T3.9:多标的共享账户、再平衡、截面动量与逆波动率权重。

## 用例

| 用例 | 证据 |
| --- | --- |
| T3.1 两标的手算 | `t3_1_two_symbols_hand_calculated`,A qty=500、B qty=-1000、现金 100k、gross 100k |
| T3.2 价格变动 | `t3_2_portfolio_equity_tracks_price_moves`,权益 110k |
| T3.3 平仓再平衡 | `t3_3_rebalance_closes_positions_and_updates_cash`,A 卖 500、B 买 1000、现金 110k |
| T3.4 非法日历 | `rejects_timeline_mismatch_between_symbols`,错误含 symbol 与 ts |
| T3.5 非法货币 | `rejects_currency_mismatch`,启动时报错 |
| T3.6 截面动量 | `t3_6_top_k_selection_and_inverse_vol_weights`,权重 2/3、1/3、Σabs=1 |
| T3.7 逆波动率多空 | `t3_7_inverse_vol_keeps_long_short_directions`,权重 2/3、-1/3 |
| T3.8 σ 无效回退 | `t3_8_zero_vol_falls_back_to_equal_weights_with_warning`、`t3_8_insufficient_vol_history_falls_back_to_equal_weights`,等权 0.5/0.5 并告警 |
| T3.9 只在调仓 bar 输出 | `t3_9_only_rebalance_bars_emit_targets`,指数 5、10 输出 |

## 额外覆盖

- `sells_execute_before_buys_to_free_cash`:先卖后买,空头回补不被现金误裁剪。
- `position_reductions_fill_before_new_exposure`:减仓先于开仓,回补释放的保证金可参与后续开仓。
- `risk_exits_fill_before_targets_on_the_same_side`:同方向风险退出先成交。
- `strategy_warnings_reach_result_and_meta`:策略告警进入结果与元数据。
- `non_rebalance_bars_keep_history_for_next_signal`:非调仓 bar 继续积累历史。
- `loads_xs_momentum_config_and_hashes_parameters`、`xs_momentum_equal_weighting_omits_vol_parameters`、`rejects_invalid_xs_momentum_fields`、`rejects_xs_momentum_gross_above_margin_limit`:配置解析、哈希与校验。
- `strategy::tests::inverse_vol_weights_*`:权重函数手算、多空方向、无效 σ、零方向。

## 门禁

```bash
bash scripts/check.sh
cargo build --release
cargo run --release -- validate --config config/02.portfolio-smoke.toml
cargo run --release -- run --config config/02.portfolio-smoke.toml
```

## 真实冒烟

- 从空 `target/smoke-s3` 开始。
- `fixtures/portfolio_2.csv` 两个标的共享同一交易日历。
- `xs_momentum` 每 2 根 bar 调仓,lookback=2、vol_lookback=2、top_k=2、gross=0.9。
- 产物 `result.json` 与 `report.html` 存在,JSON 可解析,HTML 无外链。
- 保留产物再跑一次,`result_hash` 一致。

## 验收结果(2026-11-16)

```text
cargo fmt --all -- --check                                 通过
cargo test --workspace --all-targets                       82 passed
cargo clippy --workspace --all-targets --all-features      通过(-D warnings)
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps 通过
cargo build --release                                      通过
bash scripts/check.sh --ci                                 通过(deny/audit 离线)
```

deny/audit 使用本地 advisory 库离线运行;在线拉取因远程 TLS 失败,未改变检查结论。

S3 冒烟:

- `config/02.portfolio-smoke.toml` 校验通过,2 个标的、12 根 bar。
- 配置哈希 `a434c549b6e95f5cb5c5f3aae15eb1528a3048b8bccfaaf847de8960adbf6766`,数据哈希 `dcd6725781a9d96ff083476d1bddb4681d057cb71307e4e1bbd80a912c8f5b20`。
- 总收益 5.09%,CAGR 420.21%,最大回撤 4.35%;4 笔交易,10 笔成交,0 条告警。
- 二次运行 `result_hash` 一致:`8796ce7f400ccbd6a30592c2a98bfba0258b53f609e108a07767fc7a17e2a411`。
- `result.json` 可解析,`report.html` 无外链。

S1/S2 回归:

- `config/00.smoke.toml` 的 `result_hash` 保持 `144d1118f69392cf1e2369f8b0375037257d5e9eda9bbe59153f64ccf8ee5714`。
- `config/01.risk-smoke.toml` 的 `result_hash` 保持 `8476c2288d3f99a60c9b0ce53f09427e8ac25cb8f3e521194eb15ede8bc4d571`。

## 退出条件

- T3.1–T3.9 通过。
- fmt/clippy/test/doc 通过。
- 组合冒烟通过,且 S1/S2 回归哈希不变。
- 汇报已运行与未运行项。
