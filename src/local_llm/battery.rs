//! Host battery probe. Read-only, best-effort, zero new crate deps.
//!
//! Used by [`crate::providers::ollama::OllamaProvider`] to flip the active
//! tuning to [`crate::providers::ollama::OllamaTuning::for_battery_saver`]
//! when the laptop is running on battery below a configurable threshold.
//! Desktops (no battery) always report `None` and the tuning stays on
//! whichever `for_app_chat` / `for_channel_chat` profile the caller
//! picked, which is the correct behaviour — the app should not degrade
//! response quality on a machine that's literally plugged into the wall.
//!
//! Platform coverage
//! -----------------
//! * Linux: `/sys/class/power_supply/BAT*/capacity` + `/sys/class/power_supply/*/online`
//! * macOS: `pmset -g batt` parsing
//! * Windows: `powercfg /batteryreport /xml` is too heavy for a 5-second
//!   poll — we use `WMIC PATH Win32_Battery GET
//!   EstimatedChargeRemaining,BatteryStatus /VALUE` instead (synchronous,
//!   <100ms on fast hosts)
//! * Other OSes: return `BatterySnapshot::unknown()` and let callers
//!   keep their default tuning

use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc, OnceLock,
};
use std::time::Duration;

use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use tokio::process::Command;

/// Default poll interval for the background refresh loop.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// Default low-battery threshold (percent). Below this, the reactive
/// tuning flips to battery-saver: `keep_alive: "0"` and `num_ctx: 4_096`.
pub const DEFAULT_LOW_BATTERY_PCT: u8 = 25;

/// Outcome of a single probe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatterySnapshot {
    /// None when no battery is detected (desktop, docker, CI). Some(pct)
    /// with pct in `[0, 100]` otherwise.
    pub percent: Option<u8>,
    /// None when unknown (desktop / probe failure), Some(true) when
    /// plugged into AC, Some(false) when on battery.
    pub on_ac_power: Option<bool>,
}

impl BatterySnapshot {
    pub const fn unknown() -> Self {
        Self {
            percent: None,
            on_ac_power: None,
        }
    }

    /// Should MoA apply battery-saver tuning right now?
    ///
    /// True when: the host has a battery, it's NOT on AC, AND the charge
    /// is at or below `low_threshold_pct`. A desktop (no battery) or a
    /// plugged-in laptop always returns false.
    pub fn needs_battery_saver(&self, low_threshold_pct: u8) -> bool {
        match (self.on_ac_power, self.percent) {
            (Some(false), Some(pct)) => pct <= low_threshold_pct,
            _ => false,
        }
    }
}

/// Process-wide shared battery monitor. Lazily starts the refresh loop
/// on first access inside a tokio runtime.
#[derive(Debug)]
pub struct BatteryHealth {
    // AtomicU8: 0 = unknown (no battery OR probe failed), 1..=101 means
    // percent + 1 (so 1 = 0%, 101 = 100%). Atomics give us lock-free
    // read from the hot path (every outgoing chat request).
    percent_plus_one: AtomicU8,
    on_ac_known: AtomicBool,
    on_ac_power: AtomicBool,
}

impl BatteryHealth {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            percent_plus_one: AtomicU8::new(0),
            on_ac_known: AtomicBool::new(false),
            on_ac_power: AtomicBool::new(true),
        })
    }

    /// Cheap lock-free snapshot. Callers on the chat hot path call this.
    pub fn snapshot(&self) -> BatterySnapshot {
        let raw = self.percent_plus_one.load(Ordering::Relaxed);
        let percent = if raw == 0 { None } else { Some(raw - 1) };
        let on_ac_power = if self.on_ac_known.load(Ordering::Relaxed) {
            Some(self.on_ac_power.load(Ordering::Relaxed))
        } else {
            None
        };
        BatterySnapshot {
            percent,
            on_ac_power,
        }
    }

    fn store(&self, snap: BatterySnapshot) {
        let raw = match snap.percent {
            Some(p) => p.saturating_add(1),
            None => 0,
        };
        self.percent_plus_one.store(raw, Ordering::Relaxed);
        match snap.on_ac_power {
            Some(v) => {
                self.on_ac_power.store(v, Ordering::Relaxed);
                self.on_ac_known.store(true, Ordering::Relaxed);
            }
            None => {
                self.on_ac_known.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Run one probe immediately (used on startup and by tests).
    pub async fn check_now(&self) -> BatterySnapshot {
        let snap = probe_host_battery().await;
        self.store(snap);
        snap
    }

    /// Spawn a background refresh loop at the given interval. Call at
    /// most once per process; the returned handle is intentionally
    /// leaked to the tokio runtime because a battery monitor that never
    /// stops is the correct behaviour for the whole process lifetime.
    pub fn spawn_refresh_loop(
        self: Arc<Self>,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let _ = self.check_now().await;
                tokio::time::sleep(interval).await;
            }
        })
    }
}

static SHARED_BATTERY: OnceLock<Arc<BatteryHealth>> = OnceLock::new();

/// Returns the process-wide shared [`BatteryHealth`]. Cheap — one atomic
/// get + Arc::clone. First call from inside a tokio runtime spawns the
/// refresh loop at [`DEFAULT_REFRESH_INTERVAL`]; before then the reading
/// is the construction-time default (unknown / on AC / 0%).
pub fn shared() -> Arc<BatteryHealth> {
    SHARED_BATTERY
        .get_or_init(|| {
            let h = BatteryHealth::new();
            if tokio::runtime::Handle::try_current().is_ok() {
                let _refresh: tokio::task::JoinHandle<()> =
                    Arc::clone(&h).spawn_refresh_loop(DEFAULT_REFRESH_INTERVAL);
            }
            h
        })
        .clone()
}

// ── Platform probes ──────────────────────────────────────────────────

async fn probe_host_battery() -> BatterySnapshot {
    #[cfg(target_os = "linux")]
    {
        if let Some(snap) = probe_linux().await {
            return snap;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(snap) = probe_macos().await {
            return snap;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(snap) = probe_windows().await {
            return snap;
        }
    }
    BatterySnapshot::unknown()
}

#[cfg(target_os = "linux")]
async fn probe_linux() -> Option<BatterySnapshot> {
    use tokio::fs as afs;
    // Find the first BAT* directory.
    let mut percent: Option<u8> = None;
    let mut on_ac_power: Option<bool> = None;

    if let Ok(mut entries) = afs::read_dir("/sys/class/power_supply").await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("BAT") {
                let capacity_path = entry.path().join("capacity");
                if let Ok(content) = afs::read_to_string(&capacity_path).await {
                    if let Ok(pct) = content.trim().parse::<u8>() {
                        percent = Some(pct.min(100));
                    }
                }
            }
            // `AC` is the conventional name but can also be `ACAD` or
            // similar. Match any supply of type `Mains`.
            let type_path = entry.path().join("type");
            if let Ok(type_str) = afs::read_to_string(&type_path).await {
                if type_str.trim() == "Mains" {
                    let online_path = entry.path().join("online");
                    if let Ok(online_str) = afs::read_to_string(&online_path).await {
                        on_ac_power = Some(online_str.trim() == "1");
                    }
                }
            }
        }
    }

    if percent.is_none() && on_ac_power.is_none() {
        return None;
    }
    Some(BatterySnapshot {
        percent,
        on_ac_power,
    })
}

#[cfg(target_os = "macos")]
async fn probe_macos() -> Option<BatterySnapshot> {
    // `pmset -g batt` output:
    //   Now drawing from 'Battery Power'
    //    -InternalBattery-0 (id=...) 78%; discharging; 3:42 remaining present: true
    // or on desktops: `Now drawing from 'AC Power'` with no battery line.
    let out = Command::new("pmset")
        .args(["-g", "batt"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);

    let on_ac_power = if text.contains("'AC Power'") {
        Some(true)
    } else if text.contains("'Battery Power'") {
        Some(false)
    } else {
        None
    };

    let percent = text
        .lines()
        .find_map(|line| {
            // Look for the first `NN%` token.
            line.split_whitespace()
                .find_map(|tok| tok.strip_suffix('%').and_then(|p| p.parse::<u8>().ok()))
        })
        .map(|p| p.min(100));

    if percent.is_none() && on_ac_power.is_none() {
        return None;
    }
    Some(BatterySnapshot {
        percent,
        on_ac_power,
    })
}

#[cfg(target_os = "windows")]
async fn probe_windows() -> Option<BatterySnapshot> {
    // WMIC returns key=value lines separated by blank lines.
    let out = Command::new("wmic")
        .args([
            "PATH",
            "Win32_Battery",
            "GET",
            "EstimatedChargeRemaining,BatteryStatus",
            "/VALUE",
        ])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);

    let mut percent: Option<u8> = None;
    let mut battery_status: Option<u16> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("EstimatedChargeRemaining=") {
            percent = val.trim().parse::<u8>().ok().map(|p| p.min(100));
        } else if let Some(val) = line.strip_prefix("BatteryStatus=") {
            battery_status = val.trim().parse::<u16>().ok();
        }
    }

    // Per MSDN Win32_Battery:
    //   1 = Discharging (on battery)
    //   2 = Connected to AC (not charging)
    //   3 = Fully charged (on AC)
    //   4 = Low
    //   5 = Critical
    //   6..=9 = Charging states (on AC)
    let on_ac_power = battery_status.map(|s| matches!(s, 2 | 3 | 6 | 7 | 8 | 9));

    if percent.is_none() && on_ac_power.is_none() {
        return None;
    }
    Some(BatterySnapshot {
        percent,
        on_ac_power,
    })
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_snapshot_never_triggers_battery_saver() {
        assert!(!BatterySnapshot::unknown().needs_battery_saver(99));
    }

    #[test]
    fn on_ac_snapshot_does_not_trigger_even_when_low() {
        let s = BatterySnapshot {
            percent: Some(5),
            on_ac_power: Some(true),
        };
        assert!(!s.needs_battery_saver(25));
    }

    #[test]
    fn on_battery_triggers_at_or_below_threshold() {
        let low = BatterySnapshot {
            percent: Some(25),
            on_ac_power: Some(false),
        };
        assert!(low.needs_battery_saver(25));
        let lower = BatterySnapshot {
            percent: Some(5),
            on_ac_power: Some(false),
        };
        assert!(lower.needs_battery_saver(25));
    }

    #[test]
    fn on_battery_above_threshold_does_not_trigger() {
        let high = BatterySnapshot {
            percent: Some(80),
            on_ac_power: Some(false),
        };
        assert!(!high.needs_battery_saver(25));
    }

    #[test]
    fn unknown_ac_status_never_triggers() {
        // Even on a host where we read percent but not AC status, we
        // must not flip to battery-saver — desktops read percent=None
        // but some embedded Linux reports percent without AC info.
        let partial = BatterySnapshot {
            percent: Some(5),
            on_ac_power: None,
        };
        assert!(!partial.needs_battery_saver(25));
    }

    #[tokio::test]
    async fn shared_memoizes() {
        let a = shared();
        let b = shared();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn check_now_writes_atomic_store() {
        let h = BatteryHealth::new();
        let snap = BatterySnapshot {
            percent: Some(42),
            on_ac_power: Some(false),
        };
        h.store(snap);
        let read = h.snapshot();
        assert_eq!(read.percent, Some(42));
        assert_eq!(read.on_ac_power, Some(false));
    }

    #[tokio::test]
    async fn check_now_handles_unknown_roundtrip() {
        let h = BatteryHealth::new();
        h.store(BatterySnapshot::unknown());
        let read = h.snapshot();
        assert_eq!(read, BatterySnapshot::unknown());
    }

    #[tokio::test]
    async fn probe_does_not_panic_on_current_host() {
        // Whatever the host is, a single probe must not panic.
        let _ = probe_host_battery().await;
    }

    /// Manual smoke test — dumps the current host's battery state.
    /// Run with:
    ///     cargo test --lib local_llm::battery::tests::dump_current_battery -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn dump_current_battery() {
        let snap = probe_host_battery().await;
        println!("\nBattery snapshot: {:?}", snap);
        println!(
            "needs_battery_saver(25%) = {}",
            snap.needs_battery_saver(DEFAULT_LOW_BATTERY_PCT)
        );
    }
}
