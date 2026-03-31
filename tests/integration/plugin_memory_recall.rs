//! Verify that zeroclaw_memory_recall host function queries memory and returns results.
//!
//! Acceptance criterion for US-ZCL-23:
//! > zeroclaw_memory_recall host function queries memory and returns results
//!
//! These tests assert that:
//! 1. HostFunctionRegistry wires through to the configured Memory backend for recall ops
//! 2. Recall calls are delegated with correct query, limit, and session_id
//! 3. Recall returns matching MemoryEntry results from the backend
//! 4. The memory_read host function is registered when the memory capability has read=true
//! 5. The memory_read host function is NOT registered when read=false

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::PluginManifest;
use zeroclaw::security::audit::AuditLogger;

/// A tracking memory backend that records all recall() calls and returns
/// pre-configured results.
struct TrackingMemory {
    recall_calls: Mutex<Vec<RecallCall>>,
    recall_results: Mutex<Vec<MemoryEntry>>,
}

#[derive(Debug, Clone)]
struct RecallCall {
    query: String,
    limit: usize,
    session_id: Option<String>,
    since: Option<String>,
    until: Option<String>,
}

impl TrackingMemory {
    fn new() -> Self {
        Self {
            recall_calls: Mutex::new(Vec::new()),
            recall_results: Mutex::new(Vec::new()),
        }
    }

    fn with_results(results: Vec<MemoryEntry>) -> Self {
        Self {
            recall_calls: Mutex::new(Vec::new()),
            recall_results: Mutex::new(results),
        }
    }

    fn recorded_calls(&self) -> Vec<RecallCall> {
        self.recall_calls.lock().clone()
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
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.recall_calls.lock().push(RecallCall {
            query: query.to_string(),
            limit,
            session_id: session_id.map(String::from),
            since: since.map(String::from),
            until: until.map(String::from),
        });
        Ok(self.recall_results.lock().clone())
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
        Ok(0)
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

fn make_entry(key: &str, content: &str, category: MemoryCategory) -> MemoryEntry {
    MemoryEntry {
        id: format!("id-{key}"),
        key: key.to_string(),
        content: content.to_string(),
        category,
        timestamp: "2026-03-29T00:00:00Z".to_string(),
        session_id: None,
        score: None,
        namespace: "default".to_string(),
        importance: None,
        superseded_by: None,
    }
}

// ---------------------------------------------------------------------------
// 1. Registry wires through to the configured Memory backend for recall
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registry_memory_recall_delegates_to_backend() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let results = registry
        .memory
        .recall("test query", 10, None, None, None)
        .await
        .expect("recall should succeed");

    assert!(results.is_empty(), "empty backend returns no results");
    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 1, "exactly one recall call expected");
    assert_eq!(calls[0].query, "test query");
    assert_eq!(calls[0].limit, 10);
    assert!(calls[0].session_id.is_none());
}

#[tokio::test]
async fn registry_memory_recall_preserves_session_id() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    registry
        .memory
        .recall("session query", 5, Some("sess-99"), None, None)
        .await
        .unwrap();

    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].session_id.as_deref(), Some("sess-99"));
}

#[tokio::test]
async fn registry_memory_recall_preserves_time_range() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    registry
        .memory
        .recall(
            "time query",
            10,
            None,
            Some("2026-03-01T00:00:00Z"),
            Some("2026-03-29T23:59:59Z"),
        )
        .await
        .unwrap();

    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].since.as_deref(), Some("2026-03-01T00:00:00Z"));
    assert_eq!(calls[0].until.as_deref(), Some("2026-03-29T23:59:59Z"));
}

// ---------------------------------------------------------------------------
// 2. Recall returns matching results from the backend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recall_returns_entries_from_backend() {
    let entries = vec![
        make_entry("greeting", "Hello world", MemoryCategory::Core),
        make_entry("farewell", "Goodbye world", MemoryCategory::Daily),
    ];
    let memory = Arc::new(TrackingMemory::with_results(entries.clone()));
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let results = registry
        .memory
        .recall("world", 10, None, None, None)
        .await
        .expect("recall should succeed");

    assert_eq!(results.len(), 2, "should return both matching entries");
    assert_eq!(results[0].key, "greeting");
    assert_eq!(results[0].content, "Hello world");
    assert_eq!(results[1].key, "farewell");
    assert_eq!(results[1].content, "Goodbye world");
}

#[tokio::test]
async fn recall_preserves_entry_categories() {
    let entries = vec![
        make_entry("k1", "core content", MemoryCategory::Core),
        make_entry("k2", "daily content", MemoryCategory::Daily),
        make_entry(
            "k3",
            "custom content",
            MemoryCategory::Custom("plugin_data".into()),
        ),
    ];
    let memory = Arc::new(TrackingMemory::with_results(entries));
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let results = registry
        .memory
        .recall("content", 10, None, None, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 3);
    assert_eq!(results[0].category, MemoryCategory::Core);
    assert_eq!(results[1].category, MemoryCategory::Daily);
    assert_eq!(
        results[2].category,
        MemoryCategory::Custom("plugin_data".into())
    );
}

// ---------------------------------------------------------------------------
// 3. Multiple recalls accumulate in the call log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_recalls_recorded() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    for i in 0..3 {
        registry
            .memory
            .recall(&format!("query_{i}"), i + 1, None, None, None)
            .await
            .unwrap();
    }

    let calls = memory.recorded_calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].query, "query_0");
    assert_eq!(calls[0].limit, 1);
    assert_eq!(calls[2].query, "query_2");
    assert_eq!(calls[2].limit, 3);
}

// ---------------------------------------------------------------------------
// 4. memory_read host function is registered when read capability is set
// ---------------------------------------------------------------------------

#[test]
fn memory_read_function_registered_when_read_capability_enabled() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(true, false);

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 1, "read-only capability should yield 1 function");
}

#[test]
fn memory_read_and_write_functions_registered_when_both_enabled() {
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
// 5. memory_read NOT registered when read=false
// ---------------------------------------------------------------------------

#[test]
fn memory_read_not_registered_when_read_disabled() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(false, true);

    let fns = registry.build_functions(&manifest);
    assert_eq!(
        fns.len(),
        2,
        "write-only capability should yield 2 functions (store + forget, no memory_read)"
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
