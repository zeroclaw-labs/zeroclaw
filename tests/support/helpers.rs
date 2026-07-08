//! Shared builder helpers for constructing test agents.

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw::agent::agent::Agent;
use zeroclaw::agent::dispatcher::{NativeToolDispatcher, XmlToolDispatcher};
use zeroclaw::config::MemoryConfig;
use zeroclaw::memory;
use zeroclaw::memory::Memory;
use zeroclaw::observability::{NoopObserver, Observer};
use zeroclaw::providers::{ChatResponse, ModelProvider, ToolCall};
use zeroclaw::tools::Tool;

/// Create an in-memory "none" backend for tests.
pub fn make_memory() -> Arc<dyn Memory> {
    let cfg = MemoryConfig {
        backend: "none".into(),
        ..MemoryConfig::default()
    };
    Arc::from(memory::create_memory(&cfg, &std::env::temp_dir(), None).unwrap())
}

/// Create a `NoopObserver` for tests.
pub fn make_observer() -> Arc<dyn Observer> {
    Arc::from(NoopObserver {})
}

/// Create a text-only `ChatResponse`.
pub fn text_response(text: &str) -> ChatResponse {
    ChatResponse {
        text: Some(text.into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
    }
}

/// Create a `ChatResponse` with tool calls.
pub fn tool_response(calls: Vec<ToolCall>) -> ChatResponse {
    ChatResponse {
        text: Some(String::new()),
        tool_calls: calls,
        usage: None,
        reasoning_content: None,
    }
}

/// Build an agent with `NativeToolDispatcher`.
pub fn build_agent(model_provider: Box<dyn ModelProvider>, tools: Vec<Box<dyn Tool>>) -> Agent {
    Agent::builder()
        .model_provider(model_provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .build()
        .unwrap()
}

/// Build an agent with `XmlToolDispatcher`.
pub fn build_agent_xml(model_provider: Box<dyn ModelProvider>, tools: Vec<Box<dyn Tool>>) -> Agent {
    Agent::builder()
        .model_provider(model_provider)
        .tools(tools)
        .memory(make_memory())
        .observer(make_observer())
        .tool_dispatcher(Box::new(XmlToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .build()
        .unwrap()
}

/// Build an agent with an optional custom `Memory` backend.
pub fn build_recording_agent(
    model_provider: Box<dyn ModelProvider>,
    tools: Vec<Box<dyn Tool>>,
    memory: Option<Arc<dyn zeroclaw::memory::Memory>>,
) -> Agent {
    Agent::builder()
        .model_provider(model_provider)
        .tools(tools)
        .memory(memory.unwrap_or_else(make_memory))
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .build()
        .unwrap()
}

/// Build an agent with real `SqliteMemory` in a temporary directory.
pub fn build_agent_with_sqlite_memory(
    model_provider: Box<dyn ModelProvider>,
    tools: Vec<Box<dyn Tool>>,
    temp_dir: &std::path::Path,
) -> Agent {
    let cfg = MemoryConfig {
        backend: "sqlite".into(),
        ..MemoryConfig::default()
    };
    let mem = Arc::from(memory::create_memory(&cfg, temp_dir, None).unwrap());
    Agent::builder()
        .model_provider(model_provider)
        .tools(tools)
        .memory(mem)
        .observer(make_observer())
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .build()
        .unwrap()
}

/// Mock memory whose `recall` returns the given (key, content) pairs as
/// Core entries. With the unified engine injection, wiring this as the
/// agent's memory reproduces the old "static context string" strategy shim.
pub struct StaticRecallMemory {
    entries: Vec<(String, String)>,
}

impl StaticRecallMemory {
    pub fn new(entries: &[(&str, &str)]) -> Self {
        Self {
            entries: entries
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }
}

#[async_trait]
impl zeroclaw::memory::Memory for StaticRecallMemory {
    fn name(&self) -> &str {
        "static-recall"
    }
    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: zeroclaw::memory::MemoryCategory,
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
    ) -> anyhow::Result<Vec<zeroclaw::memory::MemoryEntry>> {
        Ok(self
            .entries
            .iter()
            .map(|(k, v)| zeroclaw::memory::MemoryEntry {
                id: k.clone(),
                key: k.clone(),
                content: v.clone(),
                category: zeroclaw::memory::MemoryCategory::Core,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: None,
                score: None,
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
                kind: None,
                pinned: false,
                tenant_id: None,
                agent_alias: None,
                agent_id: None,
            })
            .collect())
    }
    async fn get(&self, _key: &str) -> anyhow::Result<Option<zeroclaw::memory::MemoryEntry>> {
        Ok(None)
    }
    async fn list(
        &self,
        _category: Option<&zeroclaw::memory::MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw::memory::MemoryEntry>> {
        Ok(vec![])
    }
    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
    async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
    async fn count(&self) -> anyhow::Result<usize> {
        Ok(self.entries.len())
    }
    async fn health_check(&self) -> bool {
        true
    }
    async fn store_with_agent(
        &self,
        _key: &str,
        _content: &str,
        _category: zeroclaw::memory::MemoryCategory,
        _session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        _agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn recall_for_agents(
        &self,
        _allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<zeroclaw::memory::MemoryEntry>> {
        self.recall(query, limit, session_id, since, until).await
    }
}

impl zeroclaw_api::attribution::Attributable for StaticRecallMemory {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Memory(zeroclaw_api::attribution::MemoryKind::InMemory)
    }
    fn alias(&self) -> &str {
        "StaticRecallMemory"
    }
}
