//! Crate-wide test-only synchronization for shared process-global state.
//!
//! Several tool modules read or write the same process-global runtime proxy
//! config / client cache in `zeroclaw_config::schema`:
//! `file_download` installs a temporary proxy for its proxy-bypass tests,
//! real-request `execute()` tests in `file_upload` / `file_upload_bundle` build
//! clients from that config, and `proxy_config` tests write it through the
//! `set`/`disable` handlers. When these run in parallel in the same
//! `zeroclaw-tools` library test binary, one test can observe (or clobber)
//! another's temporary proxy. This module exposes one shared lock so all of
//! them can serialize on it, keeping the suite deterministic.

use zeroclaw_config::schema::{ProxyConfig, runtime_proxy_config, set_runtime_proxy_config};

/// Serializes access to the process-global runtime proxy config and client
/// cache across the whole crate test binary. Every test that reads, writes, or
/// builds a request from that state must hold this lock for its duration.
static PROXY_TEST_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// Acquire the proxy test lock, initializing it on first use. Acquire at the
/// top of any test that touches the runtime proxy config or client cache, and
/// hold it for the whole test so proxy state stays stable while it is read.
pub(crate) async fn proxy_test_lock_guard() -> tokio::sync::MutexGuard<'static, ()> {
    PROXY_TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

/// RAII guard that installs a runtime proxy config for a test and restores the
/// default (proxy disabled) on drop — including on panic, so a failing
/// proxy-bypass test cannot leak its config into sibling tests. Serializes
/// against every other proxy-touching test via [`PROXY_TEST_LOCK`].
pub(crate) struct RuntimeProxyGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl RuntimeProxyGuard {
    pub(crate) async fn install(config: ProxyConfig) -> Self {
        let _lock = PROXY_TEST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        set_runtime_proxy_config(config);
        RuntimeProxyGuard { _lock }
    }
}

impl Drop for RuntimeProxyGuard {
    fn drop(&mut self) {
        set_runtime_proxy_config(ProxyConfig::default());
    }
}

/// Process-env proxy variables written by `ProxyConfig::apply_to_process_env`
/// / `clear_process_env`, including the lowercase aliases the config helper
/// sets alongside each uppercase form (`set_proxy_env_pair` sets both `KEY`
/// and `key`). Captured and restored by [`RuntimeProxySnapshotGuard`]
/// alongside the process-global runtime config, so a mutating `proxy_config`
/// handler test (which drives `set_runtime_proxy_config` and may also call
/// `apply_to_process_env` / `clear_process_env`) cannot leak a proxy into a
/// later test that builds a client via the process-global config.
const PROXY_ENV_KEYS: [&str; 8] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// RAII guard that snapshots the process-global runtime proxy config AND the
/// proxy process-env variables at construction, then restores both on drop —
/// including on panic. Unlike [`RuntimeProxyGuard`] (which resets the config to
/// the default), this guard restores the exact prior state, which is what a
/// `proxy_config` handler test needs when it mutates the runtime proxy through
/// the tool's `set`/`disable`/`enable` actions. Serializes against every other
/// proxy-touching test via [`PROXY_TEST_LOCK`]; the restoration runs in
/// [`Drop::drop`] while the lock guard is still held.
pub(crate) struct RuntimeProxySnapshotGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    config: ProxyConfig,
    env: Vec<(String, Option<String>)>,
}

impl RuntimeProxySnapshotGuard {
    pub(crate) async fn snapshot() -> Self {
        let _lock = PROXY_TEST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let config = runtime_proxy_config();
        let env = PROXY_ENV_KEYS
            .iter()
            .map(|key| (key.to_string(), std::env::var(key).ok()))
            .collect();
        RuntimeProxySnapshotGuard { _lock, config, env }
    }
}

impl Drop for RuntimeProxySnapshotGuard {
    fn drop(&mut self) {
        // Restore the runtime config first; `set_runtime_proxy_config` also
        // clears the client cache, so a client built under the leaked proxy is
        // rebuilt against the restored config on the next `build_runtime_proxy_client`.
        set_runtime_proxy_config(self.config.clone());
        for (key, value) in &self.env {
            // SAFETY: mutating the process env here mirrors `clear_process_env`
            // (which carries the same safety comment); the guard still holds
            // the proxy test lock, so no sibling proxy test is reading the env
            // concurrently, matching the single-threaded-per-lock discipline
            // the proxy_config set handler relies on.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}
