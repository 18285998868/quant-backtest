//! `quant-backtest` 的 CLI 编排。
//!
//! 本 crate 只做参数解析、配置/数据装载、引擎调用与报告落盘;
//! 领域逻辑全部在 `backtest-core`、`backtest-data`、`backtest-strategy`、`backtest-report`。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use backtest_core::analysis::{BootstrapStatistic, RunAnalysisInput, analyze_run};
use backtest_core::config::{RunConfig, ThreadSetting, load_config};
use backtest_core::engine::{EngineInput, run_backtest};
use backtest_core::error::{Error, Result};
use backtest_core::metrics::equity_returns;
use backtest_core::model::{InstrumentSpec, Timeframe, Universe};
use backtest_core::strategy::Strategy;
use backtest_core::system::{
    Capacity, DEFAULT_WORKER_BUDGET_BYTES, ResolvedWorkers, detect_capacity, resolve_workers,
};
use backtest_data::{DataPlan, LoadOptions, LoadSource, load_universe_reporting};
use backtest_report::{ReportExtras, ReportPaths, write_report};
use backtest_strategy::{SmaCross, build};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use rayon::prelude::*;

mod stream;
mod sweep;
mod walkforward;

const BENCH_TASKS: usize = 500;
const BENCH_BARS: usize = 2_500;
const BENCH_REPS: usize = 3;
const THROUGHPUT_FLOOR_2C: f64 = 1_000_000.0;

#[derive(Parser)]
#[command(
    name = "quant-backtest",
    version,
    about = "日线级多标的策略回测引擎",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 校验配置与数据,不执行回测
    Validate {
        /// 运行配置 TOML
        #[arg(long, value_name = "run.toml")]
        config: PathBuf,
    },
    /// 执行一次回测并输出 JSON 与 HTML
    Run {
        /// 运行配置 TOML
        #[arg(long, value_name = "run.toml")]
        config: PathBuf,
        /// 覆盖配置中的线程数
        #[arg(long)]
        threads: Option<usize>,
        /// 覆盖配置中的内存上限(GiB)
        #[arg(long)]
        memory_limit_gb: Option<f64>,
        /// 覆盖配置中的输出目录
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 参数扫描并输出 sweep.csv 与 sweep_summary.json
    Sweep {
        /// 扫描配置 TOML
        #[arg(long, value_name = "sweep.toml")]
        config: PathBuf,
        /// 覆盖配置中的线程数
        #[arg(long)]
        threads: Option<usize>,
        /// 覆盖配置中的内存上限(GiB)
        #[arg(long)]
        memory_limit_gb: Option<f64>,
        /// 覆盖配置中的输出目录
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Walk-forward 逐窗选参并输出 OOS 报告
    Walkforward {
        /// walk-forward 配置 TOML(需同时含 [sweep] 与 [walk_forward])
        #[arg(long, value_name = "walkforward.toml")]
        config: PathBuf,
        /// 覆盖配置中的线程数
        #[arg(long)]
        threads: Option<usize>,
        /// 覆盖配置中的内存上限(GiB)
        #[arg(long)]
        memory_limit_gb: Option<f64>,
        /// 覆盖配置中的输出目录
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 合成负载吞吐基准
    Bench {
        /// 固定线程数
        #[arg(long)]
        threads: Option<usize>,
        /// 依次跑 1/2/4/8 线程
        #[arg(long)]
        scale: bool,
        /// 任务数,默认 500
        #[arg(long, default_value_t = BENCH_TASKS)]
        tasks: usize,
        /// 每个任务的 bar 数,默认 2500
        #[arg(long, default_value_t = BENCH_BARS)]
        bars: usize,
        /// 合成标的数,默认 1
        #[arg(long, default_value_t = 1)]
        symbols: usize,
    },
}

pub fn main_entry() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误: {error}");
            ExitCode::from(error.exit_code() as u8)
        }
    }
}

fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Validate { config } => cmd_validate(&config),
        Command::Run {
            config,
            threads,
            memory_limit_gb,
            out,
        } => cmd_run(&config, threads, memory_limit_gb, out),
        Command::Sweep {
            config,
            threads,
            memory_limit_gb,
            out,
        } => cmd_sweep(&config, threads, memory_limit_gb, out),
        Command::Walkforward {
            config,
            threads,
            memory_limit_gb,
            out,
        } => cmd_walkforward(&config, threads, memory_limit_gb, out),
        Command::Bench {
            threads,
            scale,
            tasks,
            bars,
            symbols,
        } => cmd_bench(threads, scale, tasks, bars, symbols),
    }
}

fn cmd_validate(path: &Path) -> Result<()> {
    let config = load_config(path)?;
    let workers = resolve_for(&config)?;
    print_capacity(&workers);
    let history = build(&config.strategy, config.instruments.len()).history_bars();
    let loaded = prepare_data(&config, &workers, 1, history)?;
    print_data(&config, &loaded, &workers);
    println!("校验通过: {}", path.display());
    Ok(())
}

fn cmd_run(
    path: &Path,
    threads: Option<usize>,
    memory_limit_gb: Option<f64>,
    out: Option<PathBuf>,
) -> Result<()> {
    let mut config = load_config(path)?;
    config.apply_overrides(threads, memory_limit_gb, out)?;
    let workers = resolve_for(&config)?;
    print_capacity(&workers);
    let history = build(&config.strategy, config.instruments.len()).history_bars();
    let loaded = prepare_data(&config, &workers, 1, history)?;
    print_data(&config, &loaded, &workers);

    let result = match &loaded {
        Loaded::Full(universe) => {
            let mut strategy = build(&config.strategy, config.instruments.len());
            let input = EngineInput {
                initial_cash: config.initial_cash,
                risk_free_rate: config.risk_free_rate,
                execution: config.execution,
                risk: config.risk,
                strategy_name: config.strategy.name().to_string(),
                params: config.strategy.params(),
                config_hash: config.config_hash.clone(),
                threads: workers.threads,
                requested_start: config.start,
                requested_end: config.end,
                start_index: 0,
                trade_start_index: 0,
                end_index: universe.len(),
            };
            run_backtest(universe, &input, strategy.as_mut())?
        }
        Loaded::Stream { plan, chunk_bars } => stream::run_streaming(
            &config,
            plan,
            workers.threads,
            sweep::IndexRange::full(plan.len()),
            *chunk_bars,
        )?,
    };
    let extras = if config.analysis.enabled {
        let returns = equity_returns(&result.equity, config.initial_cash);
        let analysis = analyze_run(&RunAnalysisInput {
            returns: &returns,
            periods_per_year: result.metrics.periods_per_year,
            seed: config.analysis.seed,
            bootstrap_samples: config.analysis.bootstrap_samples,
        })?;
        ReportExtras {
            analysis: Some(analysis),
            ..ReportExtras::default()
        }
    } else {
        ReportExtras::default()
    };
    let paths = write_report(&result, &extras, &config.output_dir)?;
    print_result(&result, &extras, &paths);
    Ok(())
}

fn cmd_sweep(
    path: &Path,
    threads: Option<usize>,
    memory_limit_gb: Option<f64>,
    out: Option<PathBuf>,
) -> Result<()> {
    let mut config = load_config(path)?;
    if config.sweep.is_none() {
        return Err(backtest_core::error::ConfigError::Invalid {
            field: "sweep".into(),
            message: "配置缺少 [sweep] 段".into(),
        }
        .into());
    }
    config.apply_overrides(threads, memory_limit_gb, out)?;
    warn_analysis_unsupported(&config, "sweep");
    let workers = resolve_for(&config)?;
    print_capacity(&workers);
    let history = sweep::Grid::build(&config)?.max_warmup_bars();
    let loaded = prepare_data(&config, &workers, workers.threads, history)?;
    print_data(&config, &loaded, &workers);
    let data = match &loaded {
        Loaded::Full(universe) => sweep::Data::Full(universe),
        Loaded::Stream { plan, chunk_bars } => sweep::Data::Stream {
            plan,
            chunk_bars: *chunk_bars,
        },
    };
    let run = sweep::execute(
        &config,
        &data,
        &workers,
        threads.is_some(),
        memory_limit_gb.is_some(),
    )?;
    let paths = sweep::write_outputs(&run, &config.output_dir)?;
    let summary = &run.summary;
    println!(
        "扫描:共 {} 组,有效 {},跳过非法 {},告警 {}",
        summary.counts.total,
        summary.counts.valid,
        summary.counts.skipped_invalid,
        summary.counts.warnings
    );
    println!(
        "目标:{} top_k={} oos_split={} 线程={}",
        summary.meta.objective, summary.meta.top_k, summary.meta.oos_split, summary.meta.threads
    );
    if let Some(first) = summary.results.first() {
        let params = first
            .params
            .iter()
            .map(|(name, value)| format!("{name}={}", param_text(value)))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "最佳:#1 {} sharpe={} cagr={} calmar={} 参数[{}]",
            first.config_hash,
            format_opt(first.metrics.sharpe),
            format_opt(first.metrics.cagr),
            format_opt(first.metrics.calmar),
            params
        );
    }
    println!("哈希:sweep={}", summary.sweep_hash);
    println!("产物:{}", paths.csv.display());
    println!("产物:{}", paths.json.display());
    let extras = ReportExtras {
        sweep: Some(run.report),
        ..ReportExtras::default()
    };
    let report_paths = write_report(&run.best_full, &extras, &config.output_dir)?;
    println!("产物:{}", report_paths.json.display());
    println!("产物:{}", report_paths.html.display());
    Ok(())
}

fn cmd_walkforward(
    path: &Path,
    threads: Option<usize>,
    memory_limit_gb: Option<f64>,
    out: Option<PathBuf>,
) -> Result<()> {
    let mut config = load_config(path)?;
    if config.sweep.is_none() {
        return Err(backtest_core::error::ConfigError::Invalid {
            field: "sweep".into(),
            message: "配置缺少 [sweep] 段".into(),
        }
        .into());
    }
    let wf =
        config
            .walk_forward
            .clone()
            .ok_or_else(|| backtest_core::error::ConfigError::Invalid {
                field: "walk_forward".into(),
                message: "配置缺少 [walk_forward] 段".into(),
            })?;
    config.apply_overrides(threads, memory_limit_gb, out)?;
    warn_analysis_unsupported(&config, "walkforward");
    let workers = resolve_for(&config)?;
    print_capacity(&workers);
    let grid = sweep::Grid::build(&config)?;
    let loaded = prepare_data(&config, &workers, workers.threads, grid.max_warmup_bars())?;
    print_data(&config, &loaded, &workers);
    let data = match &loaded {
        Loaded::Full(universe) => sweep::Data::Full(universe),
        Loaded::Stream { plan, chunk_bars } => sweep::Data::Stream {
            plan,
            chunk_bars: *chunk_bars,
        },
    };
    let run = walkforward::execute(&config, &wf, &grid, &data, &workers)?;
    let result = &run.result;
    println!(
        "窗口:{} 个,跳过 {} 个,目标 {}",
        result.windows.len(),
        result.skipped_windows.len(),
        result.objective
    );
    println!(
        "OOS:总收益={} sharpe={} maxdd={} 交易={}",
        format_opt(result.oos_metrics.total_return),
        format_opt(result.oos_metrics.sharpe),
        format_opt(result.oos_metrics.max_drawdown),
        result.oos_metrics.trades
    );
    if let Some(dsr) = &result.dsr {
        println!(
            "DSR:{} (N={}, 每期 SR={})",
            format_opt(Some(dsr.probability)),
            dsr.trials,
            format_opt(Some(dsr.sharpe_per_period))
        );
    }
    let extras = ReportExtras {
        walk_forward: Some(run.result),
        ..ReportExtras::default()
    };
    let paths = write_report(&run.base, &extras, &config.output_dir)?;
    println!("产物:{}", paths.json.display());
    println!("产物:{}", paths.html.display());
    Ok(())
}

fn param_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn format_opt(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.6}"))
        .unwrap_or_else(|| "—".into())
}

fn cmd_bench(
    threads: Option<usize>,
    scale: bool,
    tasks: usize,
    bars: usize,
    symbols: usize,
) -> Result<()> {
    if tasks == 0 || bars == 0 || symbols == 0 {
        return Err(backtest_core::error::ConfigError::Invalid {
            field: "bench.tasks/bench.bars/bench.symbols".into(),
            message: "必须大于 0".into(),
        }
        .into());
    }
    let capacity = detect_capacity();
    let workers = resolve_for_requested(threads, None, &capacity)?;
    print_capacity(&workers);
    println!(
        "基准:任务数 {tasks},每任务 {bars} bar × {symbols} 标的,每个策略重复 {BENCH_REPS} 次,单 worker 预算 {} MiB",
        DEFAULT_WORKER_BUDGET_BYTES / (1024 * 1024)
    );

    let thread_list: Vec<usize> = if scale {
        vec![1, 2, 4, 8]
    } else {
        vec![workers.threads]
    };
    let universe = Arc::new(synthetic_universe(bars, symbols));
    let mut rates = Vec::new();
    for thread in thread_list {
        if thread > capacity.cpu_available {
            println!(
                "警告: 线程 {thread} 超过检测到的 {} 核,结果可能受超订影响",
                capacity.cpu_available
            );
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(thread)
            .build()
            .map_err(|error| backtest_core::error::ConfigError::Invalid {
                field: "bench.threads".into(),
                message: error.to_string(),
            })?;
        for kind in [BenchKind::Empty, BenchKind::SmaCross] {
            let rate = measure(&pool, kind, tasks, &universe)?;
            rates.push((kind.label(), thread, rate));
            let per_task_us = if rate > 0.0 {
                (bars * symbols) as f64 / rate * 1_000_000.0
            } else {
                f64::INFINITY
            };
            println!(
                "{:<10} threads={thread:<2} tasks={tasks} bars={bars} symbols={symbols} reps={BENCH_REPS} throughput={rate:.0} bar步/秒 每任务={per_task_us:.3} us 1.25e9步外推={:.1} 分钟",
                kind.label(),
                projected_minutes(rate)
            );
        }
    }

    for (label, thread, rate) in &rates {
        if *thread == 2 && *rate < THROUGHPUT_FLOOR_2C {
            println!(
                "警告: {label} 在 2 线程下 {rate:.0} bar步/秒,低于门槛 {THROUGHPUT_FLOOR_2C:.0}"
            );
        }
    }
    println!(
        "检测:CPU {} 核,可用内存 {}",
        capacity.cpu_available,
        capacity
            .mem_available_bytes
            .map(format_gib)
            .unwrap_or_else(|| "未知".into())
    );
    Ok(())
}

fn projected_minutes(rate: f64) -> f64 {
    const TARGET_STEPS: f64 = 500.0 * 2500.0 * 1000.0;
    if rate <= 0.0 {
        f64::INFINITY
    } else {
        TARGET_STEPS / rate / 60.0
    }
}

#[derive(Clone, Copy)]
enum BenchKind {
    Empty,
    SmaCross,
}

impl BenchKind {
    fn label(self) -> &'static str {
        match self {
            BenchKind::Empty => "空策略",
            BenchKind::SmaCross => "双均线",
        }
    }

    fn make(self) -> Box<dyn Strategy> {
        match self {
            BenchKind::Empty => Box::new(EmptyStrategy),
            BenchKind::SmaCross => Box::new(SmaCross::new(2, 20, 0.99, true, 1)),
        }
    }
}

struct EmptyStrategy;

impl Strategy for EmptyStrategy {
    fn on_bar(
        &mut self,
        _ctx: &backtest_core::strategy::MarketContext<'_>,
        _out: &mut Vec<backtest_core::strategy::TargetPosition>,
    ) {
    }
}

fn measure(
    pool: &rayon::ThreadPool,
    kind: BenchKind,
    tasks: usize,
    universe: &Arc<Universe>,
) -> Result<f64> {
    let steps = tasks.saturating_mul(universe.len());
    let throughput_steps = steps.saturating_mul(universe.symbol_count());
    let started = Instant::now();
    for _ in 0..BENCH_REPS {
        let results: Result<Vec<usize>> = pool.install(|| {
            (0..tasks)
                .into_par_iter()
                .map(|_| {
                    let mut strategy = kind.make();
                    let input = bench_input();
                    let result = run_backtest(universe, &input, strategy.as_mut())?;
                    Ok(result.equity.len())
                })
                .collect()
        });
        let done: usize = results?.into_iter().sum();
        if done != steps {
            return Err(backtest_core::error::ConfigError::Invalid {
                field: "bench.steps".into(),
                message: format!("期望 {steps} 步,实际 {done} 步"),
            }
            .into());
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    if elapsed <= 0.0 {
        return Ok(f64::INFINITY);
    }
    Ok(throughput_steps as f64 * BENCH_REPS as f64 / elapsed)
}

fn bench_input() -> EngineInput {
    EngineInput {
        initial_cash: 100_000.0,
        risk_free_rate: 0.0,
        execution: backtest_core::config::ExecutionConfig::default(),
        risk: backtest_core::config::RiskConfig::default(),
        strategy_name: "bench".into(),
        params: std::collections::BTreeMap::new(),
        config_hash: String::new(),
        threads: 1,
        requested_start: None,
        requested_end: None,
        start_index: 0,
        trade_start_index: 0,
        end_index: usize::MAX,
    }
}

fn synthetic_universe(bars: usize, symbols: usize) -> Universe {
    let base = 1_600_000_000_000_i64;
    let day = 86_400_000_i64;
    let mut ts = Vec::with_capacity(bars);
    let mut open = Vec::with_capacity(bars);
    let mut high = Vec::with_capacity(bars);
    let mut low = Vec::with_capacity(bars);
    let mut close = Vec::with_capacity(bars);
    let mut volume = Vec::with_capacity(bars);
    for index in 0..bars {
        let wave = (index as f64 / 17.0).sin() * 4.0;
        let drift = index as f64 * 0.002;
        let price = 100.0 + wave + drift;
        ts.push(base + index as i64 * day);
        open.push(price);
        high.push(price * 1.01);
        low.push(price * 0.99);
        close.push(price);
        volume.push(1_000_000.0);
    }
    let mut specs = Vec::with_capacity(symbols);
    let mut series = Vec::with_capacity(symbols);
    for symbol in 0..symbols {
        specs.push(InstrumentSpec {
            symbol: format!("SYN{symbol}"),
            asset_class: backtest_core::model::AssetClass::Crypto,
            currency: "USD".into(),
            multiplier: 1.0,
            lot_size: 0.0,
            margin_rate: 1.0,
            fee_bps: 0.0,
            fee_fixed: 0.0,
            slippage_bps: 0.0,
            periods_per_year: Some(365),
        });
        series.push(backtest_core::model::BarSeries {
            symbol: symbol as u32,
            ts: ts.clone(),
            open: open.clone(),
            high: high.clone(),
            low: low.clone(),
            close: close.clone(),
            volume: volume.clone(),
        });
    }
    Universe {
        timeframe: Timeframe::ONE_DAY,
        specs,
        series,
        funding: Vec::new(),
        timeline: ts,
        data_hash: String::new(),
        base_index: 0,
    }
}

fn resolve_for(config: &RunConfig) -> Result<ResolvedWorkers> {
    let requested = match config.threads {
        ThreadSetting::Auto => None,
        ThreadSetting::Fixed(value) => Some(value),
    };
    resolve_for_requested(
        requested,
        (config.memory_limit_gb > 0.0).then_some(config.memory_limit_gb),
        &detect_capacity(),
    )
}

fn resolve_for_requested(
    requested: Option<usize>,
    memory_limit_gb: Option<f64>,
    capacity: &Capacity,
) -> Result<ResolvedWorkers> {
    let resolved = resolve_workers(
        requested,
        memory_limit_gb,
        capacity,
        DEFAULT_WORKER_BUDGET_BYTES,
    )?;
    for note in &capacity.notes {
        println!("系统: {note}");
    }
    for warning in &resolved.warnings {
        println!("警告: {warning}");
    }
    Ok(resolved)
}

fn load(config: &RunConfig) -> Result<Universe> {
    let (universe, diagnostics) = load_universe_reporting(&LoadOptions {
        source: LoadSource::from(&config.data),
        timeframe: config.timeframe,
        instruments: &config.instruments,
        start: config.start,
        end: config.end,
    })?;
    for note in diagnostics.notes {
        println!("数据源: {note}");
    }
    Ok(universe)
}

/// 已装载的数据:整段驻留,或只保留规划并按窗口读取。
enum Loaded {
    Full(Universe),
    Stream { plan: DataPlan, chunk_bars: usize },
}

/// 根据显式 `chunk_bars` 或内存上限决定装载方式。
///
/// 未配置分块且没有显式内存上限时直接整段装载,避免多扫一遍数据。
fn prepare_data(
    config: &RunConfig,
    workers: &ResolvedWorkers,
    parallelism: usize,
    history_bars: usize,
) -> Result<Loaded> {
    let decide = config.chunk_bars > 0 || config.memory_limit_gb > 0.0;
    if !decide {
        return Ok(Loaded::Full(load(config)?));
    }
    let (plan, diagnostics) = stream::plan_data(config)?;
    if let Some(chunk_bars) = stream::choose_chunk(
        config.chunk_bars,
        config.memory_limit_gb,
        &plan,
        workers.memory_limit_bytes,
        parallelism,
        history_bars,
    ) {
        for note in diagnostics.notes {
            println!("数据源: {note}");
        }
        Ok(Loaded::Stream { plan, chunk_bars })
    } else {
        Ok(Loaded::Full(load(config)?))
    }
}

fn print_data(config: &RunConfig, loaded: &Loaded, workers: &ResolvedWorkers) {
    match loaded {
        Loaded::Full(universe) => print_universe(config, universe, workers),
        Loaded::Stream { plan, chunk_bars } => {
            print_plan(config, plan, workers, *chunk_bars);
        }
    }
}

fn print_plan(config: &RunConfig, plan: &DataPlan, workers: &ResolvedWorkers, chunk_bars: usize) {
    let first = plan.timeline.first().copied().map(format_ts);
    let last = plan.timeline.last().copied().map(format_ts);
    println!(
        "数据:{} 个标的,{} 根 bar,{} 至 {}",
        plan.symbol_count(),
        plan.len(),
        first.unwrap_or_else(|| "—".into()),
        last.unwrap_or_else(|| "—".into())
    );
    let full = plan.estimated_data_bytes();
    let window = plan.bytes_per_bar() * chunk_bars as u64;
    println!(
        "数据内存:全量约 {},分块每窗口 {} 根/约 {},另加策略历史",
        format_bytes(full),
        chunk_bars,
        format_bytes(window)
    );
    if let Some(limit) = workers.memory_limit_bytes
        && window > limit
    {
        eprintln!(
            "警告: 单窗口估算 {} 超过内存上限 {}",
            format_bytes(window),
            format_bytes(limit)
        );
    }
    println!("哈希:config={} data={}", config.config_hash, plan.data_hash);
    println!(
        "标的:{}",
        plan.specs
            .iter()
            .map(|spec| spec.symbol.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("周期: {}", plan.timeframe);
}

fn print_capacity(workers: &ResolvedWorkers) {
    println!(
        "线程:自动检测 {} 核,最终使用 {} 线程;内存上限 {}",
        workers.cpu_available,
        workers.threads,
        workers
            .memory_limit_bytes
            .map(format_gib)
            .unwrap_or_else(|| "未知".into())
    );
}

fn print_universe(config: &RunConfig, universe: &Universe, workers: &ResolvedWorkers) {
    let first = universe.timeline.first().copied().map(format_ts);
    let last = universe.timeline.last().copied().map(format_ts);
    println!(
        "数据:{} 个标的,{} 根 bar,{} 至 {}",
        universe.symbol_count(),
        universe.len(),
        first.unwrap_or_else(|| "—".into()),
        last.unwrap_or_else(|| "—".into())
    );
    let estimated = universe.estimated_data_bytes();
    println!("数据内存:约 {}", format_bytes(estimated));
    if let Some(warning) = data_memory_warning(estimated, workers.memory_limit_bytes) {
        eprintln!("警告: {warning}");
    }
    println!(
        "哈希:config={} data={}",
        config.config_hash, universe.data_hash
    );
    println!(
        "标的:{}",
        universe
            .specs
            .iter()
            .map(|spec| spec.symbol.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("周期: {}", universe.timeframe);
}

fn data_memory_warning(estimated: u64, limit: Option<u64>) -> Option<String> {
    let limit = limit?;
    if estimated <= limit {
        return None;
    }
    Some(format!(
        "数据常驻内存约 {},超过内存上限 {};分钟级数据请缩小 run.start/run.end 或改用更粗周期",
        format_bytes(estimated),
        format_bytes(limit)
    ))
}

fn warn_analysis_unsupported(config: &RunConfig, command: &str) {
    if config.analysis.enabled {
        eprintln!(
            "警告: analysis.enabled=true,但当前版本仅 run 支持 bootstrap;{command} 未执行分析"
        );
    }
}

fn print_result(
    result: &backtest_core::model::BacktestResult,
    extras: &ReportExtras,
    paths: &ReportPaths,
) {
    let metrics = &result.metrics;
    println!(
        "结果:总收益 {} CAGR {},最大回撤 {}",
        format_pct(metrics.total_return),
        format_pct(metrics.cagr),
        format_pct(metrics.max_drawdown)
    );
    println!(
        "交易:{} 笔,费用 {:.2},成交 {} 笔",
        metrics.trades,
        metrics.total_fees,
        result.fills.len()
    );
    if let Some(funding) = &result.funding {
        println!(
            "资金费:净支出 {:.2},付出 {:.2},收到 {:.2},结算 {} 次",
            funding.total_cost, funding.paid, funding.received, funding.events
        );
    }
    if let Some(analysis) = &extras.analysis {
        if let Some(bootstrap) = &analysis.bootstrap {
            let sharpe = bootstrap
                .statistics
                .iter()
                .find(|stat| stat.name == BootstrapStatistic::Sharpe);
            match sharpe {
                Some(stat) => println!(
                    "分析:bootstrap {} 次,块长 {},Sharpe {:.0}% 区间 [{}, {}]",
                    bootstrap.samples,
                    bootstrap.block_length,
                    bootstrap.confidence * 100.0,
                    format_num(stat.lower),
                    format_num(stat.upper)
                ),
                None => println!(
                    "分析:bootstrap {} 次,块长 {},没有 Sharpe 结果",
                    bootstrap.samples, bootstrap.block_length
                ),
            }
        } else {
            println!("分析:已启用,但没有可用的 bootstrap 结果");
        }
        for warning in &analysis.warnings {
            println!("分析警告: {warning}");
        }
    }
    println!("产物:{}", paths.json.display());
    println!("产物:{}", paths.html.display());
    if !result.warnings.is_empty() {
        for warning in &result.warnings {
            println!("警告: {warning}");
        }
    }
}

fn format_num(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.3}"))
        .unwrap_or_else(|| "—".into())
}

fn format_pct(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.2}%", value * 100.0))
        .unwrap_or_else(|| "—".into())
}

fn format_ts(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ts)
        .map(|value| value.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn format_gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.2} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else {
        format!("{:.1} KiB", value / KIB)
    }
}

/// 把任意错误转成退出码,便于测试与嵌入。
pub fn exit_code(error: &Error) -> u8 {
    error.exit_code() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_universe_is_well_formed() {
        let universe = synthetic_universe(64, 1);
        assert_eq!(universe.len(), 64);
        for index in 0..universe.len() {
            let bar = universe.bar(0, index);
            assert!(bar.low < bar.open);
            assert!(bar.high > bar.open);
        }
    }

    #[test]
    fn format_helpers_are_stable() {
        assert_eq!(format_pct(None), "—");
        assert_eq!(format_pct(Some(0.0123)), "1.23%");
        assert_eq!(format_gib(2 * 1024 * 1024 * 1024), "2.00 GiB");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn data_memory_warning_only_fires_above_limit() {
        assert!(data_memory_warning(100, None).is_none());
        assert!(data_memory_warning(100, Some(100)).is_none());
        let warning = data_memory_warning(101, Some(100)).expect("warning");
        assert!(warning.contains("超过内存上限"), "{warning}");
        assert!(warning.contains("run.start/run.end"), "{warning}");
    }

    #[test]
    fn t1_5_end_to_end_runs_are_deterministic() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let data = root.join("fixtures/sma_6bars.csv");
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("run.toml");
        let out_a = dir.path().join("out-a");
        let out_b = dir.path().join("out-b");
        let config = format!(
            r#"[run]
data = "{}"
initial_cash = 100000.0
start = "2020-01-01T00:00:00Z"
end = "2020-01-07T00:00:00Z"
output_dir = "{}"
threads = 1
memory_limit_gb = 0
timeframe = "1d"

[strategy]
name = "sma_cross"
fast = 2
slow = 3
weight = 0.99
allow_short = false

[[instruments]]
symbol = "TEST"
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
            out_a.display()
        );
        std::fs::write(&config_path, config).expect("write config");

        cmd_run(&config_path, None, None, Some(out_a.clone())).expect("first run");
        cmd_run(&config_path, None, None, Some(out_b.clone())).expect("second run");

        let a: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out_a.join("result.json")).expect("json"),
        )
        .expect("parse");
        let b: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out_b.join("result.json")).expect("json"),
        )
        .expect("parse");
        assert_eq!(a["result_hash"], b["result_hash"]);
        assert!(out_a.join("report.html").exists());
        assert!(out_b.join("report.html").exists());
        assert_eq!(a["schema_version"], "1.2");
    }
}
