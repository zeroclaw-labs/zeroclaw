//! Minimal memory component used by the plugin-host memory tests.
//!
//! Entries live in this instance's own linear memory, as every memory plugin's
//! must today: the memory world imports no host storage. A fresh instance has
//! therefore forgotten everything an earlier one stored.
//!
//! `store-entry` takes its behavior from the content it is asked to store:
//!
//! - `memory:reject` returns an error string of the plugin's own;
//! - `memory:trap` traps, as a buggy plugin's would;
//! - `memory:spin` computes until the host's deadline or fuel runs out.
//!
//! Any other content is stored under its key.

#[cfg(target_family = "wasm")]
mod component {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "memory-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::memory::{
        AgentFilter, ExportFilter, Guest as Memory, MemoryCapabilities, MemoryCategory,
        MemoryEntry, ProceduralMessage,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;

    const NOT_SUPPORTED: &str = "not-supported";

    struct FixtureMemory;

    thread_local! {
        /// Every stored entry, by key.
        static ENTRIES: RefCell<BTreeMap<String, MemoryEntry>> =
            const { RefCell::new(BTreeMap::new()) };
    }

    fn insert(
        key: String,
        content: String,
        category: MemoryCategory,
        session_id: Option<String>,
        agent_id: Option<String>,
    ) -> Result<(), String> {
        match content.as_str() {
            "memory:reject" => return Err("fixture refuses this entry".to_string()),
            "memory:trap" => panic!("fixture store-entry traps on request"),
            "memory:spin" => {
                let mut value = 0_u64;
                loop {
                    value = std::hint::black_box(value.wrapping_add(1));
                }
            }
            _ => {}
        }
        let entry = MemoryEntry {
            id: key.clone(),
            key: key.clone(),
            content,
            category,
            timestamp: "1970-01-01T00:00:00Z".to_string(),
            session_id,
            score: None,
            namespace: "default".to_string(),
            importance: None,
            superseded_by: None,
            agent_alias: agent_id.clone(),
            agent_id,
        };
        ENTRIES.with_borrow_mut(|entries| entries.insert(key, entry));
        Ok(())
    }

    /// Entries whose content contains `query` (every entry for an empty or
    /// `*` query) in `session_id`, when one is given.
    fn matching(
        query: &str,
        limit: u64,
        session_id: Option<&str>,
        agents: &AgentFilter,
    ) -> Vec<MemoryEntry> {
        ENTRIES.with_borrow(|entries| {
            entries
                .values()
                .filter(|entry| query.is_empty() || query == "*" || entry.content.contains(query))
                .filter(|entry| session_id.is_none_or(|id| entry.session_id.as_deref() == Some(id)))
                .filter(|entry| match agents {
                    AgentFilter::All => true,
                    AgentFilter::Some(ids) => entry
                        .agent_id
                        .as_ref()
                        .is_some_and(|agent| ids.contains(agent)),
                })
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .cloned()
                .collect()
        })
    }

    fn same_category(left: &MemoryCategory, right: &MemoryCategory) -> bool {
        match (left, right) {
            (MemoryCategory::Core, MemoryCategory::Core)
            | (MemoryCategory::Daily, MemoryCategory::Daily)
            | (MemoryCategory::Conversation, MemoryCategory::Conversation) => true,
            (MemoryCategory::Custom(left), MemoryCategory::Custom(right)) => left == right,
            _ => false,
        }
    }

    impl PluginInfo for FixtureMemory {
        fn plugin_name() -> String {
            "memory-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Memory for FixtureMemory {
        fn name() -> String {
            "memory-fixture".to_string()
        }

        fn get_memory_capabilities() -> MemoryCapabilities {
            MemoryCapabilities::empty()
        }

        fn store_entry(
            key: String,
            content: String,
            category: MemoryCategory,
            session_id: Option<String>,
        ) -> Result<(), String> {
            insert(key, content, category, session_id, None)
        }

        fn recall(
            query: String,
            limit: u64,
            session_id: Option<String>,
            _since: Option<String>,
            _until: Option<String>,
        ) -> Result<Vec<MemoryEntry>, String> {
            Ok(matching(
                &query,
                limit,
                session_id.as_deref(),
                &AgentFilter::All,
            ))
        }

        fn get(key: String) -> Result<Option<MemoryEntry>, String> {
            Ok(ENTRIES.with_borrow(|entries| entries.get(&key).cloned()))
        }

        fn list_entries(
            category: Option<MemoryCategory>,
            session_id: Option<String>,
        ) -> Result<Vec<MemoryEntry>, String> {
            Ok(
                matching("", u64::MAX, session_id.as_deref(), &AgentFilter::All)
                    .into_iter()
                    .filter(|entry| {
                        category
                            .as_ref()
                            .is_none_or(|wanted| same_category(wanted, &entry.category))
                    })
                    .collect(),
            )
        }

        fn forget(key: String) -> Result<bool, String> {
            Ok(ENTRIES.with_borrow_mut(|entries| entries.remove(&key).is_some()))
        }

        fn forget_for_agent(key: String, agent_id: String) -> Result<bool, String> {
            Ok(ENTRIES.with_borrow_mut(|entries| {
                let owned = entries
                    .get(&key)
                    .is_some_and(|entry| entry.agent_id.as_deref() == Some(agent_id.as_str()));
                owned && entries.remove(&key).is_some()
            }))
        }

        fn count() -> Result<u64, String> {
            Ok(ENTRIES.with_borrow(|entries| entries.len() as u64))
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
            agent_id: Option<String>,
        ) -> Result<(), String> {
            insert(key, content, category, session_id, agent_id)
        }

        fn recall_for_agents(
            agents: AgentFilter,
            query: String,
            limit: u64,
            session_id: Option<String>,
            _since: Option<String>,
            _until: Option<String>,
        ) -> Result<Vec<MemoryEntry>, String> {
            Ok(matching(&query, limit, session_id.as_deref(), &agents))
        }

        // The capability-gated exports below are stubs: the fixture
        // advertises no optional capability, so the host never calls them.

        fn get_for_agent(_key: String, _agent_id: String) -> Result<Option<MemoryEntry>, String> {
            Err(NOT_SUPPORTED.to_string())
        }

        fn purge_namespace(_namespace: String) -> Result<u64, String> {
            Err(NOT_SUPPORTED.to_string())
        }

        fn purge_session(_session_id: String) -> Result<u64, String> {
            Err(NOT_SUPPORTED.to_string())
        }

        fn purge_session_for_agent(_session_id: String, _agent_id: String) -> Result<u64, String> {
            Err(NOT_SUPPORTED.to_string())
        }

        fn purge_agent(_agent_alias: String) -> Result<u64, String> {
            Err(NOT_SUPPORTED.to_string())
        }

        fn reindex() -> Result<u64, String> {
            Ok(0)
        }

        fn store_procedural(
            _messages: Vec<ProceduralMessage>,
            _session_id: Option<String>,
        ) -> Result<(), String> {
            Ok(())
        }

        fn ensure_agent_uuid(alias: String) -> Result<String, String> {
            Ok(alias)
        }

        fn recall_namespaced(
            _namespace: String,
            _query: String,
            _limit: u64,
            _session_id: Option<String>,
            _since: Option<String>,
            _until: Option<String>,
        ) -> Result<Vec<MemoryEntry>, String> {
            Err(NOT_SUPPORTED.to_string())
        }

        fn export_entries(_filter: ExportFilter) -> Result<Vec<MemoryEntry>, String> {
            Err(NOT_SUPPORTED.to_string())
        }

        fn store_with_metadata(
            _key: String,
            _content: String,
            _category: MemoryCategory,
            _session_id: Option<String>,
            _namespace: Option<String>,
            _importance: Option<f64>,
        ) -> Result<(), String> {
            Err(NOT_SUPPORTED.to_string())
        }
    }

    export!(FixtureMemory);
}
