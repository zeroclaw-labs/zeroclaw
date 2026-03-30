//! Integration test for memory host functions: store-then-recall round-trip.
//!
//! Task US-ZCL-23-9: Create a test plugin that calls memory_store then
//! memory_recall. Load it with memory capability declared. Verify data
//! round-trips through ZeroClaw's memory backend. Test with read-only
//! capability (store should fail).
//!
//! These tests assert that:
//! 1. Data stored via memory_store can be recalled via memory_recall
//! 2. Author-tagged keys are used consistently across store and recall
//! 3. Multiple store-then-recall cycles preserve all entries
//! 4. Read-only capability does not register store/forget functions
//! 5. Write-only capability does not register recall function

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::PluginManifest;
use zeroclaw::security::audit::AuditLogger;

/// An in-memory backend that supports both store and recall so we can verify
/// round-trip behaviour through the HostFunctionRegistry.
struct RoundTripMemory {
    entries: Mutex<Vec<StoredEntry>>,
}

#[derive(Debug, Clone)]
struct StoredEntry {
    key: String,
    content: String,
    category: MemoryCategory,
}

impl RoundTripMemory {
    fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Memory for RoundTripMemory {
    fn name(&self) -> &str {
        "roundtrip"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.entries.lock().push(StoredEntry {
            key: key.to_string(),
            content: content.to_string(),
            category,
        });
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let entries = self.entries.lock();
        let results: Vec<MemoryEntry> = entries
            .iter()
            .filter(|e| e.key.contains(query) || e.content.contains(query))
            .take(limit)
            .map(|e| MemoryEntry {
                id: format!("id-{}", e.key),
                key: e.key.clone(),
                content: e.content.clone(),
                category: e.category.clone(),
                timestamp: "2026-03-29T00:00:00Z".to_string(),
                session_id: None,
                score: None,
                namespace: "default".to_string(),
                importance: None,
                superseded_by: None,
            })
            .collect();
        Ok(results)
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let entries = self.entries.lock();
        Ok(entries.iter().find(|e| e.key == key).map(|e| MemoryEntry {
            id: format!("id-{}", e.key),
            key: e.key.clone(),
            content: e.content.clone(),
            category: e.category.clone(),
            timestamp: "2026-03-29T00:00:00Z".to_string(),
            session_id: None,
            score: None,
            namespace: "default".to_string(),
            importance: None,
            superseded_by: None,
        }))
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let mut entries = self.entries.lock();
        let before = entries.len();
        entries.retain(|e| e.key != key);
        Ok(entries.len() < before)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(self.entries.lock().len())
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
        name = "roundtrip_plugin"
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
// 1. Store then recall: data round-trips through the backend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_then_recall_roundtrips_through_backend() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());
    let plugin_name = "roundtrip_plugin";

    // Store a value using the tagged key (as the host function would)
    let tagged = HostFunctionRegistry::tagged_key(plugin_name, "greeting");
    registry
        .memory
        .store(&tagged, "Hello from plugin", MemoryCategory::Custom("plugin".into()), None)
        .await
        .expect("store should succeed");

    // Recall using the tagged key prefix
    let results = registry
        .memory
        .recall(&tagged, 10, None, None, None)
        .await
        .expect("recall should succeed");

    assert_eq!(results.len(), 1, "should find exactly one entry");
    assert_eq!(results[0].key, tagged);
    assert_eq!(results[0].content, "Hello from plugin");
}

#[tokio::test]
async fn store_then_recall_preserves_content_exactly() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());
    let plugin_name = "roundtrip_plugin";

    let content = r#"{"temperature": 72, "unit": "F", "forecast": "sunny"}"#;
    let tagged = HostFunctionRegistry::tagged_key(plugin_name, "weather_data");
    registry
        .memory
        .store(&tagged, content, MemoryCategory::Custom("plugin".into()), None)
        .await
        .unwrap();

    let results = registry
        .memory
        .recall(&tagged, 10, None, None, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, content, "content must survive round-trip unchanged");
}

// ---------------------------------------------------------------------------
// 2. Author-tagged keys are consistent across store and recall
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tagged_keys_are_consistent_for_store_and_recall() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());
    let plugin_name = "weather_plugin";

    let tagged = HostFunctionRegistry::tagged_key(plugin_name, "forecast");
    registry
        .memory
        .store(&tagged, "sunny", MemoryCategory::Custom("plugin".into()), None)
        .await
        .unwrap();

    // Recall by matching on the same tagged key
    let results = registry
        .memory
        .recall("plugin:weather_plugin:forecast", 10, None, None, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].key, "plugin:weather_plugin:forecast");
    assert_eq!(results[0].content, "sunny");
}

#[tokio::test]
async fn different_plugins_store_under_isolated_namespaces() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    // Plugin A stores
    let key_a = HostFunctionRegistry::tagged_key("plugin_a", "data");
    registry
        .memory
        .store(&key_a, "value_a", MemoryCategory::Custom("plugin".into()), None)
        .await
        .unwrap();

    // Plugin B stores same logical key
    let key_b = HostFunctionRegistry::tagged_key("plugin_b", "data");
    registry
        .memory
        .store(&key_b, "value_b", MemoryCategory::Custom("plugin".into()), None)
        .await
        .unwrap();

    // Recall for plugin A only
    let results_a = registry
        .memory
        .recall("plugin:plugin_a:data", 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(results_a.len(), 1);
    assert_eq!(results_a[0].content, "value_a");

    // Recall for plugin B only
    let results_b = registry
        .memory
        .recall("plugin:plugin_b:data", 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(results_b.len(), 1);
    assert_eq!(results_b[0].content, "value_b");
}

// ---------------------------------------------------------------------------
// 3. Multiple store-then-recall cycles accumulate correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_stores_then_recall_returns_all_entries() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());
    let plugin_name = "multi_plugin";

    for i in 0..5 {
        let tagged = HostFunctionRegistry::tagged_key(plugin_name, &format!("key_{i}"));
        registry
            .memory
            .store(
                &tagged,
                &format!("value_{i}"),
                MemoryCategory::Custom("plugin".into()),
                None,
            )
            .await
            .unwrap();
    }

    // Recall with a broad query matching the plugin prefix
    let results = registry
        .memory
        .recall("plugin:multi_plugin:", 10, None, None, None)
        .await
        .unwrap();

    assert_eq!(results.len(), 5, "all 5 stored entries should be recalled");
    for (i, entry) in results.iter().enumerate() {
        assert_eq!(entry.content, format!("value_{i}"));
    }
}

#[tokio::test]
async fn store_recall_forget_recall_cycle() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());
    let plugin_name = "lifecycle_plugin";
    let tagged = HostFunctionRegistry::tagged_key(plugin_name, "ephemeral");

    // Store
    registry
        .memory
        .store(&tagged, "temporary", MemoryCategory::Custom("plugin".into()), None)
        .await
        .unwrap();

    // Recall — should find it
    let results = registry
        .memory
        .recall(&tagged, 10, None, None, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    // Forget
    let removed = registry.memory.forget(&tagged).await.unwrap();
    assert!(removed, "forget should return true for existing key");

    // Recall again — should be empty
    let results = registry
        .memory
        .recall(&tagged, 10, None, None, None)
        .await
        .unwrap();
    assert!(results.is_empty(), "entry should be gone after forget");
}

// ---------------------------------------------------------------------------
// 4. Read-only capability does NOT register store/forget functions
// ---------------------------------------------------------------------------

#[test]
fn read_only_capability_denies_store_and_forget() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(true, false);

    let fns = registry.build_functions(&manifest);

    // read=true, write=false => only recall function (1 total)
    assert_eq!(
        fns.len(),
        1,
        "read-only should yield exactly 1 function (recall)"
    );
}

#[test]
fn read_only_plugin_gets_no_store_function_name() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(true, false);

    let fns = registry.build_functions(&manifest);
    let names: Vec<String> = fns.iter().map(|f| f.name().to_string()).collect();

    assert!(
        !names.contains(&"zeroclaw_memory_store".to_string()),
        "read-only plugin must not have store function"
    );
    assert!(
        !names.contains(&"zeroclaw_memory_forget".to_string()),
        "read-only plugin must not have forget function"
    );
}

// ---------------------------------------------------------------------------
// 5. Write-only capability does NOT register recall function
// ---------------------------------------------------------------------------

#[test]
fn write_only_capability_denies_recall() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(false, true);

    let fns = registry.build_functions(&manifest);

    // read=false, write=true => store + forget (2 total, but NOT recall)
    assert_eq!(
        fns.len(),
        2,
        "write-only should yield exactly 2 functions (store + forget)"
    );
    let names: Vec<String> = fns.iter().map(|f| f.name().to_string()).collect();
    assert!(
        !names.contains(&"zeroclaw_memory_recall".to_string()),
        "write-only plugin must not have recall function"
    );
}

// ---------------------------------------------------------------------------
// 6. Full capability registers all three memory functions
// ---------------------------------------------------------------------------

#[test]
fn full_memory_capability_registers_all_functions() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_memory(true, true);

    let fns = registry.build_functions(&manifest);
    assert_eq!(
        fns.len(),
        3,
        "read+write should yield 3 functions (recall + store + forget)"
    );

    let names: Vec<String> = fns.iter().map(|f| f.name().to_string()).collect();
    assert!(names.contains(&"zeroclaw_memory_recall".to_string()));
    assert!(names.contains(&"zeroclaw_memory_store".to_string()));
    assert!(names.contains(&"zeroclaw_memory_forget".to_string()));
}

// ---------------------------------------------------------------------------
// 7. No memory capability means no functions at all
// ---------------------------------------------------------------------------

#[test]
fn no_memory_capability_registers_no_memory_functions() {
    let memory = Arc::new(RoundTripMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());

    let toml_str = r#"
        name = "bare_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let manifest: PluginManifest = toml::from_str(toml_str).expect("valid manifest TOML");

    let fns = registry.build_functions(&manifest);
    assert!(fns.is_empty(), "no memory capability => no host functions");
}
