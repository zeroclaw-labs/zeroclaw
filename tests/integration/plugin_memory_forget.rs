//! Verify that zeroclaw_memory_forget host function removes memory entries.
//!
//! Acceptance criterion for US-ZCL-23:
//! > zeroclaw_memory_forget host function removes memory entries
//!
//! These tests assert that:
//! 1. HostFunctionRegistry wires through to the configured Memory backend for forget ops
//! 2. Forget calls are delegated with the correct key
//! 3. Forget returns true when the key existed and was removed
//! 4. Forget returns false when the key did not exist

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::security::audit::AuditLogger;

/// A tracking memory backend that records all forget() calls and returns
/// pre-configured results.
struct TrackingMemory {
    forget_calls: Mutex<Vec<String>>,
    /// Keys that "exist" in the backend — forget() returns true for these.
    existing_keys: Mutex<Vec<String>>,
}

impl TrackingMemory {
    fn new() -> Self {
        Self {
            forget_calls: Mutex::new(Vec::new()),
            existing_keys: Mutex::new(Vec::new()),
        }
    }

    fn with_existing_keys(keys: Vec<&str>) -> Self {
        Self {
            forget_calls: Mutex::new(Vec::new()),
            existing_keys: Mutex::new(keys.into_iter().map(String::from).collect()),
        }
    }

    fn recorded_calls(&self) -> Vec<String> {
        self.forget_calls.lock().clone()
    }
}

#[async_trait]
impl Memory for TrackingMemory {
    fn name(&self) -> &str {
        "tracking"
    }

    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        self.forget_calls.lock().push(key.to_string());
        let mut keys = self.existing_keys.lock();
        if let Some(pos) = keys.iter().position(|k| k == key) {
            keys.remove(pos);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(self.existing_keys.lock().len())
    }

    async fn health_check(&self) -> bool {
        true
    }
}

fn make_audit() -> Arc<AuditLogger> {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let cfg = AuditConfig {
        enabled: false,
        ..Default::default()
    };
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(AuditLogger::new(cfg, path).expect("audit logger"))
}

// ---------------------------------------------------------------------------
// 1. Registry wires through to the configured Memory backend for forget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_memory_forget_delegates_to_backend() {
    let memory = Arc::new(TrackingMemory::with_existing_keys(vec!["target_key"]));
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let removed = registry
        .memory
        .forget("target_key")
        .await
        .expect("forget should succeed");

    assert!(removed, "forget should return true for an existing key");
    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 1, "exactly one forget call expected");
    assert_eq!(calls[0], "target_key");
}

// ---------------------------------------------------------------------------
// 2. Forget calls are delegated with the correct key
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_memory_forget_preserves_key() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    // Key doesn't exist — we just verify delegation
    registry.memory.forget("some/namespaced/key").await.unwrap();
    registry.memory.forget("another-key").await.unwrap();

    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], "some/namespaced/key");
    assert_eq!(calls[1], "another-key");
}

// ---------------------------------------------------------------------------
// 3. Forget returns true when the key existed and was removed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn forget_returns_true_when_key_exists() {
    let memory = Arc::new(TrackingMemory::with_existing_keys(vec!["k1", "k2", "k3"]));
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let removed = registry.memory.forget("k2").await.unwrap();
    assert!(removed, "should return true for existing key");

    // Count should decrease by one
    assert_eq!(
        registry.memory.count().await.unwrap(),
        2,
        "one key should have been removed"
    );
}

#[tokio::test]
async fn forget_removes_only_target_key() {
    let memory = Arc::new(TrackingMemory::with_existing_keys(vec!["a", "b", "c"]));
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    registry.memory.forget("b").await.unwrap();

    // Forgetting "a" and "c" should still succeed (they remain)
    assert!(registry.memory.forget("a").await.unwrap());
    assert!(registry.memory.forget("c").await.unwrap());
    assert_eq!(registry.memory.count().await.unwrap(), 0);
}

// ---------------------------------------------------------------------------
// 4. Forget returns false when the key did not exist
// ---------------------------------------------------------------------------

#[tokio::test]
async fn forget_returns_false_when_key_missing() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let removed = registry
        .memory
        .forget("nonexistent_key")
        .await
        .expect("forget should succeed even for missing key");

    assert!(
        !removed,
        "forget should return false for a key that does not exist"
    );
}

#[tokio::test]
async fn forget_same_key_twice_returns_false_second_time() {
    let memory = Arc::new(TrackingMemory::with_existing_keys(vec!["ephemeral"]));
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let first = registry.memory.forget("ephemeral").await.unwrap();
    let second = registry.memory.forget("ephemeral").await.unwrap();

    assert!(first, "first forget should return true");
    assert!(!second, "second forget should return false — key already gone");

    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 2, "both calls should be recorded");
}

// ---------------------------------------------------------------------------
// 5. Multiple forgets accumulate in the call log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_forgets_recorded() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    for i in 0..4 {
        registry
            .memory
            .forget(&format!("key_{i}"))
            .await
            .unwrap();
    }

    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0], "key_0");
    assert_eq!(calls[3], "key_3");
}
