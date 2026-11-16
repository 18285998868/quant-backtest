//! 二进制入口:只负责调用库并转换退出码。

use std::process::ExitCode;

fn main() -> ExitCode {
    backtest_cli::main_entry()
}
