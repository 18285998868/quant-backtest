//! CLI 契约测试:退出码、错误上下文、配置边界与跨线程复现。

// 辅助函数不在 #[test] 内,`allow-expect-in-tests` 覆盖不到,测试代码统一放行。
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

fn fixture(path: &str) -> PathBuf {
    workspace_root().join(path)
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(workspace_root())
        .output()
        .expect("run quant-backtest")
}

fn assert_exit(output: &Output, expected: i32) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(expected),
        "expected exit {expected}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(!stderr.contains("panicked"), "panic detected:\n{stderr}");
}

fn write_config(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write config");
    path
}

fn base_config(data: &Path, out: &Path, symbol: &str) -> String {
    format!(
        r#"[run]
data = "{}"
initial_cash = 100000.0
output_dir = "{}"
threads = 1
memory_limit_gb = 0
timeframe = "1d"

[execution]
participation_rate = 0.0

[strategy]
name = "sma_cross"
fast = 2
slow = 3
weight = 0.99
allow_short = false

[[instruments]]
symbol = "{symbol}"
asset_class = "crypto"
currency = "USD"
multiplier = 1.0
lot_size = 0.0
margin_rate = 1.0
fee_bps = 0.0
fee_fixed = 0.0
slippage_bps = 0.0
periods_per_year = 365
"#,
        data.display(),
        out.display()
    )
}

fn walkforward_config(data: &Path, out: &Path, body: &str) -> String {
    format!(
        "{}\n[sweep]\nobjective = \"sharpe\"\ntop_k = 1\noos_split = 0.7\n\n\
         [sweep.grid]\nfast = [2]\nslow = [3]\n\n[walk_forward]\n{body}\n",
        base_config(data, out, "TEST")
    )
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn invalid_data_fixtures_exit_with_data_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cases = [
        ("empty.csv", "为空"),
        ("unsorted.csv", "乱序"),
        ("duplicate.csv", "重复"),
        ("negative_price.csv", "价格必须 > 0"),
        ("nan_price.csv", "有限值"),
        ("inf_price.csv", "有限值"),
        ("negative_volume.csv", "volume 必须是有限且 >= 0"),
        ("missing_column.csv", "缺少必需列"),
        ("bad_timestamp.csv", "ts:"),
        ("empty_symbol.csv", "symbol 不能为空"),
    ];
    for (name, needle) in cases {
        let data = fixture(&format!("fixtures/invalid/{name}"));
        let config = write_config(
            dir.path(),
            &format!("{name}.toml"),
            &base_config(&data, &dir.path().join("out"), "AAA"),
        );
        let output = run(&[
            "validate",
            "--config",
            config.to_str().expect("config path"),
        ]);
        assert_exit(&output, 3);
        let text = stderr(&output);
        assert!(text.contains(needle), "{name}: {text}");
    }

    let missing = write_config(
        dir.path(),
        "missing-data.toml",
        &base_config(
            &dir.path().join("absent.csv"),
            &dir.path().join("out"),
            "AAA",
        ),
    );
    let output = run(&[
        "validate",
        "--config",
        missing.to_str().expect("config path"),
    ]);
    assert_exit(&output, 3);
    assert!(stderr(&output).contains("解析 CSV"));
}

#[test]
fn zero_volume_is_a_valid_bar() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = fixture("fixtures/invalid/zero_volume.csv");
    let config = write_config(
        dir.path(),
        "zero-volume.toml",
        &base_config(&data, &dir.path().join("out"), "AAA"),
    );
    let output = run(&[
        "validate",
        "--config",
        config.to_str().expect("config path"),
    ]);
    assert_exit(&output, 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains("校验通过"));
}

#[test]
fn invalid_config_fields_exit_with_config_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = fixture("fixtures/sma_6bars.csv");
    let out = dir.path().join("out");
    let base = base_config(&data, &out, "TEST");

    let cases = [
        (
            "threads-zero.toml",
            base.replace("threads = 1", "threads = 0"),
            "run.threads",
        ),
        (
            "memory-negative.toml",
            base.replace("memory_limit_gb = 0", "memory_limit_gb = -1.0"),
            "run.memory_limit_gb",
        ),
        (
            "unknown-field.toml",
            base.replace("timeframe = \"1d\"", "timeframe = \"1d\"\nmystery = 1"),
            "mystery",
        ),
        (
            "walkforward-train-zero.toml",
            walkforward_config(
                &data,
                &out,
                "train_bars = 0\ntest_bars = 2\nstep_bars = 2\nwarmup_bars = 1",
            ),
            "walk_forward.train_bars",
        ),
        (
            "walkforward-test-zero.toml",
            walkforward_config(
                &data,
                &out,
                "train_bars = 2\ntest_bars = 0\nstep_bars = 2\nwarmup_bars = 1",
            ),
            "walk_forward.test_bars",
        ),
        (
            "walkforward-step-overlap.toml",
            walkforward_config(
                &data,
                &out,
                "train_bars = 2\ntest_bars = 2\nstep_bars = 1\nwarmup_bars = 1",
            ),
            "walk_forward.step_bars",
        ),
        (
            "walkforward-warmup-zero.toml",
            walkforward_config(
                &data,
                &out,
                "train_bars = 2\ntest_bars = 2\nstep_bars = 2\nwarmup_bars = 0",
            ),
            "walk_forward.warmup_bars",
        ),
        (
            "walkforward-warmup-large.toml",
            walkforward_config(
                &data,
                &out,
                "train_bars = 2\ntest_bars = 2\nstep_bars = 2\nembargo_bars = 1\nwarmup_bars = 5",
            ),
            "walk_forward.warmup_bars",
        ),
    ];
    for (name, body, needle) in cases {
        let config = write_config(dir.path(), name, &body);
        let output = run(&[
            "validate",
            "--config",
            config.to_str().expect("config path"),
        ]);
        assert_exit(&output, 2);
        let text = stderr(&output);
        assert!(text.contains(needle), "{name}: {text}");
    }
}

#[test]
fn cli_overrides_reject_invalid_threads_and_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = fixture("fixtures/sma_6bars.csv");
    let config = write_config(
        dir.path(),
        "run.toml",
        &base_config(&data, &dir.path().join("out"), "TEST"),
    );
    let path = config.to_str().expect("config path");

    let output = run(&["run", "--config", path, "--threads", "0"]);
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("threads"));

    let output = run(&["run", "--config", path, "--memory-limit-gb=-1"]);
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("memory_limit_gb"));

    let output = run(&["run", "--config", path, "--memory-limit-gb=0.001"]);
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("单 worker 预算"));
}

#[test]
fn walkforward_rejects_impossible_windows_without_panicking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = fixture("fixtures/s5_daily.csv");
    let out = dir.path().join("out");

    let no_window = write_config(
        dir.path(),
        "no-window.toml",
        &walkforward_config(
            &data,
            &out,
            "train_bars = 180\ntest_bars = 60\nstep_bars = 60\nembargo_bars = 2000\nwarmup_bars = 40",
        ),
    );
    let output = run(&[
        "walkforward",
        "--config",
        no_window.to_str().expect("config path"),
        "--threads",
        "1",
    ]);
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("不足以构成一个完整窗口"));

    let overflow = write_config(
        dir.path(),
        "overflow.toml",
        &walkforward_config(
            &data,
            &out,
            "train_bars = 9223372036854775807\ntest_bars = 9223372036854775807\n\
             step_bars = 9223372036854775807\nembargo_bars = 9223372036854775807\nwarmup_bars = 40",
        ),
    );
    let output = run(&[
        "walkforward",
        "--config",
        overflow.to_str().expect("config path"),
        "--threads",
        "1",
    ]);
    assert_exit(&output, 2);
    assert!(stderr(&output).contains("溢出"));
}

#[test]
fn result_hash_is_stable_across_threads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let data = fixture("fixtures/sma_6bars.csv");
    let out_a = dir.path().join("out-a");
    let out_b = dir.path().join("out-b");
    let config = write_config(
        dir.path(),
        "run.toml",
        &base_config(&data, &dir.path().join("out"), "TEST"),
    );
    let path = config.to_str().expect("config path");

    let first = run(&[
        "run",
        "--config",
        path,
        "--threads",
        "1",
        "--out",
        out_a.to_str().expect("out a"),
    ]);
    assert_exit(&first, 0);
    let second = run(&[
        "run",
        "--config",
        path,
        "--threads",
        "2",
        "--out",
        out_b.to_str().expect("out b"),
    ]);
    assert_exit(&second, 0);

    let read = |path: &Path| -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(path.join("result.json")).expect("read result.json"),
        )
        .expect("parse result.json")
    };
    let a = read(&out_a);
    let b = read(&out_b);
    assert_eq!(a["result_hash"], b["result_hash"]);
    assert_eq!(a["meta"]["threads"], 1);
    assert_eq!(b["meta"]["threads"], 2);
}
