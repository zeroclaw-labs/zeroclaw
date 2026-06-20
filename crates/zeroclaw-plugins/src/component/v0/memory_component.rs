// Memory adapter: `ComponentMemory` implements `zeroclaw_api::memory_traits::Memory`
// backed by a WIT component-model plugin (the `memory-plugin` world in
// `wit/memory.wit`).
//
// Instance lifecycle: warm — the `Store` and `MemoryPlugin` bindings are
// created once at construction and held in an `Arc<Mutex<...>>`. Memory backends
// are long-lived and called at high frequency; re-instantiation per call would
// be prohibitively expensive.
//
// Type conversions are in-module. The canonical source of truth for all Rust
// types remains in `zeroclaw-api`; WIT types produced by bindgen! are converted
// on every crossing.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use zeroclaw_api::attribution::{Attributable, MemoryKind, Role};
use zeroclaw_api::memory_traits::{ExportFilter, MemoryCategory, MemoryEntry, ProceduralMessage};

use super::bindings::memory::{
    MemoryPlugin,
    exports::zeroclaw::plugin::memory::{
        AgentFilter as WitAgentFilter, ExportFilter as WitExportFilter, MemoryCapabilities,
        MemoryCategory as WitMemoryCategory, MemoryEntry as WitMemoryEntry,
        ProceduralMessage as WitProceduralMessage,
    },
};
use super::plugin_store::{self, PluginStore};
use crate::component::engine::ComponentEngine;
use crate::error::PluginError;
use crate::{FineGrainedPermission, call_plugin};

// ── Attributable ──────────────────────────────────────────────────────────────

/// A memory backend backed by a WIT Component Model plugin (WASIP2 ABI).
pub struct ComponentMemory {
    alias: String,
    capabilities: MemoryCapabilities,
    state: Arc<Mutex<(wasmtime::Store<PluginStore>, MemoryPlugin)>>,
    /// Canonical plugin name as self-reported by `plugin-info`. Source of truth.
    plugin_name: String,
    /// Plugin version string as self-reported by `plugin-info`. Source of truth.
    plugin_version: String,
}

impl Attributable for ComponentMemory {
    fn role(&self) -> Role {
        Role::Memory(MemoryKind::Plugin)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

// ── Construction ──────────────────────────────────────────────────────────────

impl ComponentMemory {
    /// Compile and instantiate a memory plugin from raw WASM bytes.
    ///
    /// Calls `get-memory-capabilities` once and stores the result. The returned
    /// `ComponentMemory` is ready to use immediately.
    ///
    /// `permissions` is applied to the long-lived store so that filesystem,
    /// TCP, UDP, and HTTP access are restricted to the declared
    /// `fine_grained_permissions` list.
    pub async fn from_bytes(
        alias: impl Into<String>,
        engine: &Arc<ComponentEngine>,
        bytes: &[u8],
        permissions: &[FineGrainedPermission],
        network_config: crate::PluginNetworkConfig,
    ) -> Result<Self, PluginError> {
        let component = engine.compile(bytes)?;
        let mut linker = wasmtime::component::Linker::<PluginStore>::new(engine.engine());
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(PluginError::from)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(PluginError::from)?;
        plugin_store::add_to_linker_memory(&mut linker)?;
        let host = PluginStore::with_permissions(permissions, &network_config).await?;
        let mut store = wasmtime::Store::new(engine.engine(), host);

        let instance = linker
            .instantiate_async(&mut store, &component)
            .await
            .map_err(PluginError::from)?;
        let bindings = MemoryPlugin::new(&mut store, &instance).map_err(PluginError::from)?;

        // Phase 2: read plugin-info exports — canonical source of truth.
        let plugin_info = bindings.zeroclaw_plugin_plugin_info();
        let plugin_name = plugin_info
            .call_plugin_name(&mut store)
            .await
            .map_err(PluginError::from)?;
        let plugin_version = plugin_info
            .call_plugin_version(&mut store)
            .await
            .map_err(PluginError::from)?;

        let capabilities = bindings
            .zeroclaw_plugin_memory()
            .call_get_memory_capabilities(&mut store)
            .await
            .map_err(PluginError::from)?;

        Ok(Self {
            alias: alias.into(),
            capabilities,
            state: Arc::new(Mutex::new((store, bindings))),
            plugin_name,
            plugin_version,
        })
    }
}

// ── Type conversions ──────────────────────────────────────────────────────────

fn to_wit_category(cat: MemoryCategory) -> WitMemoryCategory {
    match cat {
        MemoryCategory::Core => WitMemoryCategory::Core,
        MemoryCategory::Daily => WitMemoryCategory::Daily,
        MemoryCategory::Conversation => WitMemoryCategory::Conversation,
        MemoryCategory::Custom(s) => WitMemoryCategory::Custom(s),
    }
}

fn from_wit_category(cat: WitMemoryCategory) -> MemoryCategory {
    match cat {
        WitMemoryCategory::Core => MemoryCategory::Core,
        WitMemoryCategory::Daily => MemoryCategory::Daily,
        WitMemoryCategory::Conversation => MemoryCategory::Conversation,
        WitMemoryCategory::Custom(s) => MemoryCategory::Custom(s),
    }
}

fn from_wit_entry(e: WitMemoryEntry) -> MemoryEntry {
    MemoryEntry {
        id: e.id,
        key: e.key,
        content: e.content,
        category: from_wit_category(e.category),
        timestamp: e.timestamp,
        session_id: e.session_id,
        score: e.score,
        namespace: e.namespace,
        importance: e.importance,
        superseded_by: e.superseded_by,
        agent_alias: e.agent_alias,
        agent_id: e.agent_id,
    }
}

fn to_wit_export_filter(f: &ExportFilter) -> WitExportFilter {
    WitExportFilter {
        namespace: f.namespace.clone(),
        session_id: f.session_id.clone(),
        category: f.category.clone().map(to_wit_category),
        since: f.since.clone(),
        until: f.until.clone(),
    }
}

fn to_wit_agent_filter(agents: &[&str]) -> WitAgentFilter {
    if agents.is_empty() {
        WitAgentFilter::All
    } else {
        WitAgentFilter::Some(agents.iter().map(|s| s.to_string()).collect())
    }
}

fn to_wit_procedural(msgs: &[ProceduralMessage]) -> Vec<WitProceduralMessage> {
    msgs.iter()
        .map(|m| WitProceduralMessage {
            role: m.role.clone(),
            content: m.content.clone(),
            name: m.name.clone(),
        })
        .collect()
}

// ── Memory trait impl ─────────────────────────────────────────────────────────

#[async_trait]
impl zeroclaw_api::memory_traits::Memory for ComponentMemory {
    fn name(&self) -> &str {
        &self.alias
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let key = key.to_string();
        let content = content.to_string();
        let session_id = session_id.map(str::to_string);
        let wit_cat = to_wit_category(category);
        call_plugin!(
            self,
            "store",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_store_entry(store, &key, &content, &wit_cat, session_id.as_deref())
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let query = query.to_string();
        let limit = limit as u64;
        let session_id = session_id.map(str::to_string);
        let since = since.map(str::to_string);
        let until = until.map(str::to_string);
        call_plugin!(
            self,
            "recall",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_recall(
                        store,
                        &query,
                        limit,
                        session_id.as_deref(),
                        since.as_deref(),
                        until.as_deref(),
                    )
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|entries| entries.into_iter().map(from_wit_entry).collect())
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        let key = key.to_string();
        call_plugin!(
            self,
            "get",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_get(store, &key)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|opt| opt.map(from_wit_entry))
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let wit_cat = category.cloned().map(to_wit_category);
        let session_id = session_id.map(str::to_string);
        call_plugin!(
            self,
            "list",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_list_entries(store, wit_cat.as_ref(), session_id.as_deref())
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|entries| entries.into_iter().map(from_wit_entry).collect())
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        let key = key.to_string();
        call_plugin!(
            self,
            "forget",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_forget(store, &key)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
        let key = key.to_string();
        let agent_id = agent_id.to_string();
        call_plugin!(
            self,
            "forget_for_agent",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_forget_for_agent(store, &key, &agent_id)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn count(&self) -> anyhow::Result<usize> {
        call_plugin!(
            self,
            "count",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_count(store)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|n| n as usize)
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn health_check(&self) -> bool {
        call_plugin!(
            self,
            "health_check",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_health_check(store)
                    .await
                    .map_err(anyhow::Error::msg)
                    .unwrap_or(false)
            }
        )
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let key = key.to_string();
        let content = content.to_string();
        let session_id = session_id.map(str::to_string);
        let namespace = namespace.map(str::to_string);
        let agent_id = agent_id.map(str::to_string);
        let wit_cat = to_wit_category(category);
        call_plugin!(
            self,
            "store_with_agent",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_store_with_agent(
                        store,
                        &key,
                        &content,
                        &wit_cat,
                        session_id.as_deref(),
                        namespace.as_deref(),
                        importance,
                        agent_id.as_deref(),
                    )
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let wit_filter = to_wit_agent_filter(allowed_agent_ids);
        let query = query.to_string();
        let limit = limit as u64;
        let session_id = session_id.map(str::to_string);
        let since = since.map(str::to_string);
        let until = until.map(str::to_string);
        call_plugin!(
            self,
            "recall_for_agents",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_recall_for_agents(
                        store,
                        &wit_filter,
                        &query,
                        limit,
                        session_id.as_deref(),
                        since.as_deref(),
                        until.as_deref(),
                    )
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|entries| entries.into_iter().map(from_wit_entry).collect())
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    // ── Capability-gated overrides ────────────────────────────────────────────

    async fn get_for_agent(
        &self,
        key: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::GET_FOR_AGENT)
        {
            // Default: compose get() + agent-id filter.
            let hit = self.get(key).await?;
            return Ok(hit.filter(|e| e.agent_id.as_deref() == Some(agent_id)));
        }
        let key = key.to_string();
        let agent_id = agent_id.to_string();
        call_plugin!(
            self,
            "get_for_agent",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_get_for_agent(store, &key, &agent_id)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|opt| opt.map(from_wit_entry))
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::PURGE_NAMESPACE)
        {
            anyhow::bail!("purge_namespace not supported by this memory backend");
        }
        let namespace = namespace.to_string();
        call_plugin!(
            self,
            "purge_namespace",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_purge_namespace(store, &namespace)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|n| n as usize)
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::PURGE_SESSION)
        {
            anyhow::bail!("purge_session not supported by this memory backend");
        }
        let session_id = session_id.to_string();
        call_plugin!(
            self,
            "purge_session",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_purge_session(store, &session_id)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|n| n as usize)
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn purge_session_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> anyhow::Result<usize> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::PURGE_SESSION_FOR_AGENT)
        {
            anyhow::bail!("purge_session_for_agent not supported by this memory backend");
        }
        let session_id = session_id.to_string();
        let agent_id = agent_id.to_string();
        call_plugin!(
            self,
            "purge_session_for_agent",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_purge_session_for_agent(store, &session_id, &agent_id)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|n| n as usize)
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn purge_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        if !self.capabilities.contains(MemoryCapabilities::PURGE_AGENT) {
            anyhow::bail!("purge_agent not supported by this memory backend");
        }
        let agent_alias = agent_alias.to_string();
        call_plugin!(
            self,
            "purge_agent",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_purge_agent(store, &agent_alias)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|n| n as usize)
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn reindex(&self) -> anyhow::Result<usize> {
        if !self.capabilities.contains(MemoryCapabilities::REINDEX) {
            return Ok(0);
        }
        call_plugin!(
            self,
            "reindex",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_reindex(store)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|n| n as usize)
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::STORE_PROCEDURAL)
        {
            return Ok(());
        }
        let wit_msgs = to_wit_procedural(messages);
        let session_id = session_id.map(str::to_string);
        call_plugin!(
            self,
            "store_procedural",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_store_procedural(store, &wit_msgs, session_id.as_deref())
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> anyhow::Result<String> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::ENSURE_AGENT_UUID)
        {
            return Ok(alias.to_string());
        }
        let alias = alias.to_string();
        call_plugin!(
            self,
            "ensure_agent_uuid",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_ensure_agent_uuid(store, &alias)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn recall_namespaced(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::RECALL_NAMESPACED)
        {
            // Default: delegate to recall() and post-filter by namespace.
            let entries = self
                .recall(query, limit * 2, session_id, since, until)
                .await?;
            return Ok(entries
                .into_iter()
                .filter(|e| e.namespace == namespace)
                .take(limit)
                .collect());
        }
        let namespace = namespace.to_string();
        let query = query.to_string();
        let limit_u64 = limit as u64;
        let session_id = session_id.map(str::to_string);
        let since = since.map(str::to_string);
        let until = until.map(str::to_string);
        call_plugin!(
            self,
            "recall_namespaced",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_recall_namespaced(
                        store,
                        &namespace,
                        &query,
                        limit_u64,
                        session_id.as_deref(),
                        since.as_deref(),
                        until.as_deref(),
                    )
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|entries| entries.into_iter().map(from_wit_entry).collect())
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn export(&self, filter: &ExportFilter) -> anyhow::Result<Vec<MemoryEntry>> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::EXPORT_ENTRIES)
        {
            // Default: list() + post-filter on namespace + time range.
            let entries = self
                .list(filter.category.as_ref(), filter.session_id.as_deref())
                .await?;
            return Ok(entries
                .into_iter()
                .filter(|e| {
                    if let Some(ref ns) = filter.namespace
                        && e.namespace != *ns
                    {
                        return false;
                    }
                    if let Some(ref since) = filter.since
                        && e.timestamp.as_str() < since.as_str()
                    {
                        return false;
                    }
                    if let Some(ref until) = filter.until
                        && e.timestamp.as_str() > until.as_str()
                    {
                        return false;
                    }
                    true
                })
                .collect());
        }
        let wit_filter = to_wit_export_filter(filter);
        call_plugin!(
            self,
            "export_entries",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_export_entries(store, &wit_filter)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|entries| entries.into_iter().map(from_wit_entry).collect())
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::STORE_WITH_METADATA)
        {
            // Default: delegate to store(), dropping namespace + importance.
            return self.store(key, content, category, session_id).await;
        }
        let key = key.to_string();
        let content = content.to_string();
        let session_id = session_id.map(str::to_string);
        let namespace = namespace.map(str::to_string);
        let wit_cat = to_wit_category(category);
        call_plugin!(
            self,
            "store_with_metadata",
            async move |store, bindings: &mut MemoryPlugin| {
                bindings
                    .zeroclaw_plugin_memory()
                    .call_store_with_metadata(
                        store,
                        &key,
                        &content,
                        &wit_cat,
                        session_id.as_deref(),
                        namespace.as_deref(),
                        importance,
                    )
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_category_round_trip_all_variants() {
        let cases = [
            MemoryCategory::Core,
            MemoryCategory::Daily,
            MemoryCategory::Conversation,
            MemoryCategory::Custom("project_notes".into()),
        ];
        for cat in cases {
            let wit = to_wit_category(cat.clone());
            let back = from_wit_category(wit);
            assert_eq!(back, cat);
        }
    }

    #[test]
    fn memory_entry_field_mapping() {
        let wit = WitMemoryEntry {
            id: "id-1".into(),
            key: "k".into(),
            content: "c".into(),
            category: WitMemoryCategory::Core,
            timestamp: "2026-01-01T00:00:00Z".into(),
            session_id: Some("s1".into()),
            score: Some(0.9),
            namespace: "default".into(),
            importance: Some(0.5),
            superseded_by: None,
            agent_alias: Some("clamps".into()),
            agent_id: Some("uuid-123".into()),
        };
        let entry = from_wit_entry(wit);
        assert_eq!(entry.id, "id-1");
        assert_eq!(entry.category, MemoryCategory::Core);
        assert_eq!(entry.score, Some(0.9));
        assert_eq!(entry.importance, Some(0.5));
        assert_eq!(entry.agent_alias.as_deref(), Some("clamps"));
        assert_eq!(entry.agent_id.as_deref(), Some("uuid-123"));
    }

    #[test]
    fn agent_filter_empty_slice_maps_to_all() {
        let filter = to_wit_agent_filter(&[]);
        assert!(matches!(filter, WitAgentFilter::All));
    }

    #[test]
    fn agent_filter_non_empty_slice_maps_to_some() {
        let filter = to_wit_agent_filter(&["a", "b"]);
        match filter {
            WitAgentFilter::Some(ids) => assert_eq!(ids, vec!["a", "b"]),
            _ => panic!("expected Some"),
        }
    }
}
