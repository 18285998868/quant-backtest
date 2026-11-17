# 015 统计分析模块设计草案

日期:2026-11-17
状态:M11-C、M11-A 与 M11-D 已实现并验收;M11-B/E 待推进
上游:[需求规格](../01-需求规格.md)、[调研与决策](../02-调研与决策.md)、[流程设计](../03-流程设计.md)
相关:[014 多标的收盘价标记](./014-multi-symbol-marking.md)

## 1. 背景

M9 之后,引擎的吞吐已不是主要瓶颈:500 标的、4 线程约 478M bar 步/秒(空策略)、449M(SmaCross);W4(10 标的、1m、10 组参数)端到端中位 13.34 秒;186 个测试通过。下一步的可信度缺口集中在成本与统计两处,本文只处理统计部分。

当前已有的防过拟合工具:

- 70/30 样本外切分与 OOS 指标;
- walk-forward(rolling/anchored、embargo、warmup、OOS 复利拼接);
- DSR(Deflated Sharpe Ratio),只校正搜索次数;
- 参数边际效应与两参数热力图。

仍缺:

- PBO/CSCV:回测过拟合概率;
- SPA/Reality Check:策略族相对基准的显著性;
- Bootstrap 置信区间:Sharpe/CAGR/最大回撤等指标的不确定性;
- 参数平台:区分孤立尖峰与稳健区域;
- 试验登记:把非法、失败、跳过的试验也计入搜索次数。

一次 1m 实验说明了缺口的现实影响。10 个 Binance 现货标的、2023-05 至 2026-11、SMA(4–12, 24–48)、初始资金 1,000,000、`weight=0.09`、`top_k=2`、`oos_split=0.7`:

| 成本档 | 选中参数 | 全期收益 | 样本内收益 | 样本外收益 | 样本内 DSR | 换手/年 | 成本拖累/年 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 0x | 4/24 | -8.1% | +112.6% | -57.1% | 0.852 | 29,603 | 0 |
| 1x | 12/48 | -100% | -100% | -100% | 0 | 13,281 | 664% |
| 4x | 12/48 | -100% | -100% | -100% | 0 | 13,605 | 2,721% |

0x 的 +112.6% 是样本内最优组,不是全期;全期是 -8.1%。三档的 `data_hash` 都是 `fdb449ba...`,扫描哈希分别为 `6afb563b...`、`4a3f718e...`、`152ce65c...`。这说明两个独立问题:

- 成本档改变最优参数,1x 下没有任何参数存活;报告需要给出成本生存边界。
- 样本内 DSR 0.852,样本外 -57.1%;DSR 校正了搜索次数,但没有校正选择偏差和状态漂移。

## 2. 目标与非目标

目标:

- 在 `backtest-core` 增加纯统计内核,输入收益矩阵与参数网格,输出可序列化的诊断结果。
- 把 PBO/CSCV、SPA、bootstrap、plateau、trial registry 接入 `sweep` 与 `walkforward` 的编排。
- 结果确定性可复现:固定种子、并行顺序无关、独立 `analysis_hash`。
- 默认关闭;关闭时 `config_hash`、`result_hash`、`sweep_hash` 逐位不变。
- 资产无关:加密、美股、外汇、后续多资产组合共用同一套接口。

非目标:

- 不改变成交、账本、保证金、成本语义;这些属于 M10。
- 不新增基准数据源、公司行动、多币种;SPA 的基准先由用户提供,后续接 M10-D。
- 不新增独立 crate、独立服务或独立进程。
- 不重排报告章节;诊断放在第 9 章“样本外与稳健性”。
- 不承诺统计工具能修复经济假设错误;它只回答“选择偏差有多大、结果是否显著”。

## 3. 设计原则

1. 纯内核进 core:只接受切片、矩阵和配置,不读文件、不解析 TOML、不生成 HTML、不碰引擎状态。
2. 编排进 CLI:构建收益矩阵、调用内核、排序、序列化、落盘。
3. 展示进 report:只渲染 `AnalysisReport`,不计算统计量。
4. 输入资产无关:用日收益矩阵,不用 `EquityPoint`、`InstrumentSpec`、`RunConfig`。
5. 默认关闭:统计可能很重,不能拖慢普通回测与扫描。
6. 口径透明:CSCV 近似、SPA 基准依赖、样本量下限都写进报告和文档,不用单一指标假装结论。

已定决策:

1. 统计内核放 `backtest-core::analysis`,暂不独立 crate;抽取条件见第 4 节。
2. bootstrap 用自带的 SplitMix64,不引入 `rand` 运行时依赖。
3. `[analysis]` 默认关闭,分析结果用独立 `analysis_hash`;基础回测哈希不变。
4. 首版顺序为 bootstrap → PBO → plateau → SPA → trial registry;SPA 的基准输入可以后接 M10-D。

## 4. 架构与模块边界

| 层 | 位置 | 职责 | 不负责 |
| --- | --- | --- | --- |
| 统计内核 | `crates/backtest-core/src/analysis/` | bootstrap、PBO、SPA、plateau、RNG | I/O、配置解析、报告、引擎 |
| 编排 | `crates/backtest-cli/src/sweep.rs`、`walkforward.rs` | 采集收益矩阵、调用内核、排序、序列化 | 统计公式 |
| 展示 | `crates/backtest-report` 第 9 章 | 渲染子节、告警 | 统计计算 |
| 配置 | `crates/backtest-core/src/config.rs` | `[analysis]` 解析、默认值、校验 | 统计实现 |

模块布局:

```text
crates/backtest-core/src/analysis/
  mod.rs        // TrialReturns、GridMatrix、AnalysisReport、错误
  rng.rs        // SplitMix64、种子派生、无偏采样
  bootstrap.rs  // stationary/circular block bootstrap、置信区间
  pbo.rs        // CSCV/PBO
  spa.rs        // White/Hansen SPA
  plateau.rs    // 参数平台得分
```

依赖方向:`backtest-core` 不依赖 `analysis` 之外的 crate;`backtest-cli` 依赖 `backtest-core`;`backtest-report` 依赖 `backtest-core`。统计内核不依赖 `backtest-data`、`backtest-strategy`、`backtest-report`。

为什么先放 core,不独立 crate:

- core 已有 `metrics::deflated_sharpe`、`normal_cdf`、`normal_ppf` 与 `walkforward` 纯函数;PBO/SPA/bootstrap 需要复用这些数值工具。独立 crate 要么反向依赖 core,要么复制数值代码。
- CLI 的模块注释明确写“只做参数解析、配置/数据装载、引擎调用与报告落盘;领域逻辑全部在 core/data/strategy/report”。把统计公式放 CLI 会破坏这个边界。
- report 只应渲染;在 report 里计算会让报告生成变慢,并把统计逻辑和 schema 绑死。
- 当前只有一个消费者(CLI),独立 crate 的收益主要是依赖隔离和多消费者复用;两者都还不成立。

抽成 `backtest-stats` 的触发条件:

- 出现第二个消费者,例如 API、service、GUI;
- 统计依赖明显变重,例如引入矩阵库、并行随机数库;
- `analysis` 超过约 1,500 行或 4 个子模块;
- 外部用户需要不依赖引擎的统计库。

抽取时保持 `TrialReturns`、`GridMatrix`、`AnalysisReport` 接口不变,CLI 只改 import。

独立的 `analyze` 子命令可以后加:它读取可选的紧凑收益矩阵文件,调用同一个 core 内核;不要为它复制一套统计代码。首版先随 `sweep`/`walkforward` 计算,避免矩阵持久化与格式设计。

## 5. 数据契约

### 5.1 TrialReturns

```rust
pub struct TrialReturns {
    pub labels: Vec<String>,
    pub returns: Vec<Vec<f64>>,
    pub periods_per_year: u32,
    pub source: AnalysisSource,
}

pub enum AnalysisSource {
    Sweep,
    WalkForward,
    Run,
}
```

约定:

- `returns[t][n]` 是第 `n` 个试验在第 `t` 个日界的简单收益(外层时间、内层试验),按时间升序。
- 所有行等长且时间对齐;不允许缺失值。长度不足或时间轴不一致的试验直接剔除并告警,不用 `0.0` 填充。
- `labels` 与 `returns` 长度一致,建议用参数 JSON 的规范字符串。
- `periods_per_year` 用于年化 Sharpe/CAGR;加密货币 365,美股 252,外汇 252 或按标的配置。
- 不携带权益、成交、持仓;需要时由编排层从 `EquityPoint` 聚合。

### 5.2 GridMatrix

```rust
pub struct GridMatrix {
    pub axes: Vec<ParamAxis>,
    pub values: Vec<f64>,
}

pub struct ParamAxis {
    pub name: String,
    pub values: Vec<serde_json::Value>,
}
```

约定:

- `values` 按行主序展开,长度等于各轴长度乘积。
- 编排层把缺失目标值转成 `f64::NAN` 再填入矩阵;输出统计 `null` 数量,`NaN` 不参与邻居计数。
- 只支持 2 个轴做平台分析;多于 2 个轴时按边际聚合或跳过,报告标注。
- 目标值与 `SweepTask::objective_value` 同口径,缺失值用 `NaN` 前的 `Option` 表示;矩阵内部用 `f64::NAN` 并在输出中转为 `null` 计数。

### 5.3 AnalysisReport

目标结构(全部阶段完成后):

```rust
pub struct AnalysisReport {
    pub version: u32,
    pub hash: String,
    pub seed: u64,
    pub trials: TrialCounts,
    pub bootstrap: Option<BootstrapReport>,
    pub pbo: Option<PboReport>,
    pub spa: Option<SpaReport>,
    pub plateau: Option<PlateauReport>,
    pub warnings: Vec<String>,
}
```

`TrialCounts`:

```rust
pub struct TrialCounts {
    pub grid_total: usize,
    pub grid_valid: usize,
    pub skipped_invalid: usize,
    pub oos_evaluated: usize,
    pub analysis_trials: usize,
}
```

首版实现只包含 `version`、`hash`、`seed`、`input_hash`、`bootstrap` 与
`warnings`;`trials`、`pbo`、`spa`、`plateau` 在对应阶段加入,旧报告缺字段按
“未运行”处理。`BootstrapStat` 额外带 `valid_samples`,用于区分请求样本数与
实际有效样本数。

### 5.4 配置

```toml
[analysis]
enabled = false
seed = 42
bootstrap_samples = 1000
pbo_blocks = 10
spa_samples = 1000
spa_block_length = 0
plateau_tolerance = 0.10
min_trials = 20
```

字段:

| 字段 | 默认 | 校验 | 说明 |
| --- | --- | --- | --- |
| `enabled` | `false` | 布尔 | 关闭时不采集、不计算、不写报告 |
| `seed` | `42` | `u64` | 主种子;每个试验/样本派生 |
| `bootstrap_samples` | `1000` | `100..=100000` | bootstrap 重采样次数 |
| `pbo_blocks` | `10` | 偶数,`4..=20` | CSCV 时间块数(M11-A) |
| `spa_samples` | `1000` | `100..=10000` | SPA bootstrap 次数(M11-B) |
| `spa_block_length` | `0` | `0` 或正整数 | `0` 表示自动 `round(T^(1/3))`(M11-B) |
| `plateau_tolerance` | `0.10` | `>0` | 目标值容差,按目标值量纲(M11-D) |
| `min_trials` | `20` | `>=2` | 低于此值只告警,不拒绝(后续阶段) |

首版只解析前三个字段;其余字段随对应阶段加入,加入前出现会被
`deny_unknown_fields` 拒绝。`[analysis]` 不参与 `config_hash`。

适用范围:

| 分析 | `run` | `sweep` | `walkforward` |
| --- | --- | --- | --- |
| bootstrap | 是 | 是 | 是 |
| PBO | 否 | 是 | 否 |
| SPA | 否 | 是 | 是(OOS) |
| plateau | 否 | 是 | 否 |
| trial registry | 否 | 是 | 是 |

`run` 的区间估计用 bootstrap。首版只有 `run` 支持 bootstrap;
`sweep`/`walkforward` 在 `enabled = true` 时告警并跳过,等收益矩阵采集接入后
再按上表执行。PBO/SPA/plateau 字段在对应阶段加入。

## 6. 数据流

首版 `run` 不改引擎:`BacktestResult.equity` 已包含每根 bar 的权益,CLI 用
`metrics::equity_returns()` 得到与指标同口径的每期收益,直接交给
`analysis::analyze_run()`。统计口径因此是“每期”,不是日聚合;报告按每期标注。

`sweep`/`walkforward` 需要试验矩阵,才走下面的紧凑采集路径(PBO/SPA 阶段):

当前 `run_grid` 使用 `run_backtest_stats`,只返回指标与逐期 Sharpe,不保留权益曲线。PBO/SPA 需要每个试验的收益序列,不能直接拿现有 `SweepTask`。

建议的紧凑采集路径:

1. `EngineInput`/`BacktestStats` 增加可选 `daily_returns: Option<Vec<f64>>`。
2. `mark_close` 在 UTC 日边界把当日权益收益推入缓冲;默认关闭,不改变现有算术与哈希。
3. `[analysis].enabled = true` 时,`run_grid` 打开采集,把每个试验的日收益放入独立的 `Vec<Vec<f64>>`。
4. `SweepTask` 不序列化收益矩阵;矩阵在编排层按 index 排序,分析完成后丢弃。
5. PBO 把日收益复利成 S 个块收益;SPA 直接使用日收益矩阵;bootstrap 使用选中试验或 OOS 收益;plateau 使用现有目标值矩阵。
6. 分析结果写入 `SweepSummary::analysis`/`WalkForwardResult::analysis`,报告第 9 章渲染。

内存量级:

- N=1000、D=1300、8 字节约 10 MB;N=10,000 约 100 MB。
- 受 `run.memory_limit_gb` 约束;超过预算时跳过分析并告警,不降低回测本身的内存上限。
- 不为每个试验保留 1m 级权益曲线;日收益是统计口径的最小充分数据。

为什么用日收益而不是每根 bar:

- 1m 级 N×T 矩阵不可行;日聚合是 PBO/SPA/bootstrap 的常规口径。
- 日收益仍能捕捉策略边缘;如果策略只在日内有效,报告需要明确“统计基于日聚合”。
- 日界按 UTC;多日历资产后续在数据层对齐,统计层只接收已对齐的收益矩阵。

## 7. 算法

### 7.1 RNG:SplitMix64

选择理由:无新依赖、状态小、跨平台确定、适合重采样;不是密码学随机数。

```text
state = state + 0x9E3779B97F4A7C15
z = state
z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9
z = (z ^ (z >> 27)) * 0x94D049BB133111EB
return z ^ (z >> 31)
```

约定:

- `next_u64()` 返回上述 64 位值。
- `next_f64()` 取高 53 位除以 `2^53`,返回 `[0,1)`。
- `below(n)` 用拒绝采样消除模偏差;`n=0` 视为错误。
- 种子派生:`seed_for(master, domain, index) = first_u64(SHA256(master_le || domain || index_le))`。`domain` 是稳定字符串,例如 `"bootstrap"`、`"pbo"`、`"spa"`。
- 每个试验、每个 bootstrap 样本使用独立派生子种子,避免 rayon 调度影响结果。
- 测试用公开的 SplitMix64 参考向量,并验证 `next_f64` 范围与 `below` 的基本均匀性。

### 7.2 Bootstrap

采用 stationary bootstrap(Politis & Romano):

- 输入:`returns: &[f64]`,期望块长 `L`,`samples`。
- `L = 0` 时自动取 `max(1, round(T^(1/3)))`;首版不暴露块长配置,SPA 阶段再接
  `spa_block_length`。
- 以概率 `1/L` 重新开始一个新块,否则延续当前块;块首在 `0..T` 均匀抽取。
- 每个样本计算统计量;输出百分位置信区间 `[q(α/2), q(1-α/2)]`,默认 95%。

统计量:

| 名称 | 定义 | 备注 |
| --- | --- | --- |
| `mean` | 算术平均日收益 | 年化由报告层决定 |
| `volatility` | 日收益样本标准差 | `ddof=1` |
| `sharpe` | `mean / std * sqrt(periods_per_year)` | `std=0` 时 `null` |
| `cagr` | 复利路径年化 | 需要 `periods_per_year` |
| `max_drawdown` | 复利路径最大回撤 | 正数表示回撤幅度 |

输出:

```rust
pub struct BootstrapReport {
    pub samples: usize,
    pub block_length: usize,
    pub confidence: f64,
    pub statistics: Vec<BootstrapStat>,
}

pub struct BootstrapStat {
    pub name: BootstrapStatistic,
    pub point: Option<f64>,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub valid_samples: usize,
}
```

边界:

- `T < 2`、全零收益、非有限值:返回 `null` 并告警。
- 首版只做 percentile CI;BCa 留作后续。
- Bootstrap 不修复选择偏差;报告必须和 PBO/SPA 一起看。

### 7.3 PBO/CSCV

输入:`TrialReturns`、`pbo_blocks = S`(偶数)。

步骤:

1. 把 `T` 个日收益按时间切成 `S` 个连续块,块长尽量相等。
2. 枚举 `S/2` 个块作为样本内(IS),其余为样本外(OOS);共 `C(S, S/2)` 个组合。`S=10` 时 252 个。
3. 对每个组合,在每个试验上计算 IS 目标值(默认 Sharpe),选 IS 最优试验 `n*`。
4. 计算 `n*` 在全部试验中的 OOS 排名 `r`(`1` 最差,`N` 最好)。
5. 相对排名 `ω = r / (N + 1)`,logit `φ = ln(ω / (1 - ω))`。
6. `PBO = count(φ <= 0) / 组合数`。

输出:

```rust
pub struct PboReport {
    pub pbo: f64,
    pub splits: usize,
    pub blocks: usize,
    pub trials: usize,
    pub logit_mean: f64,
    pub logit_p05: f64,
    pub logit_p50: f64,
    pub logit_p95: f64,
    pub best_label: Option<String>,
    pub warnings: Vec<String>,
}
```

口径与边界:

- 默认目标 Sharpe;`objective` 可扩展为 `return`/`calmar`,但首版只做 Sharpe。
- 并列时取试验 index 较小者,保证确定性。
- CSCV 假设时间块可交换;止损、共享账户、路径依赖策略只能近似。报告必须写“CSCV 近似”。
- `N < min_trials` 时只告警;`N < 10` 时 PBO 噪声很大,报告明确“样本不足”。
- `S` 必须为偶数,且 `S <= T`(每块至少 1 个收益点);否则配置错误。

### 7.4 SPA / Reality Check

首版实现 studentized、去中心化的 stationary bootstrap;这是 Hansen SPA 的近似,不做 Hansen 的 `A_i` 截断。报告写明 variant,避免把近似当成完整 SPA。

- 输入:`TrialReturns`、基准日收益 `benchmark: &[f64]`、`spa_samples`、块长。
- `d_i,t = r_i,t - benchmark_t`。
- 统计量 `T = max_i sqrt(T) * mean(d_i) / sd(d_i)`;`sd=0` 的试验跳过。
- 零假设:策略族中最好的也不优于基准。
- Stationary bootstrap 重采样 `d_i,t`;为施加零假设,先对每个试验去中心化 `d_i,t - mean(d_i)`,再计算 `T*`。
- `p = (1 + count(T* >= T)) / (samples + 1)`。
- 输出 `p_value`、最优试验、统计量、块长、样本数、variant、告警。

输出:

```rust
pub struct SpaReport {
    pub p_value: f64,
    pub statistic: f64,
    pub trials: usize,
    pub samples: usize,
    pub block_length: usize,
    pub best_label: Option<String>,
    pub benchmark: String,
    pub variant: String,
    pub warnings: Vec<String>,
}
```

边界:

- 没有基准时跳过 SPA 并告警;不要用一个随便的资产冒充基准。
- 多资产下基准按运行/组合给定;后续接 M10-D。
- `N < min_trials` 告警;SPA 的检验功效依赖试验数和样本长度。
- 首版不做 stepwise SPA;只给整体 p 值。

### 7.5 Plateau

输入:`GridMatrix`、`tolerance`。

步骤:

1. 只处理 2 个参数轴;多于 2 轴时按边际聚合或跳过。
2. 每个格点的邻居取上下左右 4 个(不取对角,避免参数尺度不一致)。
3. `score(i,j) = count(邻居目标值 >= 当前目标值 - tolerance) / 邻居数`。
4. `best` = 目标值最大的格点;`robust` = `score >= 0.75` 的格点中目标值最大者。
5. 若 `best` 的 `score < 0.5`,标记“孤立尖峰”。

输出:

```rust
pub struct PlateauReport {
    pub best: GridPoint,
    pub robust: Option<GridPoint>,
    pub isolated: bool,
    pub scores: Vec<f64>,
    pub warnings: Vec<String>,
}
```

边界:

- 目标值缺失/非有限的格点不参与邻居计数。
- 边界格点邻居数较少,按实际邻居数归一。
- `tolerance` 按目标值量纲;Sharpe 场景建议 `0.10` 到 `0.25`。

### 7.6 Trial registry

- 记录 `grid_total`、`grid_valid`、`skipped_invalid`、`oos_evaluated`、`analysis_trials`。
- `trial_hash` = 规范 JSON 的 SHA-256,内容为按 index 排序的 `(params, config_hash)`。
- DSR/PBO/SPA 的试验数以此为准,避免只统计成功试验。
- walk-forward 的试验数 = 网格大小 × 实际运行窗口数;跳过的窗口单列。

## 8. 哈希与 schema

### 8.1 哈希

- `[analysis].enabled = false`:不采集、不计算、不新增字段;`config_hash`、`result_hash`、`sweep_hash` 与报告 schema 保持不变。
- `analysis.hash` 独立计算:`SHA256(canonical_json({version, seed, input_hash, bootstrap, warnings}))`;计算时 `hash` 字段置空,避免自引用。
- `input_hash` 覆盖收益序列的规范 f64 位模式、序列长度与 `periods_per_year`;不覆盖文件名、路径或 `[analysis]` 配置。
- 基础 `sweep_hash`/`result_hash` 不吸收 `analysis`;报告的内容哈希吸收 `analysis`。
- 所有浮点输出先规范化:`-0.0` 变 `0.0`,`NaN`/`Inf` 转 `null` 并告警。
- 并行结果按样本/试验 index 排序后再哈希,保证线程数变化不影响 `analysis_hash`。

### 8.2 schema

- 报告 schema:启用分析时为 1.3,顶层增加 `analysis`;关闭时保持 1.2 且不写
  `analysis`。第 9 章只在启用时增加 bootstrap 子节。
- PBO/SPA/plateau 接入后,扫描报告再升 1.3:`sweep_summary.json` 顶层增加可选
  `analysis`(M11-A/B/D)。
- 旧报告 1.2/扫描 1.1 仍然可读;新字段缺失时按“未运行”处理。
- `analysis` 字段缺失或 `null` 表示未启用;“启用了但没有结果”用 `warnings` 说明。

### 8.3 金样

- 固定 SmaCross 配置、XsMomentum 配置、sweep 配置、walkforward 配置各一份。
- 关闭 `[analysis]` 时,与 M9 二进制的 `result_hash`/`sweep_hash` 逐位一致。
- 开启后,同一配置在 1/2/4/8 线程下 `analysis_hash` 一致。
- 改变 `seed` 只改变随机重采样结果,不改变基础回测哈希。

## 9. 确定性、并行与性能

确定性:

- 主种子 + domain + index 派生;不共享可变 RNG。
- 所有试验/样本的输出按 index 排序后再汇总。
- 浮点归约顺序固定;bootstrap 样本按生成顺序累加。

并行:

- PBO 的 252 个组合可并行;每个组合只读矩阵。
- SPA 的 bootstrap 样本按 block 分片并行;每个样本独立种子。
- Bootstrap 的统计量按样本并行。
- Plateau 按格点并行。
- 嵌套 rayon 要避免:分析阶段使用独立线程池或 `pool.install`,不与回测的窗口内并行动作嵌套。

复杂度与预算:

| 分析 | 复杂度 | 典型预算 |
| --- | --- | --- |
| Bootstrap | `O(B * T)` | N=1000 trials 时只跑选中序列,B=1000,T=1300,约 1.3M 次 |
| PBO | `O(C(S,S/2) * N * S)` | S=10,N=1000,约 2.5M 次 |
| SPA | `O(B * N * T)` | B=1000,N=1000,T=1300,约 1.3B 次,需要分片和上限 |
| Plateau | `O(cells * 4)` | 网格规模级 |

性能门禁:

- 关闭分析时,500 标的合成基准回归不超过 5%。
- 日收益采集只在 `enabled=true` 时发生;不得改变 `mark_close` 的现有算术顺序。
- SPA 超过时间预算时只减少 `spa_samples`;若必须只跑 top-K,报告把结果标为 `top_k_spa`,不能与全量 SPA 混用。降级要写告警,不能静默。
- 分析阶段不进入回测热路径;报告生成只读结果。

## 10. 报告

第 9 章“样本外与稳健性”增加子节。M11-C 先落地 Bootstrap 子节,其余子节随
PBO/SPA/plateau/registry 阶段加入:

1. 过拟合概率 PBO
   - PBO、组合数、块数、logit 均值/分位、最优试验标签。
   - `PBO > 0.5` 时红色告警;`N < min_trials` 时灰色提示。
2. SPA 显著性
   - p 值、统计量、基准名称、最优试验。
   - `p > 0.05` 时明确写“不能证明有超额收益”。
3. Bootstrap 置信区间
   - Sharpe/CAGR/最大回撤/波动率的点估计与 95% 区间。
4. 参数平台
   - 最优点、稳健点、孤立尖峰标记;热力图叠加平台得分。
5. 试验登记
   - 总组合、有效、非法、OOS 评估、分析试验数、`trial_hash`。

不新增章节,不改变前 8 章顺序;报告仍是单文件离线 HTML。

## 11. 测试与验收

| 编号 | 验收 | 方法 |
| --- | --- | --- |
| AC-M11-1 | SplitMix64 参考向量一致,`next_f64` 在 `[0,1)`,`below` 无模偏差 | 单元测试 |
| AC-M11-2 | 正态收益的 95% bootstrap CI 覆盖率约 95% | 1000 次重复模拟 |
| AC-M11-3 | 纯随机策略族 PBO 约 0.5 | 合成矩阵,固定种子 |
| AC-M11-4 | 有明显真实边缘的策略族 PBO 显著低于 0.5 | 合成矩阵 |
| AC-M11-5 | 全部策略等于基准时 SPA p 值接近 1 | 合成矩阵 |
| AC-M11-6 | 单一强策略时 SPA p 值很小 | 合成矩阵 |
| AC-M11-7 | 平坦目标面 plateau 得分接近 1,孤立尖峰接近 0 | 手算网格 |
| AC-M11-8 | 试验登记与网格计数一致,非法/跳过不丢失 | 扫描夹具 |
| AC-M11-9 | 关闭分析时历史 `result_hash`/`sweep_hash` 不变 | 金样回归 |
| AC-M11-10 | 1/2/4/8 线程 `analysis_hash` 一致 | 并行回归 |
| AC-M11-11 | 报告 schema 1.3 通过校验,旧 1.2 仍可读 | schema 快照 |
| AC-M11-12 | 关闭分析时 500 标的基准回归不超过 5% | 合成基准 |

M11-C 验收(2026-11-17):

- 208 个测试、`cargo fmt --check` 与 `cargo clippy -D warnings` 通过。
- AC-M11-1/2 由 `rng` 与 `bootstrap` 单测覆盖;AC-M11-9/10/11 由配置测试、
  报告校验测试与冒烟覆盖。
- 冒烟(`fixtures/s5_daily.csv`,400 根 1d bar,500 次重采样):schema 1.3,
  `result_hash=56fef2bc...`,`analysis_hash=c1218836...`;关闭分析后 schema 1.2
  且 `result_hash` 逐位相同;1 线程与 2 线程的 `analysis_hash` 一致。
- AC-M11-12 未单独跑基准;关闭分析时不进入引擎热路径,留到 M11-A 采集合入时
  一并回归。

M11-A 验收(2026-11-17):

- 218 个测试(含 PBO 与日收益聚合单测)、`cargo fmt --check`、
  `cargo clippy -D warnings` 与 `cargo doc` 通过;`scripts/check.sh --ci` 除
  `cargo deny`(网络不可达,环境问题)外全部通过。
- AC-M11-3/4 由 `pbo` 单测覆盖:40 个随机试验 PBO 落在 `[0.2,0.8]`,
  加入真实边缘的试验 PBO `<0.25` 且 `best_label` 指向该试验。
- AC-M11-9/10/11 由配置测试与冒烟覆盖:`sweep` 关闭分析 schema 1.1,
  开启 schema 1.2;`sweep_hash` 与关闭时逐位相同;1 线程与 2 线程
  `analysis_hash` 一致;报告 schema 校验通过,旧 1.1/1.2 仍可读。
- 冒烟(`fixtures/s5_daily.csv`,16 组参数):关闭分析 schema 1.1、PBO 关闭;
  开启分析 schema 1.2,`pbo=0.0`,16 个试验,252 个组合;1 线程与 2 线程
  `analysis_hash` 一致;报告写出 PBO 子节与“CSCV 近似”注记。
- `min_trials` 告警路径与 `pbo_blocks` 超过观测数的跳过路径均有单测覆盖。

M11-D 验收(2026-11-17):

- 143 个 core 测试(含 plateau 单测)、fmt、clippy、cargo doc 与 release 构建通过。
- AC-M11-7 由 `plateau` 单测覆盖:平坦高值面所有格点得分 1.0 且 `isolated=false`;
  孤立尖峰 `isolated=true`;稳健点选取得分 >=0.75 中目标值最大者。
- 冒烟(`fixtures/s5_daily.csv`,16 组参数):开启分析时 `analysis.plateau` 非空,
  报告写出“参数平台”子节,含 best/robust/isolated 与得分矩阵;`analysis_hash`
  与 1 线程/2 线程一致。
- `[analysis].plateau_tolerance` 默认 0.10,必须为正有限数;非正数配置被拒绝。

PBO 的路径依赖夹具:

- 用带止损的 SmaCross 生成收益,确认报告写出“CSCV 近似”告警。
- 不把近似结果当成精确证明;测试只验证不崩溃、口径标注、确定性。

## 12. 分阶段

| 阶段 | 内容 | 依赖 | 粗估 |
| --- | --- | --- | --- |
| M11-C | SplitMix64、stationary bootstrap、CI、报告子节 | 无 | 2–3 人日 |
| M11-A | CSCV/PBO、试验矩阵采集、报告子节 | M11-C 的收益矩阵契约 | 3–5 人日 |
| M11-D | plateau 得分、稳健点、热力图叠加 | 现有网格/热力图 | 2–3 人日 |
| M11-B | SPA、基准输入、降级策略 | M10-D 或用户基准 | 2–4 人日 |
| M11-E | trial registry、`trial_hash`、DSR/PBO 计数统一 | M11-A | 1 人日 |

建议顺序:M11-C → M11-A → M11-D → M11-B → M11-E。理由:bootstrap 是 SPA 的基础;PBO 直接解释 0x 实验的现象;plateau 最便宜;SPA 最重且依赖基准;registry 与 PBO 一起收口。

M11-C 已于 2026-11-17 完成并验收;其余阶段未开始。

每个阶段都要:

- 代码、单元测试、文档变更说明、`CHANGELOG.md` 记录;
- 关闭分析时哈希回归;
- 更新 `docs/03-流程设计.md` 的模块表与契约;
- 不在同一次改动里混入 M10 的账本语义。

## 13. 实现记录(M11-C)

实际落地与草案的差异:

| 项 | 草案 | 实际 |
| --- | --- | --- |
| 输入 | 日收益矩阵 | `run` 用每根 bar 的权益收益;矩阵留给 PBO/SPA |
| 配置 | 8 个字段 | 只解析 `enabled`/`seed`/`bootstrap_samples` |
| 引擎改动 | `BacktestStats.daily_returns` | 无;直接复用 `BacktestResult.equity` |
| 分析入口 | `SweepSummary::analysis` | `ReportExtras.analysis`,仅 `run` |
| schema | 固定 1.3 | 启用 1.3,关闭 1.2 |
| 统计量 | 5 项 | 5 项 + `valid_samples` |

代码位置(M11-C):

- `crates/backtest-core/src/analysis/{mod,rng,bootstrap}.rs`
- `crates/backtest-cli/src/lib.rs` 的 `cmd_run` 与 `print_result`
- `crates/backtest-report/src/lib.rs` 的 `robustness_section` 与
  `validate_report_json`
- `crates/backtest-core/src/config.rs` 的 `AnalysisConfig`

代码位置(M11-A):

- `crates/backtest-core/src/analysis/pbo.rs`
- `crates/backtest-core/src/analysis/mod.rs` 的 `analyze_sweep` 与
  `SweepAnalysisInput`
- `crates/backtest-core/src/metrics/mod.rs` 的 `daily_equity_returns`
- `crates/backtest-core/src/engine/mod.rs` 的 `collect_daily_returns`
- `crates/backtest-cli/src/sweep.rs` 的试验矩阵采集与 PBO 编排
- `crates/backtest-report/src/lib.rs` 的 `pbo_block` 与 schema 校验

代码位置(M11-D):

- `crates/backtest-core/src/analysis/plateau.rs`
- `crates/backtest-cli/src/sweep.rs` 的平台矩阵采集与 `analysis.plateau` 编排
- `crates/backtest-report/src/lib.rs` 的 `plateau_block` 与 schema 校验

未做:SPA、trial registry、`analyze` 子命令与收益矩阵持久化。

## 13.1 实现记录(M11-A)

实际落地与草案的差异:

| 项 | 草案 | 实际 |
| --- | --- | --- |
| 收益口径 | 日收益矩阵 | sweep 用 `daily_equity_returns` 聚合 bar 权益到 UTC 日收益;`run` 仍用每根 bar |
| 采集路径 | 引擎加 `daily_returns` | `EngineInput.collect_daily_returns` 开关,关闭时零分配 |
| 试验矩阵 | 全部 trial | 全部网格 trial(样本内区段),top 100 的 OOS 额外跑法不变 |
| PBO 组合数 | 全组合 | CSCV 的 `C(S,S/2)`,默认 252,超过 10000 组合告警 |
| 配置 | 8 个字段 | 解析 `enabled`/`seed`/`bootstrap_samples`/`pbo_blocks`/`min_trials` |
| schema | 1.3 | sweep 启用 1.2,关闭 1.1;`analysis` 可选 |
| `sweep_hash` | 可能吸收 analysis | 基础哈希固定 1.1 口径,不吸收 analysis |
| walk-forward | 支持分析 | 暂不支持,`enabled=true` 时告警 |

代码位置见上文 M11-A 代码位置;`walkforward` 仍调用 `warn_analysis_unsupported`。


## 14. 开放问题

1. PBO 的默认目标是否只用 Sharpe?是否允许 `return`/`calmar`?首版建议只做 Sharpe,避免多重口径。
2. 1m 策略用日收益做统计是否足够?如果策略是日内平仓,日收益仍然有效,但报告要标注聚合口径。
3. SPA 的基准来源:先用 `[analysis].benchmark` 的 CSV 路径,还是等 M10-D 的 `benchmark` 配置?建议先支持用户 CSV,后续统一。
4. `N < 20` 时 PBO/SPA 是告警还是跳过?建议告警并输出,但不让报告把它当成结论。
5. 收益矩阵是否持久化?首版不持久化;如果未来要 `analyze` 后处理,再设计紧凑二进制/CSV 格式。
6. walk-forward 是否需要窗口级 PBO?首版只做 OOS bootstrap/SPA;窗口级排名稳定性已有 `param_stability`,不重复造 CSCV。
7. 是否把 DSR 迁到 `analysis`?首版保留在 `metrics`,避免破坏现有调用与哈希;PBO/SPA 只新增。
8. 何时抽 `backtest-stats`?按第 4 节的触发条件,不在首版提前拆。
