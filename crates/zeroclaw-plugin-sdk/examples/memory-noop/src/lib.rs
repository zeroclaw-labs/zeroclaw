use std::sync::Mutex;

use zeroclaw_plugin_sdk::memory::{MemoryCapabilities, MemoryCategory, MemoryEntry, MemoryPlugin};

static ENTRIES: Mutex<Vec<MemoryEntry>> = Mutex::new(Vec::new());

struct InMemory;

impl MemoryPlugin for InMemory {
    fn plugin_info() -> (&'static str, &'static str) {
        ("memory-noop", "0.1.0")
    }

    fn name() -> String {
        "memory-noop".to_string()
    }

    fn get_memory_capabilities() -> MemoryCapabilities {
        // Implements none of the optional capabilities; relies entirely on
        // MemoryPlugin's documented stub defaults.
        MemoryCapabilities::empty()
    }

    fn store_entry(
        key: String,
        content: String,
        category: MemoryCategory,
        session_id: Option<String>,
    ) -> Result<(), String> {
        let mut entries = ENTRIES.lock().map_err(|e| e.to_string())?;
        entries.push(MemoryEntry {
            id: key.clone(),
            key,
            content,
            category,
            timestamp: "1970-01-01T00:00:00Z".to_string(),
            session_id,
            score: None,
            namespace: String::new(),
            importance: None,
            superseded_by: None,
            agent_alias: None,
            agent_id: None,
        });
        Ok(())
    }

    fn recall(
        query: String,
        limit: u64,
        _session_id: Option<String>,
        _since: Option<String>,
        _until: Option<String>,
    ) -> Result<Vec<MemoryEntry>, String> {
        let entries = ENTRIES.lock().map_err(|e| e.to_string())?;
        Ok(entries
            .iter()
            .filter(|e| query.is_empty() || query == "*" || e.content.contains(&query))
            .take(limit as usize)
            .cloned()
            .collect())
    }

    fn get(key: String) -> Result<Option<MemoryEntry>, String> {
        let entries = ENTRIES.lock().map_err(|e| e.to_string())?;
        Ok(entries.iter().find(|e| e.key == key).cloned())
    }

    fn list_entries(
        _category: Option<MemoryCategory>,
        _session_id: Option<String>,
    ) -> Result<Vec<MemoryEntry>, String> {
        let entries = ENTRIES.lock().map_err(|e| e.to_string())?;
        Ok(entries.clone())
    }

    fn forget(key: String) -> Result<bool, String> {
        let mut entries = ENTRIES.lock().map_err(|e| e.to_string())?;
        let len_before = entries.len();
        entries.retain(|e| e.key != key);
        Ok(entries.len() != len_before)
    }

    fn forget_for_agent(key: String, _agent_id: String) -> Result<bool, String> {
        Self::forget(key)
    }

    fn count() -> Result<u64, String> {
        let entries = ENTRIES.lock().map_err(|e| e.to_string())?;
        Ok(entries.len() as u64)
    }

    fn health_check() -> bool {
        true
    }

    fn store_with_agent(
        key: String,
        content: String,
        category: MemoryCategory,
        session_id: Option<String>,
        _namespace: Option<String>,
        _importance: Option<f64>,
        _agent_id: Option<String>,
    ) -> Result<(), String> {
        Self::store_entry(key, content, category, session_id)
    }

    fn recall_for_agents(
        _agents: zeroclaw_plugin_sdk::memory::AgentFilter,
        query: String,
        limit: u64,
        session_id: Option<String>,
        since: Option<String>,
        until: Option<String>,
    ) -> Result<Vec<MemoryEntry>, String> {
        Self::recall(query, limit, session_id, since, until)
    }
}

zeroclaw_plugin_sdk::export_memory!(InMemory);
