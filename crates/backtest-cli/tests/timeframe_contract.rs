//! M1 周期契约:小时/周线运行、年化因子、报告与扫描/walk-forward。

// 辅助函数不在 #[test] 内,测试代码统一放行 expect。
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_quant-backtest");

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(workspace_root())
        .output()
        .expect("run quant-backtest")
}

fn assert_ok(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(!stderr.contains("panicked"), "panic detected:\n{stderr}");
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read json")).expect("parse json")
}

#[test]
fn hourly_run_uses_hourly_annualization() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out");
    let output = run(&[
        "run",
        "--config",
        "config/04.intraday.toml",
        "--threads",
        "1",
        "--out",
        out.to_str().expect("out"),
    ]);
    assert_ok(&output);

    let value = read_json(&out.join("result.json"));
    assert_eq!(value["meta"]["timeframe"], "1h");
    assert_eq!(value["metrics"]["periods_per_year"], 8760);
    assert!(
        value["trades"]
            .as_array()
            .is_some_and(|trades| !trades.is_empty()),
        "1h 夹具应产生交易"
    );
    let html = std::fs::read_to_string(out.join("report.html")).expect("html");
    assert!(html.contains("周期 1h"));
    assert!(html.contains("复合 bar 收益"));
    assert!(!html.contains("S1–S5 只实现日线"));
}

#[test]
fn weekly_run_uses_weekly_annualization() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out");
    let output = run(&[
        "run",
        "--config",
        "config/05.weekly.toml",
        "--threads",
        "1",
        "--out",
        out.to_str().expect("out"),
    ]);
    assert_ok(&output);

    let value = read_json(&out.join("result.json"));
    assert_eq!(value["meta"]["timeframe"], "1w");
    assert_eq!(value["metrics"]["periods_per_year"], 52);
    assert_eq!(
        value["equity"].as_array().map(Vec::len),
        Some(16),
        "1w 夹具应产出 16 个权益点"
    );
    let html = std::fs::read_to_string(out.join("report.html")).expect("html");
    assert!(html.contains("周期 1w"));
}

#[test]
fn invalid_timeframe_exits_with_config_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original = std::fs::read_to_string(workspace_root().join("config/04.intraday.toml"))
        .expect("read config");
    let path = dir.path().join("invalid.toml");
    std::fs::write(
        &path,
        original.replace("timeframe = \"1h\"", "timeframe = \"5x\""),
    )
    .expect("write config");

    let output = run(&["validate", "--config", path.to_str().expect("config path")]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("timeframe"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn intraday_sweep_and_walkforward_keep_timeframe() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut text = std::fs::read_to_string(workspace_root().join("config/04.intraday.toml"))
        .expect("read config");
    text.push_str(
        r#"
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
"#,
    );
    let path = dir.path().join("intraday-grid.toml");
    std::fs::write(&path, text).expect("write config");
    let path = path.to_str().expect("config path");

    let sweep_out = dir.path().join("sweep");
    let output = run(&[
        "sweep",
        "--config",
        path,
        "--threads",
        "1",
        "--out",
        sweep_out.to_str().expect("sweep out"),
    ]);
    assert_ok(&output);
    let sweep = read_json(&sweep_out.join("result.json"));
    assert_eq!(sweep["meta"]["timeframe"], "1h");
    assert!(sweep_out.join("sweep_summary.json").exists());

    let wf_out = dir.path().join("walkforward");
    let output = run(&[
        "walkforward",
        "--config",
        path,
        "--threads",
        "1",
        "--out",
        wf_out.to_str().expect("walkforward out"),
    ]);
    assert_ok(&output);
    let walkforward = read_json(&wf_out.join("result.json"));
    assert_eq!(walkforward["meta"]["timeframe"], "1h");
    assert!(
        walkforward["walk_forward"]["windows"]
            .as_array()
            .is_some_and(|windows| !windows.is_empty()),
        "walk-forward 应至少有一个窗口"
    );
}
