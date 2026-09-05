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

use zeroclaw_config::schema::{ProxyConfig, set_runtime_proxy_config};

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
