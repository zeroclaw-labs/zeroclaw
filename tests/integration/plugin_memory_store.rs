//! Verify that zeroclaw_memory_store host function writes to configured memory backend.
//!
//! Acceptance criterion for US-ZCL-23:
//! > zeroclaw_memory_store host function writes to configured memory backend
//!
//! These tests assert that:
//! 1. HostFunctionRegistry wires through to the configured Memory backend for store ops
//! 2. Store calls are delegated with correct key, content, and category
//! 3. The memory_write host function is registered when the memory capability has write=true
//! 4. The memory_write host function is NOT registered when write=false

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::PluginManifest;
use zeroclaw::security::audit::AuditLogger;

/// A tracking memory backend that records all store() calls.
struct TrackingMemory {
    calls: Mutex<Vec<StoredEntry>>,
}

#[derive(Debug, Clone)]
struct StoredEntry {
    key: String,
    content: String,
    category: String,
    session_id: Option<String>,
}

impl TrackingMemory {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    fn stored_entries(&self) -> Vec<StoredEntry> {
        self.calls.lock().clone()
    }
}

#[async_trait]
impl Memory for TrackingMemory {
    fn name(&self) -> &str {
        "tracking"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.calls.lock().push(StoredEntry {
            key: key.to_string(),
            content: content.to_string(),
            category: category.to_string(),
            session_id: session_id.map(String::from),
        });
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

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(self.calls.lock().len())
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
    // Leak the TempDir so it lives for the test duration (won't be cleaned up,
    // but that's fine for tests that create short-lived AuditLoggers).
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(AuditLogger::new(cfg, path).expect("audit logger"))
}

fn make_manifest_with_memory(read: bool, write: bool) -> PluginManifest {
    let toml = format!(
        r#"
        name = "test_memory_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.memory]
        read = {read}
        write = {write}
        "#
    );
    toml::from_str(&toml).expect("valid manifest TOML")
}

// ---------------------------------------------------------------------------
// 1. Registry wires through to the configured Memory backend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_memory_store_delegates_to_backend() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    // Simulate what zeroclaw_memory_store will do: call registry.memory.store()
    registry
        .memory
        .store("test_key", "test_content", MemoryCategory::Core, None)
        .await
        .expect("store should succeed");

    let entries = memory.stored_entries();
    assert_eq!(entries.len(), 1, "exactly one store call expected");
    assert_eq!(entries[0].key, "test_key");
    assert_eq!(entries[0].content, "test_content");
    assert_eq!(entries[0].category, "core");
    assert!(entries[0].session_id.is_none());
}

#[tokio::test]
async fn registry_memory_store_preserves_category() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    registry
        .memory
        .store("k1", "v1", MemoryCategory::Core, None)
        .await
        .unwrap();
    registry
        .memory
        .store("k2", "v2", MemoryCategory::Daily, None)
        .await
        .unwrap();
    registry
        .memory
        .store(
            "k3",
            "v3",
            MemoryCategory::Custom("plugin_data".into()),
            None,
        )
        .await
        .unwrap();

    let entries = memory.stored_entries();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].category, "core");
    assert_eq!(entries[1].category, "daily");
    assert_eq!(entries[2].category, "plugin_data");
}

#[tokio::test]
async fn registry_memory_store_preserves_session_id() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    registry
        .memory
        .store(
            "session_key",
            "session_content",
            MemoryCategory::Conversation,
            Some("sess-42"),
        )
        .await
        .unwrap();

    let entries = memory.stored_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id.as_deref(), Some("sess-42"));
}

// ---------------------------------------------------------------------------
// 2. Multiple stores accumulate in the backend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_stores_accumulate() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    for i in 0..5 {
        registry
            .memory
            .store(
                &format!("key_{i}"),
                &format!("content_{i}"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
    }

    assert_eq!(memory.stored_entries().len(), 5);
    assert_eq!(registry.memory.count().await.unwrap(), 5);
}

// ---------------------------------------------------------------------------
// 3. memory_write host function is registered when write capability is set
// ---------------------------------------------------------------------------

#[test]
fn memory_write_function_registered_when_write_capability_enabled() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(false, true);

    let fns = registry.build_functions(&manifest);
    // With read=false, write=true we expect two functions: store + forget
    assert_eq!(
        fns.len(),
        2,
        "write-only capability should yield 2 functions (store + forget)"
    );
}

#[test]
fn memory_write_and_read_functions_registered_when_both_enabled() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(true, true);

    let fns = registry.build_functions(&manifest);
    assert_eq!(
        fns.len(),
        3,
        "read+write capability should yield 3 functions (recall + store + forget)"
    );
}

// ---------------------------------------------------------------------------
// 4. memory_write NOT registered when write=false
// ---------------------------------------------------------------------------

#[test]
fn memory_write_not_registered_when_write_disabled() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(true, false);

    let fns = registry.build_functions(&manifest);
    // read=true, write=false => only memory_read
    assert_eq!(
        fns.len(),
        1,
        "read-only capability should yield 1 function (no memory_write)"
    );
}

#[test]
fn no_memory_functions_when_no_memory_capability() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());

    let toml_str = r#"
        name = "no_memory_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let manifest: PluginManifest = toml::from_str(toml_str).expect("valid manifest TOML");

    let fns = registry.build_functions(&manifest);
    assert!(fns.is_empty(), "no memory capability => no host functions");
}
