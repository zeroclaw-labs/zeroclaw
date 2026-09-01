//! Self-process resource sampling — RSS (resident memory) and CPU%.

use parking_lot::Mutex;
use serde::Serialize;
use std::sync::OnceLock;
use std::time::Instant;
use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

#[derive(Debug, Clone, Serialize)]
pub struct ProcessStats {
    /// Resident set size in bytes. `0` when unsupported.
    pub rss_bytes: u64,
    /// Total system RAM in bytes. `0` when unsupported. The dashboard
    /// renders `rss / system_ram_total` as a percentage so the RAM tile is
    /// meaningful at a glance regardless of host size.
    pub system_ram_total_bytes: u64,
    /// CPU usage as a percentage summed across logical cores (0..100*ncpu).
    /// `None` on the first sample (no baseline) or unsupported platforms.
    pub cpu_percent: Option<f32>,
    /// Number of logical CPUs the OS reports. Useful for clamping the CPU%
    /// bar on the dashboard. `0` when unknown.
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

struct State {
    system: System,
    /// True once we've refreshed at least once — only then does
    /// `Process::cpu_usage()` have a delta to report. We return `None` on
    /// the first sample to preserve the pre-sysinfo contract (dashboard
    /// already handles this).
    have_baseline: bool,
    last_cpu_refresh: Option<Instant>,
    /// Last CPU% we returned; served on rapid re-samples so callers still
    /// see a plausible value instead of a sysinfo-internal artifact.
    last_cpu_percent: Option<f32>,
    pid: Pid,
}

static STATE: OnceLock<Mutex<Option<State>>> = OnceLock::new();

fn state() -> &'static Mutex<Option<State>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// Sample current RSS + CPU%. Cheap to call — refreshes only this process
/// and only the CPU/memory fields. Safe to call from any thread.
pub fn sample() -> ProcessStats {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return ProcessStats::unsupported();
    }
    let Ok(pid) = sysinfo::get_current_pid() else {
        return ProcessStats::unsupported();
    };

    let mut guard = state().lock();
    let st = guard.get_or_insert_with(|| {
        // Populate `cpus()` and initial memory once. `RefreshKind::nothing()`
        // keeps the constructor cheap; per-sample refreshes fill in the
        // fields we actually read below.
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(sysinfo::CpuRefreshKind::nothing())
                .with_memory(MemoryRefreshKind::nothing().with_ram()),
        );
        State {
            system,
            have_baseline: false,
            last_cpu_refresh: None,
            last_cpu_percent: None,
            pid,
        }
    });

    // Skip the CPU-refresh half when the caller is hammering us faster than
    // sysinfo's internal tick window; otherwise `cpu_usage()` returns a
    // meaningless floor/ceiling value. Memory refresh is always cheap and
    // has no minimum-interval constraint, so we always update RSS.
    let now = Instant::now();
    let cpu_stale = st
        .last_cpu_refresh
        .is_none_or(|prev| now.duration_since(prev) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    let refresh_kind = if cpu_stale {
        ProcessRefreshKind::nothing().with_cpu().with_memory()
    } else {
        ProcessRefreshKind::nothing().with_memory()
    };
    st.system
        .refresh_processes_specifics(ProcessesToUpdate::Some(&[st.pid]), true, refresh_kind);
    // Total system RAM can change at runtime (memory hot-add on enterprise
    // servers, hypervisor ballooning on VMs), so refresh it every sample.
    // Cost is a single sysctl / /proc/meminfo read / GlobalMemoryStatusEx —
    // cheap enough that we don't rate-limit it.
    st.system
        .refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    // Refresh the CPU list too — logical CPU count can shift (Linux CPU
    // hot-plug via /sys/devices/system/cpu/cpuN/online, cloud VM vCPU
    // resize). Only piggy-backs when we're already doing a CPU refresh, so
    // rapid re-samples remain cheap.
    if cpu_stale {
        st.system
            .refresh_cpu_specifics(sysinfo::CpuRefreshKind::nothing());
    }

    let Some(proc) = st.system.process(st.pid) else {
        return ProcessStats::unsupported();
    };
    let rss_bytes = proc.memory();
    let cpu_percent = if cpu_stale {
        st.last_cpu_refresh = Some(now);
        if st.have_baseline {
            let v = proc.cpu_usage();
            st.last_cpu_percent = Some(v);
            Some(v)
        } else {
            st.have_baseline = true;
            None
        }
    } else {
        // Sub-interval refresh: reuse the previous reading rather than
        // returning a sysinfo artifact.
        st.last_cpu_percent
    };
    let system_ram_total_bytes = st.system.total_memory();
    let num_cpus = st.system.cpus().len() as u32;

    ProcessStats {
        rss_bytes,
        system_ram_total_bytes,
        cpu_percent,
        num_cpus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_returns_rss_on_supported_hosts() {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return;
        }
        let s = sample();
        assert!(s.rss_bytes > 0, "rss should be non-zero on supported hosts");
    }

    #[test]
    fn sample_returns_system_ram_total_and_rss_is_a_subset() {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return;
        }
        let s = sample();
        assert!(
            s.system_ram_total_bytes > 0,
            "total memory should be non-zero on supported hosts"
        );
        assert!(
            s.rss_bytes <= s.system_ram_total_bytes,
            "process RSS ({}) cannot exceed system total ({})",
            s.rss_bytes,
            s.system_ram_total_bytes
        );
    }

    #[test]
    fn cpu_percent_filled_on_second_sample() {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return;
        }
        let _ = sample();
        // sysinfo requires at least MINIMUM_CPU_UPDATE_INTERVAL between
        // refreshes for the CPU% delta to be meaningful. Sleep just past it.
        std::thread::sleep(
            sysinfo::MINIMUM_CPU_UPDATE_INTERVAL + std::time::Duration::from_millis(10),
        );
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
    fn sample_reports_num_cpus_on_supported_hosts() {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return;
        }
        let s = sample();
        assert!(
            s.num_cpus > 0,
            "num_cpus should be non-zero on supported hosts"
        );
    }

    #[test]
    fn rapid_resample_reuses_last_cpu_percent_instead_of_sysinfo_artifact() {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
            return;
        }

        // The property under test is conditional: resamples that land inside
        // MINIMUM_CPU_UPDATE_INTERVAL of the last real reading must reuse it.
        // The condition is a timing premise this test cannot force on a
        // loaded runner: under a parallel test run the thread can be
        // descheduled between the samples for longer than the interval
        // (making a fresh reading legitimate), and `STATE` is process-global,
        // so a concurrent `sample()` from another test that crosses the
        // interval boundary rotates the cache mid-sequence. Both showed up as
        // one-in-many CI failures of the parallel runtime gate.
        //
        // So each attempt establishes the premise, samples, and only counts
        // when the whole sequence provably fit inside the interval window
        // anchored before the priming call: any refresh — ours or a
        // concurrent caller's — can then only have happened at or after that
        // anchor, so every later call in the window is sub-interval and must
        // serve the cache. Attempts where the premise did not hold are
        // retried rather than asserted. Exhausting all attempts still fails
        // loudly: with rate limiting broken, `a`/`b` are independent sysinfo
        // reads and will not equal `primed` five attempts running, so the
        // regression this test exists for is still caught.
        // Establish the process-global CPU baseline first: the very first
        // `sample()` in the process returns `None` by contract, and this
        // test must not depend on a sibling test having sampled already.
        let _ = sample();

        const ATTEMPTS: usize = 5;
        let mut last_failure = String::new();
        for attempt in 1..=ATTEMPTS {
            // Ensure the next sample takes (or observes) a reading no older
            // than `anchor`, then prime.
            std::thread::sleep(
                sysinfo::MINIMUM_CPU_UPDATE_INTERVAL + std::time::Duration::from_millis(10),
            );
            let anchor = Instant::now();
            let primed = sample();
            assert!(primed.cpu_percent.is_some(), "primed sample has cpu%");

            // Back-to-back calls intended to land well under
            // MINIMUM_CPU_UPDATE_INTERVAL — without rate limiting sysinfo
            // would return 0 (or 100*ncpu on Linux) instead of the cache.
            let a = sample();
            let b = sample();
            let elapsed = anchor.elapsed();

            if elapsed >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL {
                // Premise blown: the runner stalled us across the interval,
                // so a fresh reading was legitimate. Not a verdict either way.
                last_failure = format!(
                    "attempt {attempt}: sequence took {elapsed:?}, \
                     exceeding MINIMUM_CPU_UPDATE_INTERVAL"
                );
                continue;
            }
            if a.cpu_percent == primed.cpu_percent && b.cpu_percent == primed.cpu_percent {
                // RSS is fine to update at any cadence — just check it stays
                // plausible.
                assert!(a.rss_bytes > 0);
                assert!(b.rss_bytes > 0);
                return;
            }
            // In-window mismatch. A concurrent sampler that crossed the
            // interval boundary just before our anchor can still cause this
            // once; its fresh reading resets the cache clock, so a retry
            // gets a clean window. A real rate-limiting regression fails
            // every attempt.
            last_failure = format!(
                "attempt {attempt}: rapid resample did not reuse the last real reading \
                 (primed={:?} a={:?} b={:?}, elapsed {elapsed:?})",
                primed.cpu_percent, a.cpu_percent, b.cpu_percent
            );
        }
        panic!(
            "rapid resample never reused the cached cpu% in {ATTEMPTS} attempts; \
             last: {last_failure}"
        );
    }
}
