//! 回测领域核心:模型、配置、执行、指标与系统检测。
//!
//! 数据加载见 `backtest-data`,内置策略见 `backtest-strategy`,报告见 `backtest-report`。

pub mod config;
pub mod engine;
pub mod error;
pub mod metrics;
pub mod model;
pub mod strategy;
pub mod system;
pub mod walkforward;

pub use error::{Error, Result};
