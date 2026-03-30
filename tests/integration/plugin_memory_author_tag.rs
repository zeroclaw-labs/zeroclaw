//! Verify that all memory writes are tagged with the plugin name as author.
//!
//! Acceptance criterion for US-ZCL-23:
//! > All memory writes tagged with plugin name as author
//!
//! These tests assert that:
//! 1. `HostFunctionRegistry::tagged_key` prefixes keys with `plugin:<name>:`
//! 2. Different plugins produce distinct key prefixes
//! 3. The original key is preserved after the prefix
//! 4. A full store round-trip through the registry with a tagged key records the
//!    plugin name in the stored entry's key

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
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
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(AuditLogger::new(cfg, path).expect("audit logger"))
}

// ---------------------------------------------------------------------------
// 1. tagged_key produces the expected prefix format
// ---------------------------------------------------------------------------

#[test]
fn tagged_key_prefixes_with_plugin_name() {
    let tagged = HostFunctionRegistry::tagged_key("weather_plugin", "forecast");
    assert_eq!(tagged, "plugin:weather_plugin:forecast");
}

#[test]
fn tagged_key_preserves_original_key() {
    let tagged = HostFunctionRegistry::tagged_key("my_plugin", "some/nested/key");
    assert!(tagged.ends_with("some/nested/key"));
}

#[test]
fn tagged_key_different_plugins_produce_distinct_prefixes() {
    let a = HostFunctionRegistry::tagged_key("plugin_a", "shared_key");
    let b = HostFunctionRegistry::tagged_key("plugin_b", "shared_key");
    assert_ne!(a, b, "different plugins must produce different tagged keys");
    assert!(a.starts_with("plugin:plugin_a:"));
    assert!(b.starts_with("plugin:plugin_b:"));
}

#[test]
fn tagged_key_empty_key_still_includes_prefix() {
    let tagged = HostFunctionRegistry::tagged_key("my_plugin", "");
    assert_eq!(tagged, "plugin:my_plugin:");
}

// ---------------------------------------------------------------------------
// 2. Full store round-trip with tagged key records plugin name
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_with_tagged_key_records_plugin_name_in_backend() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let plugin_name = "weather_plugin";
    let tagged = HostFunctionRegistry::tagged_key(plugin_name, "forecast");

    registry
        .memory
        .store(&tagged, "sunny", MemoryCategory::Core, None)
        .await
        .expect("store should succeed");

    let entries = memory.stored_entries();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].key.starts_with(&format!("plugin:{plugin_name}:")),
        "stored key must start with plugin author prefix, got: {}",
        entries[0].key,
    );
    assert_eq!(entries[0].key, "plugin:weather_plugin:forecast");
}

#[tokio::test]
async fn multiple_plugins_store_with_distinct_author_tags() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let key_a = HostFunctionRegistry::tagged_key("plugin_alpha", "data");
    let key_b = HostFunctionRegistry::tagged_key("plugin_beta", "data");

    registry
        .memory
        .store(&key_a, "alpha_content", MemoryCategory::Core, None)
        .await
        .unwrap();
    registry
        .memory
        .store(&key_b, "beta_content", MemoryCategory::Core, None)
        .await
        .unwrap();

    let entries = memory.stored_entries();
    assert_eq!(entries.len(), 2);
    assert!(entries[0].key.contains("plugin_alpha"));
    assert!(entries[1].key.contains("plugin_beta"));
    assert_ne!(entries[0].key, entries[1].key);
}

#[tokio::test]
async fn tagged_store_preserves_content_and_category() {
    let memory = Arc::new(TrackingMemory::new());
    let registry = HostFunctionRegistry::new(memory.clone(), vec![], make_audit());

    let tagged = HostFunctionRegistry::tagged_key("sensor_plugin", "temperature");

    registry
        .memory
        .store(
            &tagged,
            "22.5°C",
            MemoryCategory::Custom("sensor_data".into()),
            Some("sess-99"),
        )
        .await
        .unwrap();

    let entries = memory.stored_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content, "22.5°C");
    assert_eq!(entries[0].category, "sensor_data");
    assert_eq!(entries[0].session_id.as_deref(), Some("sess-99"));
    assert!(entries[0].key.starts_with("plugin:sensor_plugin:"));
}
