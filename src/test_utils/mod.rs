//! Common test utilities and helpers for ZeroClaw tests.
//!
//! This module provides reusable test helpers, fixtures, and RAII guards
//! to reduce code duplication across the codebase.

use std::env;

/// RAII guard that restores an environment variable to its original state on drop.
///
/// Ensures cleanup even if test panics, preventing environment leakage
/// between tests.
///
/// # Example
/// ```ignore
/// use zeroclaw::test_utils::EnvGuard;
///
/// #[test]
/// fn test_with_custom_env() {
///     let _guard = EnvGuard::set("MY_VAR", "test-value");
///     // ... test code that uses MY_VAR ...
///     // MY_VAR is automatically restored when _guard goes out of scope
/// }
/// ```
pub struct EnvGuard {
    key: &'static str,
    original: Option<String>,
}

impl EnvGuard {
    /// Sets an environment variable and returns a guard that restores it on drop.
    ///
    /// # Arguments
    /// * `key` - Environment variable name
    /// * `value` - Value to set
    ///
    /// # Returns
    /// * `EnvGuard` that will restore the original value on drop
    pub fn set(key: &'static str, value: &str) -> Self {
        let original = env::var(key).ok();
        env::set_var(key, value);
        Self { key, original }
    }

    /// Removes an environment variable and returns a guard that restores it on drop.
    ///
    /// # Arguments
    /// * `key` - Environment variable name to remove
    ///
    /// # Returns
    /// * `EnvGuard` that will restore the original value on drop
    pub fn remove(key: &'static str) -> Self {
        let original = env::var(key).ok();
        env::remove_var(key);
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(val) => env::set_var(self.key, val),
            None => env::remove_var(self.key),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_guard_sets_and_restores_value() {
        // Ensure clean starting state
        env::remove_var("TEST_ENV_GUARD_VAR");

        {
            let _guard = EnvGuard::set("TEST_ENV_GUARD_VAR", "test-value");
            assert_eq!(env::var("TEST_ENV_GUARD_VAR"), Ok("test-value".to_string()));
        }

        // Value should be restored (removed, since it didn't exist before)
        assert_eq!(env::var("TEST_ENV_GUARD_VAR"), Err(env::VarError::NotPresent));
    }

    #[test]
    fn env_guard_restores_original_value() {
        env::set_var("TEST_ENV_GUARD_VAR2", "original-value");

        {
            let _guard = EnvGuard::set("TEST_ENV_GUARD_VAR2", "test-value");
            assert_eq!(env::var("TEST_ENV_GUARD_VAR2"), Ok("test-value".to_string()));
        }

        // Original value should be restored
        assert_eq!(env::var("TEST_ENV_GUARD_VAR2"), Ok("original-value".to_string()));

        // Cleanup
        env::remove_var("TEST_ENV_GUARD_VAR2");
    }

    #[test]
    fn env_guard_remove_works() {
        env::set_var("TEST_ENV_GUARD_VAR3", "some-value");

        {
            let _guard = EnvGuard::remove("TEST_ENV_GUARD_VAR3");
            assert_eq!(env::var("TEST_ENV_GUARD_VAR3"), Err(env::VarError::NotPresent));
        }

        // Original value should be restored
        assert_eq!(env::var("TEST_ENV_GUARD_VAR3"), Ok("some-value".to_string()));

        // Cleanup
        env::remove_var("TEST_ENV_GUARD_VAR3");
    }

    #[test]
    fn env_guard_handles_multiple_guards() {
        env::remove_var("TEST_ENV_VAR_A");
        env::remove_var("TEST_ENV_VAR_B");

        {
            let _guard1 = EnvGuard::set("TEST_ENV_VAR_A", "value-a");
            let _guard2 = EnvGuard::set("TEST_ENV_VAR_B", "value-b");

            assert_eq!(env::var("TEST_ENV_VAR_A"), Ok("value-a".to_string()));
            assert_eq!(env::var("TEST_ENV_VAR_B"), Ok("value-b".to_string()));
        }

        // Both should be removed (since they didn't exist before)
        assert_eq!(env::var("TEST_ENV_VAR_A"), Err(env::VarError::NotPresent));
        assert_eq!(env::var("TEST_ENV_VAR_B"), Err(env::VarError::NotPresent));
    }
}
