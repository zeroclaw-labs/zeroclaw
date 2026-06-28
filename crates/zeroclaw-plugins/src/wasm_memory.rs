//! Memory adapter: `WasmMemory` implements `zeroclaw_api::memory_traits::Memory`
//! backed by the `memory-plugin` component world. Warm store, called at high
//! frequency, so the store and bindings live for the adapter's lifetime.

use crate::component::bindings::memory::MemoryPlugin;
use crate::component::bindings::memory::exports::zeroclaw::plugin::memory::{
    AgentFilter as WitAgentFilter, ExportFilter as WitExportFilter, MemoryCapabilities,
    MemoryCategory as WitMemoryCategory, MemoryEntry as WitMemoryEntry,
    ProceduralMessage as WitProceduralMessage,
};
use crate::component::{PluginState, call_plugin, engine, load_component, wt};
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use wasmtime::Store;
use wasmtime::component::Linker;
use zeroclaw_api::attribution::{Attributable, MemoryKind, Role};
use zeroclaw_api::memory_traits::{
    ExportFilter, Memory, MemoryCategory, MemoryEntry, ProceduralMessage,
};

/// A memory backend backed by a WIT component-model plugin.
pub struct WasmMemory {
    alias: String,
    capabilities: MemoryCapabilities,
    state: Arc<Mutex<(Store<PluginState>, MemoryPlugin)>>,
}

impl Attributable for WasmMemory {
    fn role(&self) -> Role {
        Role::Memory(MemoryKind::Plugin)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

fn linker() -> Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine());
    crate::component::add_wasi(&mut linker)?;
    let mut options = crate::component::bindings::memory::LinkOptions::default();
    options.plugins_wit_v0(true);
    wt(
        MemoryPlugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
            &mut linker,
            &options,
            |s| s,
        ),
        "failed to add memory plugin imports to linker",
    )?;
    Ok(linker)
}

impl WasmMemory {
    /// Compile and instantiate a memory plugin, caching its capabilities.
    pub async fn from_wasm(alias: impl Into<String>, wasm_path: &Path) -> Result<Self> {
        let component = load_component(wasm_path)?;
        let linker = linker()?;
        let mut store = Store::new(engine(), PluginState::default());
        let bindings = wt(
            MemoryPlugin::instantiate_async(&mut store, &component, &linker).await,
            "failed to instantiate memory plugin",
        )?;
        let capabilities = wt(
            bindings
                .zeroclaw_plugin_memory()
                .call_get_memory_capabilities(&mut store)
                .await,
            "memory.get-memory-capabilities failed",
        )?;
        Ok(Self {
            alias: alias.into(),
            capabilities,
            state: Arc::new(Mutex::new((store, bindings))),
        })
    }
}

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

fn from_wit_entries(v: Vec<WitMemoryEntry>) -> Vec<MemoryEntry> {
    v.into_iter().map(from_wit_entry).collect()
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

#[async_trait]
impl Memory for WasmMemory {
    fn name(&self) -> &str {
        &self.alias
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        let (key, content, session_id) = (
            key.to_string(),
            content.to_string(),
            session_id.map(str::to_string),
        );
        let wit_cat = to_wit_category(category);
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_store_entry(store, &key, &content, &wit_cat, session_id.as_deref())
                        .await,
                    "memory.store-entry trapped",
                )?
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
    ) -> Result<Vec<MemoryEntry>> {
        let (query, session_id, since, until) = (
            query.to_string(),
            session_id.map(str::to_string),
            since.map(str::to_string),
            until.map(str::to_string),
        );
        let limit = limit as u64;
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
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
                        .await,
                    "memory.recall trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(from_wit_entries(out))
            }
        )
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let key = key.to_string();
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_get(store, &key)
                        .await,
                    "memory.get trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out.map(from_wit_entry))
            }
        )
    }

    async fn get_for_agent(&self, key: &str, agent_id: &str) -> Result<Option<MemoryEntry>> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::GET_FOR_AGENT)
        {
            let hit = self.get(key).await?;
            return Ok(hit.filter(|e| e.agent_id.as_deref() == Some(agent_id)));
        }
        let (key, agent_id) = (key.to_string(), agent_id.to_string());
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_get_for_agent(store, &key, &agent_id)
                        .await,
                    "memory.get-for-agent trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out.map(from_wit_entry))
            }
        )
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let wit_cat = category.cloned().map(to_wit_category);
        let session_id = session_id.map(str::to_string);
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_list_entries(store, wit_cat.as_ref(), session_id.as_deref())
                        .await,
                    "memory.list-entries trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(from_wit_entries(out))
            }
        )
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let key = key.to_string();
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_forget(store, &key)
                        .await,
                    "memory.forget trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> Result<bool> {
        let (key, agent_id) = (key.to_string(), agent_id.to_string());
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_forget_for_agent(store, &key, &agent_id)
                        .await,
                    "memory.forget-for-agent trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn purge_namespace(&self, namespace: &str) -> Result<usize> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::PURGE_NAMESPACE)
        {
            anyhow::bail!("purge_namespace not supported by this memory backend");
        }
        let namespace = namespace.to_string();
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_purge_namespace(store, &namespace)
                        .await,
                    "memory.purge-namespace trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out as usize)
            }
        )
    }

    async fn purge_session(&self, session_id: &str) -> Result<usize> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::PURGE_SESSION)
        {
            anyhow::bail!("purge_session not supported by this memory backend");
        }
        let session_id = session_id.to_string();
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_purge_session(store, &session_id)
                        .await,
                    "memory.purge-session trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out as usize)
            }
        )
    }

    async fn purge_session_for_agent(&self, session_id: &str, agent_id: &str) -> Result<usize> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::PURGE_SESSION_FOR_AGENT)
        {
            anyhow::bail!("purge_session_for_agent not supported by this memory backend");
        }
        let (session_id, agent_id) = (session_id.to_string(), agent_id.to_string());
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_purge_session_for_agent(store, &session_id, &agent_id)
                        .await,
                    "memory.purge-session-for-agent trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out as usize)
            }
        )
    }

    async fn purge_agent(&self, agent_alias: &str) -> Result<usize> {
        if !self.capabilities.contains(MemoryCapabilities::PURGE_AGENT) {
            anyhow::bail!("purge_agent not supported by this memory backend");
        }
        let agent_alias = agent_alias.to_string();
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_purge_agent(store, &agent_alias)
                        .await,
                    "memory.purge-agent trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out as usize)
            }
        )
    }

    async fn count(&self) -> Result<usize> {
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings.zeroclaw_plugin_memory().call_count(store).await,
                    "memory.count trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out as usize)
            }
        )
    }

    async fn health_check(&self) -> bool {
        let result: Result<bool> = call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_health_check(store)
                        .await,
                    "memory.health-check failed",
                )
            }
        );
        result.unwrap_or(false)
    }

    async fn reindex(&self) -> Result<usize> {
        if !self.capabilities.contains(MemoryCapabilities::REINDEX) {
            return Ok(0);
        }
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings.zeroclaw_plugin_memory().call_reindex(store).await,
                    "memory.reindex trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out as usize)
            }
        )
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> Result<()> {
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
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_store_procedural(store, &wit_msgs, session_id.as_deref())
                        .await,
                    "memory.store-procedural trapped",
                )?
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
    ) -> Result<Vec<MemoryEntry>> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::RECALL_NAMESPACED)
        {
            let entries = self
                .recall(query, limit * 2, session_id, since, until)
                .await?;
            return Ok(entries
                .into_iter()
                .filter(|e| e.namespace == namespace)
                .take(limit)
                .collect());
        }
        let (namespace, query, session_id, since, until) = (
            namespace.to_string(),
            query.to_string(),
            session_id.map(str::to_string),
            since.map(str::to_string),
            until.map(str::to_string),
        );
        let limit = limit as u64;
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_recall_namespaced(
                            store,
                            &namespace,
                            &query,
                            limit,
                            session_id.as_deref(),
                            since.as_deref(),
                            until.as_deref(),
                        )
                        .await,
                    "memory.recall-namespaced trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(from_wit_entries(out))
            }
        )
    }

    async fn export(&self, filter: &ExportFilter) -> Result<Vec<MemoryEntry>> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::EXPORT_ENTRIES)
        {
            let entries = self
                .list(filter.category.as_ref(), filter.session_id.as_deref())
                .await?;
            return Ok(entries
                .into_iter()
                .filter(|e| {
                    filter
                        .namespace
                        .as_ref()
                        .is_none_or(|ns| &e.namespace == ns)
                        && filter
                            .since
                            .as_ref()
                            .is_none_or(|s| e.timestamp.as_str() >= s.as_str())
                        && filter
                            .until
                            .as_ref()
                            .is_none_or(|u| e.timestamp.as_str() <= u.as_str())
                })
                .collect());
        }
        let wit_filter = to_wit_export_filter(filter);
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_export_entries(store, &wit_filter)
                        .await,
                    "memory.export-entries trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(from_wit_entries(out))
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
    ) -> Result<()> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::STORE_WITH_METADATA)
        {
            return self.store(key, content, category, session_id).await;
        }
        let (key, content, session_id, namespace) = (
            key.to_string(),
            content.to_string(),
            session_id.map(str::to_string),
            namespace.map(str::to_string),
        );
        let wit_cat = to_wit_category(category);
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
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
                        .await,
                    "memory.store-with-metadata trapped",
                )?
                .map_err(anyhow::Error::msg)
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
    ) -> Result<()> {
        let (key, content, session_id, namespace, agent_id) = (
            key.to_string(),
            content.to_string(),
            session_id.map(str::to_string),
            namespace.map(str::to_string),
            agent_id.map(str::to_string),
        );
        let wit_cat = to_wit_category(category);
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
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
                        .await,
                    "memory.store-with-agent trapped",
                )?
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
    ) -> Result<Vec<MemoryEntry>> {
        let wit_agents = to_wit_agent_filter(allowed_agent_ids);
        let (query, session_id, since, until) = (
            query.to_string(),
            session_id.map(str::to_string),
            since.map(str::to_string),
            until.map(str::to_string),
        );
        let limit = limit as u64;
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_recall_for_agents(
                            store,
                            &wit_agents,
                            &query,
                            limit,
                            session_id.as_deref(),
                            since.as_deref(),
                            until.as_deref(),
                        )
                        .await,
                    "memory.recall-for-agents trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(from_wit_entries(out))
            }
        )
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> Result<String> {
        if !self
            .capabilities
            .contains(MemoryCapabilities::ENSURE_AGENT_UUID)
        {
            return Ok(alias.to_string());
        }
        let alias = alias.to_string();
        call_plugin!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut MemoryPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_memory()
                        .call_ensure_agent_uuid(store, &alias)
                        .await,
                    "memory.ensure-agent-uuid trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_round_trip() {
        for cat in [
            MemoryCategory::Core,
            MemoryCategory::Daily,
            MemoryCategory::Conversation,
            MemoryCategory::Custom("notes".into()),
        ] {
            assert_eq!(from_wit_category(to_wit_category(cat.clone())), cat);
        }
    }

    #[test]
    fn agent_filter_maps_empty_to_all() {
        assert!(matches!(to_wit_agent_filter(&[]), WitAgentFilter::All));
        assert!(matches!(
            to_wit_agent_filter(&["a", "b"]),
            WitAgentFilter::Some(_)
        ));
    }
}
