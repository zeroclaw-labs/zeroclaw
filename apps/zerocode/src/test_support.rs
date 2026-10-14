//! Shared test-only helpers.
//!
//! `std::env` is process-global, so every test that mutates or *reads through*
//! an environment variable must serialize on the same lock — a per-module lock
//! only protects that module and lets values leak across suites.

/// Global guard for synchronous tests that set, remove, or resolve
/// configuration through environment variables.
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Sets an environment variable for the duration of a test and removes it on
/// drop. Callers MUST hold [`env_test_lock`] for the guard's whole lifetime,
/// since `std::env` mutation is process-global.
pub(crate) struct EnvVarGuard(&'static str);

impl EnvVarGuard {
    pub(crate) fn set(name: &'static str, value: &str) -> Self {
        // SAFETY: callers serialize on `env_test_lock()`.
        unsafe { std::env::set_var(name, value) };
        Self(name)
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: callers serialize on `env_test_lock()`.
        unsafe { std::env::remove_var(self.0) };
    }
}
