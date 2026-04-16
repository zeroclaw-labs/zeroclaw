//! Lightweight network reachability detector for the on-device fallback path.
//!
//! Runs three independent endpoint probes (OpenAI, Anthropic, Google) with
//! short HEAD/GET timeouts. The host is considered **online** if any probe
//! returns *any* HTTP response — even a 401 means connectivity is intact and
//! credentials are simply missing, which is a different problem.
//!
//! State is cached in an [`AtomicBool`] so callers can poll synchronously
//! from hot paths. Background refresh runs at a configurable cadence.
//!
//! This module exists because MoA must route **proactively** to local Gemma 4
//! when offline — waiting for the cloud provider to time out (10–30 s) would
//! ruin perceived latency. Patent §1 cl. 4 mandates this behavior.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Default endpoints probed for connectivity. HEAD requests with a short
/// timeout each. Reaching any one is sufficient to consider the host online.
pub const DEFAULT_PROBE_URLS: &[&str] = &[
    "https://api.openai.com/v1/models",
    "https://api.anthropic.com/v1/messages",
    "https://generativelanguage.googleapis.com/",
];

/// Default timeout for each individual probe.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(2_500);

/// Default interval between background refresh cycles.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// Snapshot of network reachability state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkStatus {
    /// True when at least one probe URL is reachable.
    pub online: bool,
    /// Unix-seconds timestamp of the last probe completion.
    pub checked_at_unix: u64,
    /// Probe URLs used for the most recent check.
    pub probe_urls: Vec<String>,
}

impl NetworkStatus {
    /// Whether the snapshot is older than `max_age`.
    pub fn is_stale(&self, max_age: Duration) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.checked_at_unix) > max_age.as_secs()
    }
}

/// Cached reachability state with background refresh.
///
/// Cheap to share via `Arc<NetworkHealth>`. Hot-path queries use
/// [`is_online`](Self::is_online) which is a single atomic load.
pub struct NetworkHealth {
    online: AtomicBool,
    last_check_unix: AtomicU64,
    probe_urls: Vec<String>,
    timeout: Duration,
}

impl NetworkHealth {
    /// Create a new health probe with default endpoints (OpenAI, Anthropic, Google)
    /// and a 2.5 s per-probe timeout. Initial state is **online** so callers do
    /// not erroneously route to local on first request before any probe runs.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            online: AtomicBool::new(true),
            last_check_unix: AtomicU64::new(0),
            probe_urls: DEFAULT_PROBE_URLS.iter().map(|&s| s.to_string()).collect(),
            timeout: DEFAULT_PROBE_TIMEOUT,
        })
    }

    /// Custom probe URLs and timeout.
    pub fn with_config(probe_urls: Vec<String>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            online: AtomicBool::new(true),
            last_check_unix: AtomicU64::new(0),
            probe_urls,
            timeout,
        })
    }

    /// Cheap: returns the cached reachability state.
    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    /// Force the cached state (used by tests and by callers that observe a
    /// definitive event such as a successful API call).
    pub fn set_online(&self, online: bool) {
        let prev = self.online.swap(online, Ordering::Relaxed);
        if prev != online {
            tracing::info!(
                online,
                "Network reachability state changed (forced by caller)"
            );
        }
        self.last_check_unix
            .store(now_unix_secs(), Ordering::Relaxed);
    }

    /// Snapshot the current state.
    pub fn snapshot(&self) -> NetworkStatus {
        NetworkStatus {
            online: self.is_online(),
            checked_at_unix: self.last_check_unix.load(Ordering::Relaxed),
            probe_urls: self.probe_urls.clone(),
        }
    }

    /// Run all probes once, update the cache, and return the result.
    /// Result is true when at least one probe URL responded with any HTTP
    /// status (including 4xx/5xx — the wire is up).
    pub async fn check_now(&self) -> bool {
        let urls: Vec<&str> = self.probe_urls.iter().map(|s| s.as_str()).collect();
        let online = check_endpoints(&urls, self.timeout).await;
        let prev = self.online.swap(online, Ordering::Relaxed);
        self.last_check_unix
            .store(now_unix_secs(), Ordering::Relaxed);
        if prev != online {
            tracing::info!(
                online,
                probes = self.probe_urls.len(),
                "Network reachability state changed"
            );
        }
        online
    }

    /// Spawn a background tokio task that refreshes the cache every `interval`.
    /// Returns the join handle so callers may abort it on shutdown.
    pub fn spawn_refresh_loop(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let _ = self.check_now().await;
            }
        })
    }
}

impl Default for NetworkHealth {
    fn default() -> Self {
        Self {
            online: AtomicBool::new(true),
            last_check_unix: AtomicU64::new(0),
            probe_urls: DEFAULT_PROBE_URLS.iter().map(|&s| s.to_string()).collect(),
            timeout: DEFAULT_PROBE_TIMEOUT,
        }
    }
}

/// Probe `urls` in parallel; return true on first response received.
///
/// Any HTTP response counts (incl. 401/403/429) — connectivity is what we
/// measure, not authorization. Network errors (timeout, DNS, refused) count
/// as "unreachable for this URL".
pub async fn check_endpoints(urls: &[&str], timeout_per: Duration) -> bool {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let client = match reqwest::Client::builder().timeout(timeout_per).build() {
        Ok(c) => c,
        Err(_) => return false,
    };

    let mut futures: FuturesUnordered<_> = urls
        .iter()
        .map(|&url| {
            let client = client.clone();
            let url = url.to_string();
            async move { client.head(&url).send().await.map(|_| ()).map_err(|_| ()) }
        })
        .collect();

    while let Some(res) = futures.next().await {
        if res.is_ok() {
            return true;
        }
    }
    false
}

// Use the shared helper from src/util.rs (was duplicated here + cosyvoice2).
use crate::util::now_unix_secs;

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_starts_online() {
        let h = NetworkHealth::new();
        assert!(
            h.is_online(),
            "default state should be online so first req tries cloud"
        );
    }

    #[test]
    fn set_online_updates_cache() {
        let h = NetworkHealth::new();
        h.set_online(false);
        assert!(!h.is_online());
        h.set_online(true);
        assert!(h.is_online());
    }

    #[test]
    fn snapshot_carries_probe_urls() {
        let h = NetworkHealth::with_config(
            vec!["https://example.test".to_string()],
            Duration::from_millis(500),
        );
        let snap = h.snapshot();
        assert_eq!(snap.probe_urls, vec!["https://example.test"]);
    }

    #[test]
    fn snapshot_staleness() {
        let snap = NetworkStatus {
            online: true,
            checked_at_unix: 0, // very old
            probe_urls: vec![],
        };
        assert!(snap.is_stale(Duration::from_secs(60)));
    }

    #[tokio::test]
    async fn unreachable_endpoints_report_offline() {
        // Reserved TEST-NET-1 (RFC 5737) — guaranteed unreachable.
        let urls = vec!["https://192.0.2.1/".to_string()];
        let h = NetworkHealth::with_config(urls, Duration::from_millis(500));
        let online = h.check_now().await;
        assert!(!online, "TEST-NET-1 must be unreachable");
        assert!(!h.is_online());
    }

    #[tokio::test]
    async fn check_endpoints_short_circuits_on_first_success() {
        // Mix one definitely-bad and one likely-good URL; we accept either
        // outcome but the function must not panic and must return a bool.
        let urls = vec!["https://192.0.2.1/", "https://example.com/"];
        let _ = check_endpoints(&urls, Duration::from_secs(1)).await;
    }

    /// Live test against the real configured probe set. Run with:
    ///     cargo test --lib local_llm::network_health::tests::live_check -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_check() {
        let h = NetworkHealth::new();
        let online = h.check_now().await;
        let snap = h.snapshot();
        println!("\nnetwork status: online={online}");
        println!("snapshot: {snap:#?}");
    }
}
