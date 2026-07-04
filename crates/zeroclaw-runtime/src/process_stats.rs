//! Self-process resource sampling — RSS (resident memory) and CPU%.
//!
//! Linux-only via `/proc/self/{status,stat}` so no extra deps. macOS /
//! Windows return `ProcessStats::unsupported()` (rss=0, cpu=None); the
//! dashboard renders the rss tile blank-with-note on those platforms.
//!
//! CPU% is computed across calls by stashing the previous (wall_instant,
//! process_ticks) sample in a process-global `OnceLock<Mutex<...>>` and
//! taking the delta. First call returns `cpu_percent = None` since
//! there's no baseline yet; the first refresh after gateway boot fills
//! it in.

#[cfg(target_os = "linux")]
use parking_lot::Mutex;
use serde::Serialize;
#[cfg(target_os = "linux")]
use std::sync::OnceLock;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct ProcessStats {
    /// Resident set size in bytes. `0` when unsupported.
    pub rss_bytes: u64,
    /// Total system RAM in bytes, from `/proc/meminfo`'s `MemTotal`.
    /// `0` when unsupported. The dashboard renders `rss / system_ram_total`
    /// as a percentage so the RAM tile is meaningful at a glance regardless
    /// of host size.
    pub system_ram_total_bytes: u64,
    /// CPU usage as a percentage averaged across logical cores (0..100*ncpu).
    /// `None` on the first sample (no baseline) or unsupported platforms.
    pub cpu_percent: Option<f32>,
    /// Number of logical CPUs the OS reports. Useful for clamping the
    /// CPU% bar on the dashboard. `0` when unknown.
    pub num_cpus: u32,
}

impl ProcessStats {
    fn unsupported() -> Self {
        Self {
            rss_bytes: 0,
            system_ram_total_bytes: 0,
            cpu_percent: None,
            num_cpus: 0,
        }
    }
}

#[cfg(target_os = "linux")]
struct LastSample {
    wall: Instant,
    process_ticks: u64,
}

#[cfg(target_os = "linux")]
static LAST: OnceLock<Mutex<Option<LastSample>>> = OnceLock::new();

#[cfg(target_os = "linux")]
fn last() -> &'static Mutex<Option<LastSample>> {
    LAST.get_or_init(|| Mutex::new(None))
}

/// Sample current RSS + CPU%. Cheap to call (single /proc read on Linux).
/// Safe to call from any thread.
pub fn sample() -> ProcessStats {
    #[cfg(target_os = "linux")]
    {
        sample_linux().unwrap_or_else(ProcessStats::unsupported)
    }
    #[cfg(not(target_os = "linux"))]
    {
        ProcessStats::unsupported()
    }
}

#[cfg(target_os = "linux")]
fn sample_linux() -> Option<ProcessStats> {
    let rss_bytes = read_rss_bytes()?;
    let ticks = read_process_ticks()?;
    let now = Instant::now();
    let num_cpus = read_num_cpus();
    let clock_ticks = clock_ticks_per_sec();
    let system_ram_total_bytes = read_system_ram_total().unwrap_or(0);

    let mut guard = last().lock();
    let cpu_percent = if let Some(prev) = guard.as_ref() {
        let elapsed = now.duration_since(prev.wall).as_secs_f64();
        if elapsed > 0.0 && clock_ticks > 0 {
            let dticks = ticks.saturating_sub(prev.process_ticks) as f64;
            let cpu_seconds = dticks / clock_ticks as f64;
            Some(((cpu_seconds / elapsed) * 100.0) as f32)
        } else {
            None
        }
    } else {
        None
    };
    *guard = Some(LastSample {
        wall: now,
        process_ticks: ticks,
    });

    Some(ProcessStats {
        rss_bytes,
        system_ram_total_bytes,
        cpu_percent,
        num_cpus,
    })
}

#[cfg(target_os = "linux")]
fn read_system_ram_total() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // Format: `VmRSS:    12345 kB`
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_process_ticks() -> Option<u64> {
    // /proc/self/stat fields are space-delimited but `comm` (field 2) is
    // parenthesized and may contain spaces, so anchor on the closing `)`
    // and count from there. Fields after comm: state(3) ppid(4) ...
    // utime(14) stime(15).
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let close = stat.rfind(')')?;
    let after: &str = stat[close + 1..].trim_start();
    let fields: Vec<&str> = after.split_whitespace().collect();
    // After `comm)`, field indices are 0-based here but correspond to
    // /proc indices 3..; utime is /proc field 14 → here index 11,
    // stime is /proc field 15 → here index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(target_os = "linux")]
fn clock_ticks_per_sec() -> u64 {
    // SAFETY: sysconf(_SC_CLK_TCK) is a const POSIX query, no side effects.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 { v as u64 } else { 100 }
}

#[cfg(target_os = "linux")]
fn read_num_cpus() -> u32 {
    // SAFETY: sysconf(_SC_NPROCESSORS_ONLN) is a const POSIX query.
    let v = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if v > 0 { v as u32 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn sample_returns_rss_on_linux() {
        let s = sample();
        assert!(s.rss_bytes > 0, "rss should be non-zero on Linux");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn sample_returns_system_ram_total_and_rss_is_a_subset() {
        let s = sample();
        assert!(
            s.system_ram_total_bytes > 0,
            "MemTotal should be non-zero on Linux"
        );
        assert!(
            s.rss_bytes <= s.system_ram_total_bytes,
            "process RSS ({}) cannot exceed system total ({})",
            s.rss_bytes,
            s.system_ram_total_bytes
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cpu_percent_filled_on_second_sample() {
        let _ = sample();
        std::thread::sleep(std::time::Duration::from_millis(20));
        for _ in 0..10_000 {
            std::hint::black_box(0u64);
        }
        let s2 = sample();
        assert!(
            s2.cpu_percent.is_some(),
            "second sample should have cpu_percent"
        );
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn sample_is_unsupported_off_linux() {
        let s = sample();
        assert_eq!(s.rss_bytes, 0);
        assert!(s.cpu_percent.is_none());
    }
}
