//! Shared agent state trait and in-memory implementation.
//!
//! This module provides the `SharedAgentState` trait for multi-agent
//! coordination through shared state, and a memory-backed implementation
//! for single-process coordination.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use std::fmt;

use crate::coordination::message::AgentId;

/// Display wrapper for optional expected version.
#[derive(Debug, Clone)]
pub struct ExpectedVersion(pub Option<u64>);

impl fmt::Display for ExpectedVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "{}", v),
            None => write!(f, "none"),
        }
    }
}

/// Error type for shared state operations.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("key not found: {0}")]
    KeyNotFound(String),

    #[error("version mismatch for key '{key}': expected {expected}, actual {actual}")]
    VersionMismatch {
        key: String,
        expected: ExpectedVersion,
        actual: u64,
    },

    #[error("state closed")]
    Closed,

    #[error("invalid key: {0}")]
    InvalidKey(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for state operations.
pub type StateResult<T> = Result<T, StateError>;

/// Shared value with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedValue {
    /// The stored data.
    pub data: serde_json::Value,
    /// Version number for optimistic locking.
    pub version: u64,
    /// When this value was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When this value was last updated.
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Which agent created this value.
    pub created_by: AgentId,
}

impl SharedValue {
    /// Create a new shared value.
    pub fn new(created_by: AgentId, data: serde_json::Value) -> Self {
        let now = chrono::Utc::now();
        Self {
            data,
            version: 1,
            created_at: now,
            updated_at: now,
            created_by,
        }
    }

    /// Update the value with a new version.
    fn update(&mut self, data: serde_json::Value) {
        self.data = data;
        self.version += 1;
        self.updated_at = chrono::Utc::now();
    }
}

/// Shared state for multi-agent coordination.
///
/// This trait defines the interface for shared state access across agents.
/// Implementations can use different backends (memory, SQLite, Redis) while
/// providing the same API.
#[async_trait]
pub trait SharedAgentState: Send + Sync {
    /// Get a value by key.
    async fn get(&self, key: &str) -> StateResult<Option<SharedValue>>;

    /// Set a value (creates or updates).
    async fn set(&self, key: String, value: SharedValue) -> StateResult<()>;

    /// Compare-and-swap: update only if current value matches expected.
    async fn cas(
        &self,
        key: String,
        expected: Option<SharedValue>,
        new: SharedValue,
    ) -> StateResult<bool>;

    /// Delete a key.
    async fn delete(&self, key: &str) -> StateResult<bool>;

    /// List all keys (optionally filtered by prefix).
    async fn list(&self, prefix: Option<&str>) -> StateResult<Vec<String>>;

    /// Watch for changes to a key (returns a stream).
    async fn watch(&self, key: String) -> StateResult<broadcast::Receiver<SharedValue>>;
}

/// Internal state entry.
#[derive(Debug, Clone)]
struct StateEntry {
    value: SharedValue,
    watchers: Vec<broadcast::Sender<SharedValue>>,
}

/// In-memory shared state implementation.
///
/// This implementation stores state in memory and provides
/// efficient intra-process coordination between agents.
#[derive(Debug, Clone)]
pub struct MemorySharedState {
    inner: Arc<Mutex<MemoryStateInner>>,
}

#[derive(Debug)]
struct MemoryStateInner {
    entries: HashMap<String, StateEntry>,
}

impl Default for MemorySharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl MemorySharedState {
    /// Create a new in-memory shared state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryStateInner {
                entries: HashMap::new(),
            })),
        }
    }

    /// Get the number of stored keys.
    pub async fn key_count(&self) -> usize {
        let inner = self.inner.lock().await;
        inner.entries.len()
    }

    /// Get all keys and values.
    pub async fn snapshot(&self) -> HashMap<String, SharedValue> {
        let inner = self.inner.lock().await;
        inner
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.value.clone()))
            .collect()
    }

    /// Clear all entries.
    pub async fn clear(&self) {
        let mut inner = self.inner.lock().await;
        inner.entries.clear();
    }

    /// Validate a key string.
    fn validate_key(key: &str) -> StateResult<()> {
        if key.is_empty() {
            return Err(StateError::InvalidKey("key cannot be empty".to_string()));
        }
        if key.len() > 256 {
            return Err(StateError::InvalidKey("key too long (max 256 chars)".to_string()));
        }
        // Check for valid characters (printable, no control chars)
        if !key.chars().all(|c| c.is_ascii_graphic() || c == ' ' || c == '_' || c == '-' || c == '/' || c == ':') {
            return Err(StateError::InvalidKey("key contains invalid characters".to_string()));
        }
        Ok(())
    }
}

#[async_trait]
impl SharedAgentState for MemorySharedState {
    async fn get(&self, key: &str) -> StateResult<Option<SharedValue>> {
        Self::validate_key(key)?;
        let inner = self.inner.lock().await;
        Ok(inner.entries.get(key).map(|e| e.value.clone()))
    }

    async fn set(&self, key: String, value: SharedValue) -> StateResult<()> {
        Self::validate_key(&key)?;

        let mut inner = self.inner.lock().await;
        let entry = inner.entries.entry(key.clone()).or_insert_with(|| {
            StateEntry {
                value: value.clone(),
                watchers: Vec::new(),
            }
        });

        // Update existing entry
        entry.value = value.clone();

        // Notify watchers
        entry.watchers.retain(|tx| {
            tx.send(value.clone()).is_ok()
        });

        Ok(())
    }

    async fn cas(
        &self,
        key: String,
        expected: Option<SharedValue>,
        new: SharedValue,
    ) -> StateResult<bool> {
        Self::validate_key(&key)?;

        let mut inner = self.inner.lock().await;

        let current = inner.entries.get(&key);

        match (expected, current) {
            (None, None) => {
                // Key doesn't exist and we expect it to not exist - create it
                let entry = StateEntry {
                    value: new.clone(),
                    watchers: Vec::new(),
                };
                inner.entries.insert(key, entry);
                Ok(true)
            }
            (Some(exp), None) => {
                // We expect a value but key doesn't exist
                Err(StateError::VersionMismatch {
                    key,
                    expected: ExpectedVersion(Some(exp.version)),
                    actual: 0,
                })
            }
            (None, Some(curr)) => {
                // We expect no value but key exists
                Err(StateError::VersionMismatch {
                    key,
                    expected: ExpectedVersion(None),
                    actual: curr.value.version,
                })
            }
            (Some(exp), Some(curr)) => {
                if exp.version == curr.value.version {
                    // Version matches - update
                    let entry = inner.entries.get_mut(&key).unwrap();
                    entry.value = new.clone();

                    // Notify watchers
                    entry.watchers.retain(|tx| {
                        tx.send(new.clone()).is_ok()
                    });

                    Ok(true)
                } else {
                    // Version mismatch
                    Err(StateError::VersionMismatch {
                        key,
                        expected: ExpectedVersion(Some(exp.version)),
                        actual: curr.value.version,
                    })
                }
            }
        }
    }

    async fn delete(&self, key: &str) -> StateResult<bool> {
        Self::validate_key(key)?;
        let mut inner = self.inner.lock().await;
        Ok(inner.entries.remove(key).is_some())
    }

    async fn list(&self, prefix: Option<&str>) -> StateResult<Vec<String>> {
        let inner = self.inner.lock().await;
        let mut keys: Vec<String> = inner
            .entries
            .keys()
            .filter(|k| prefix.map_or(true, |p| k.starts_with(p)))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }

    async fn watch(&self, key: String) -> StateResult<broadcast::Receiver<SharedValue>> {
        Self::validate_key(&key)?;

        let (tx, rx) = broadcast::channel(100);

        let mut inner = self.inner.lock().await;
        let entry = inner.entries.entry(key.clone()).or_insert_with(|| {
            StateEntry {
                value: SharedValue::new(
                    AgentId::new("system".to_string()),
                    serde_json::Value::Null,
                ),
                watchers: Vec::new(),
            }
        });

        entry.watchers.push(tx);
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::time::{sleep, Duration, timeout};

    #[tokio::test]
    async fn set_and_get_value() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());
        let value = SharedValue::new(agent_id, serde_json::json!("test_value"));

        state.set("key1".to_string(), value.clone()).await.unwrap();

        let retrieved = state.get("key1").await.unwrap().unwrap();
        assert_eq!(retrieved.data, value.data);
        assert_eq!(retrieved.version, 1);
    }

    #[tokio::test]
    async fn get_nonexistent_key_returns_none() {
        let state = MemorySharedState::new();
        let result = state.get("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn delete_existing_key() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());
        let value = SharedValue::new(agent_id, serde_json::json!("test"));

        state.set("key1".to_string(), value).await.unwrap();
        assert_eq!(state.key_count().await, 1);

        let deleted = state.delete("key1").await.unwrap();
        assert!(deleted);
        assert_eq!(state.key_count().await, 0);
    }

    #[tokio::test]
    async fn delete_nonexistent_key_returns_false() {
        let state = MemorySharedState::new();
        let deleted = state.delete("nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn list_all_keys() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());

        for i in 0..5 {
            let value = SharedValue::new(agent_id.clone(), serde_json::json!(i));
            state.set(format!("key{}", i), value).await.unwrap();
        }

        let keys = state.list(None).await.unwrap();
        assert_eq!(keys.len(), 5);
    }

    #[tokio::test]
    async fn list_keys_with_prefix() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());

        state.set("task:1".to_string(), SharedValue::new(agent_id.clone(), serde_json::json!(1))).await.unwrap();
        state.set("task:2".to_string(), SharedValue::new(agent_id.clone(), serde_json::json!(2))).await.unwrap();
        state.set("other:1".to_string(), SharedValue::new(agent_id.clone(), serde_json::json!(3))).await.unwrap();

        let task_keys = state.list(Some("task:")).await.unwrap();
        assert_eq!(task_keys.len(), 2);
        assert!(task_keys.contains(&"task:1".to_string()));
        assert!(task_keys.contains(&"task:2".to_string()));
    }

    #[tokio::test]
    async fn cas_creates_new_key_when_expected_is_none() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());
        let value = SharedValue::new(agent_id, serde_json::json!("new"));

        let result = state.cas("new_key".to_string(), None, value).await.unwrap();
        assert!(result);
        assert_eq!(state.key_count().await, 1);
    }

    #[tokio::test]
    async fn cas_fails_when_version_mismatch() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());
        let value = SharedValue::new(agent_id.clone(), serde_json::json!("original"));

        state.set("key1".to_string(), value.clone()).await.unwrap();

        // Try to update with wrong version
        let mut wrong_version = value.clone();
        wrong_version.version = 999;
        let new_value = SharedValue::new(agent_id, serde_json::json!("updated"));

        let result = state.cas("key1".to_string(), Some(wrong_version), new_value).await;
        assert!(matches!(result, Err(StateError::VersionMismatch { .. })));
    }

    #[tokio::test]
    async fn cas_succeeds_when_version_matches() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());
        let value = SharedValue::new(agent_id.clone(), serde_json::json!("original"));

        state.set("key1".to_string(), value.clone()).await.unwrap();

        // Update with correct version
        let new_value = SharedValue::new(agent_id, serde_json::json!("updated"));
        let result = state.cas("key1".to_string(), Some(value), new_value.clone()).await.unwrap();
        assert!(result);

        let retrieved = state.get("key1").await.unwrap().unwrap();
        assert_eq!(retrieved.data, new_value.data);
        assert_eq!(retrieved.version, 2);
    }

    #[tokio::test]
    async fn cas_fails_when_key_exists_and_expected_is_none() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());
        let value = SharedValue::new(agent_id.clone(), serde_json::json!("existing"));

        state.set("key1".to_string(), value).await.unwrap();

        let new_value = SharedValue::new(agent_id, serde_json::json!("new"));
        let result = state.cas("key1".to_string(), None, new_value).await;
        assert!(matches!(result, Err(StateError::VersionMismatch { .. })));
    }

    #[tokio::test]
    async fn cas_task_claiming_pattern() {
        let state = MemorySharedState::new();
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        // Agent A claims task_123
        let value_a = SharedValue::new(
            agent_a.clone(),
            serde_json::json!({"status": "claimed", "agent": "agent_a"}),
        );
        let claimed_a = state
            .cas("task_123".to_string(), None, value_a)
            .await
            .unwrap();
        assert!(claimed_a);

        // Agent B tries to claim same task (should fail)
        let value_b = SharedValue::new(
            agent_b.clone(),
            serde_json::json!({"status": "claimed", "agent": "agent_b"}),
        );
        let claimed_b = state
            .cas("task_123".to_string(), None, value_b)
            .await
            .unwrap();
        assert!(!claimed_b);

        // Agent B can update with correct CAS
        let current = state.get("task_123").await.unwrap().unwrap();
        let updated = SharedValue::new(
            agent_b.clone(),
            serde_json::json!({"status": "completed", "agent": "agent_a"}),
        );
        let updated_ok = state
            .cas("task_123".to_string(), Some(current), updated)
            .await
            .unwrap();
        assert!(updated_ok);
    }

    #[tokio::test]
    async fn watch_receives_updates() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());

        let mut rx = state.watch("watch_key".to_string()).await.unwrap();

        // Spawn a task to update the value
        let state_clone = state.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            let value = SharedValue::new(agent_id.clone(), serde_json::json!("updated"));
            state_clone.set("watch_key".to_string(), value).await.unwrap();
        });

        // Wait for the update
        let received = timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
        assert_eq!(received.data, serde_json::json!("updated"));
    }

    #[tokio::test]
    async fn empty_key_returns_error() {
        let state = MemorySharedState::new();
        let result = state.get("").await;
        assert!(matches!(result, Err(StateError::InvalidKey(_))));
    }

    #[tokio::test]
    async fn set_empty_key_returns_error() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());
        let value = SharedValue::new(agent_id.clone(), serde_json::json!("test"));

        let result = state.set("".to_string(), value).await;
        assert!(matches!(result, Err(StateError::InvalidKey(_))));
    }

    #[tokio::test]
    async fn snapshot_returns_all_entries() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());

        for i in 1..=3 {
            let value = SharedValue::new(agent_id.clone(), serde_json::json!(i));
            state.set(format!("key{}", i), value).await.unwrap();
        }

        let snapshot = state.snapshot().await;
        assert_eq!(snapshot.len(), 3);
    }

    #[tokio::test]
    async fn clear_removes_all_entries() {
        let state = MemorySharedState::new();
        let agent_id = AgentId::new("agent_test".to_string());

        let value = SharedValue::new(agent_id, serde_json::json!("test"));
        state.set("key1".to_string(), value).await.unwrap();

        assert_eq!(state.key_count().await, 1);
        state.clear().await;
        assert_eq!(state.key_count().await, 0);
    }
}
