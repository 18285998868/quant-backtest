use crate::error::{ConfigError, Result};

const GIB: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_WORKER_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Capacity {
    pub cpu_available: usize,
    pub mem_available_bytes: Option<u64>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedWorkers {
    pub threads: usize,
    pub cpu_available: usize,
    pub memory_available_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub worker_budget_bytes: u64,
    pub warnings: Vec<String>,
}

pub fn peak_rss_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_peak_rss(&text)
}

fn parse_peak_rss(text: &str) -> Option<u64> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("VmHWM:") {
            let value = parts.next()?.parse::<u64>().ok()?;
            return Some(value.saturating_mul(1024));
        }
    }
    None
}

pub fn detect_capacity() -> Capacity {
    let mut notes = Vec::new();
    let mut cpu_available = match std::thread::available_parallelism() {
        Ok(value) => value.get(),
        Err(err) => {
            notes.push(format!("available_parallelism 失败 ({err}),回退 1 线程"));
            1
        }
    };
    if let Some(cgroup_cpu) = detect_cgroup_cpu(&mut notes)
        && cgroup_cpu < cpu_available
    {
        notes.push(format!(
            "cgroup CPU 配额限制为 {cgroup_cpu} 核 (检测到 {cpu_available} 核)"
        ));
        cpu_available = cgroup_cpu;
    }
    let mem_available_bytes = detect_memory(&mut notes);
    Capacity {
        cpu_available: cpu_available.max(1),
        mem_available_bytes,
        notes,
    }
}

pub fn resolve_workers(
    requested: Option<usize>,
    memory_limit_gb: Option<f64>,
    capacity: &Capacity,
    worker_budget_bytes: u64,
) -> Result<ResolvedWorkers> {
    if worker_budget_bytes == 0 {
        return Err(ConfigError::Invalid {
            field: "worker_budget_bytes".into(),
            message: "必须大于 0".into(),
        }
        .into());
    }
    if let Some(value) = requested
        && value == 0
    {
        return Err(ConfigError::Invalid {
            field: "threads".into(),
            message: "必须大于 0".into(),
        }
        .into());
    }
    let mut warnings = Vec::new();
    let cpu_available = capacity.cpu_available.max(1);
    let memory_limit_bytes = match memory_limit_gb {
        Some(value) if value > 0.0 => {
            if !value.is_finite() {
                return Err(ConfigError::Invalid {
                    field: "memory_limit_gb".into(),
                    message: "必须是有限数值".into(),
                }
                .into());
            }
            let bytes = value * GIB as f64;
            if bytes > u64::MAX as f64 {
                return Err(ConfigError::Invalid {
                    field: "memory_limit_gb".into(),
                    message: "超出可表示范围".into(),
                }
                .into());
            }
            Some(bytes.round() as u64)
        }
        _ => capacity.mem_available_bytes,
    };
    let reserve = memory_limit_bytes
        .map(|bytes| (bytes / 10).max(2 * GIB))
        .unwrap_or(0);
    let usable = memory_limit_bytes.map(|bytes| bytes.saturating_sub(reserve));
    if let Some(usable) = usable
        && usable < worker_budget_bytes
    {
        return Err(ConfigError::Invalid {
            field: "memory_limit_gb / memory".into(),
            message: format!(
                "可用内存 {:.2} GiB (已预留 {:.2} GiB),小于单 worker 预算 {:.0} MiB",
                usable as f64 / GIB as f64,
                reserve as f64 / GIB as f64,
                worker_budget_bytes as f64 / (1024.0 * 1024.0)
            ),
        }
        .into());
    }
    if memory_limit_bytes.is_none() {
        warnings.push("无法检测内存上限,只按 CPU 决定线程数".into());
    }
    let memory_threads = usable.map(|value| (value / worker_budget_bytes).max(1) as usize);
    let auto_threads = memory_threads
        .map(|value| value.min(cpu_available))
        .unwrap_or(cpu_available)
        .max(1);
    let threads = match requested {
        Some(value) => {
            if value > cpu_available {
                warnings.push(format!(
                    "显式线程数 {value} 超过检测到的 {cpu_available} 核,可能存在超订"
                ));
            }
            if let Some(usable) = usable {
                let wanted = (value as u64).saturating_mul(worker_budget_bytes);
                if wanted > usable {
                    warnings.push(format!(
                        "显式线程数 {value} 按单 worker 预算需要 {:.2} GiB,超过可用 {:.2} GiB",
                        wanted as f64 / GIB as f64,
                        usable as f64 / GIB as f64
                    ));
                }
            }
            value
        }
        None => auto_threads,
    };
    Ok(ResolvedWorkers {
        threads,
        cpu_available,
        memory_available_bytes: capacity.mem_available_bytes,
        memory_limit_bytes,
        worker_budget_bytes,
        warnings,
    })
}

fn detect_cgroup_cpu(notes: &mut Vec<String>) -> Option<usize> {
    if let Ok(text) = std::fs::read_to_string("/sys/fs/cgroup/cpu.max")
        && let Some(value) = parse_cpu_max(&text)
    {
        return Some(value);
    }
    let quota = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    let period = std::fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    if let (Some(quota), Some(period)) = (quota, period)
        && quota > 0
        && period > 0
    {
        return Some(((quota + period - 1) / period).max(1) as usize);
    }
    notes.push("未检测到 cgroup CPU 配额".into());
    None
}

fn parse_cpu_max(text: &str) -> Option<usize> {
    let mut parts = text.split_whitespace();
    let quota = parts.next()?;
    let period = parts.next()?.parse::<u64>().ok()?;
    if quota == "max" || period == 0 {
        return None;
    }
    let quota = quota.parse::<u64>().ok()?;
    if quota == 0 {
        return None;
    }
    Some(quota.div_ceil(period) as usize)
}

fn detect_memory(notes: &mut Vec<String>) -> Option<u64> {
    let mut candidates = Vec::new();
    if let Ok(text) = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        && let Some(value) = parse_memory_limit(&text)
    {
        candidates.push(("cgroup v2 memory.max", value));
    }
    if let Ok(text) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        && let Some(value) = parse_memory_limit(&text)
    {
        candidates.push(("cgroup v1 memory.limit_in_bytes", value));
    }
    if let Some(value) = read_meminfo_available() {
        candidates.push(("/proc/meminfo MemAvailable", value));
    }
    if candidates.is_empty() {
        notes.push("未检测到可用内存上限".into());
        return None;
    }
    let (name, value) = candidates
        .into_iter()
        .min_by_key(|(_, bytes)| *bytes)
        .unwrap_or(("unknown", 0));
    notes.push(format!(
        "内存上限取自 {name}: {:.2} GiB",
        value as f64 / GIB as f64
    ));
    Some(value)
}

fn parse_memory_limit(text: &str) -> Option<u64> {
    let text = text.trim();
    if text == "max" {
        return None;
    }
    let value = text.parse::<u64>().ok()?;
    if value >= 1 << 60 {
        return None;
    }
    Some(value)
}

fn read_meminfo_available() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_available(&text)
}

fn parse_meminfo_available(text: &str) -> Option<u64> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("MemAvailable:") {
            let value = parts.next()?.parse::<u64>().ok()?;
            return Some(value.saturating_mul(1024));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(cpu: usize, mem_gib: u64) -> Capacity {
        Capacity {
            cpu_available: cpu,
            mem_available_bytes: Some(mem_gib * GIB),
            notes: Vec::new(),
        }
    }

    #[test]
    fn auto_uses_cpu_when_memory_is_ample() {
        let resolved = resolve_workers(None, None, &capacity(2, 8), DEFAULT_WORKER_BUDGET_BYTES)
            .expect("resolve");
        assert_eq!(resolved.threads, 2);
    }

    #[test]
    fn explicit_threads_override_auto() {
        let resolved = resolve_workers(Some(1), None, &capacity(2, 8), DEFAULT_WORKER_BUDGET_BYTES)
            .expect("resolve");
        assert_eq!(resolved.threads, 1);
    }

    #[test]
    fn memory_budget_limits_workers() {
        let resolved =
            resolve_workers(None, None, &capacity(32, 3), 1024 * 1024 * 1024).expect("resolve");
        assert_eq!(resolved.threads, 1);
    }

    #[test]
    fn too_little_memory_is_a_config_error() {
        let mut cap = capacity(8, 2);
        cap.mem_available_bytes = Some(GIB);
        let error =
            resolve_workers(None, None, &cap, DEFAULT_WORKER_BUDGET_BYTES).expect_err("must fail");
        assert!(error.to_string().contains("单 worker 预算"));
    }

    #[test]
    fn missing_memory_falls_back_to_cpu_with_warning() {
        let cap = Capacity {
            cpu_available: 2,
            mem_available_bytes: None,
            notes: Vec::new(),
        };
        let resolved =
            resolve_workers(None, None, &cap, DEFAULT_WORKER_BUDGET_BYTES).expect("resolve");
        assert_eq!(resolved.threads, 2);
        assert!(!resolved.warnings.is_empty());
    }

    #[test]
    fn parses_cgroup_cpu_max() {
        assert_eq!(parse_cpu_max("max 100000\n"), None);
        assert_eq!(parse_cpu_max("200000 100000\n"), Some(2));
        assert_eq!(parse_cpu_max("150000 100000\n"), Some(2));
        assert_eq!(parse_cpu_max("50000 100000\n"), Some(1));
    }

    #[test]
    fn parses_memory_limits() {
        assert_eq!(parse_memory_limit("max\n"), None);
        assert_eq!(parse_memory_limit("1073741824\n"), Some(GIB));
        assert_eq!(parse_memory_limit("9223372036854771712"), None);
    }

    #[test]
    fn parses_meminfo_available() {
        let text = "MemTotal:       16000000 kB\nMemAvailable:    8000000 kB\n";
        assert_eq!(parse_meminfo_available(text), Some(8_000_000 * 1024));
    }

    #[test]
    fn parses_peak_rss() {
        let text = "Name:\tquant-backtest\nVmHWM:\t  2048 kB\n";
        assert_eq!(parse_peak_rss(text), Some(2048 * 1024));
    }
}
