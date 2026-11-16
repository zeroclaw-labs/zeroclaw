use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;
use zeroclaw_api::memory_traits::MemoryReadPolicy;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_memory::Memory;
use zeroclaw_memory::admissibility::entry_is_admissible;

/// Let the agent search its own memory
pub struct MemoryRecallTool {
    memory: Arc<dyn Memory>,
}

impl MemoryRecallTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for MemoryRecallTool {
    fn name(&self) -> &str {
        "memory_recall"
    }

    fn description(&self) -> &str {
        "Search long-term memory for relevant facts, preferences, or context. Returns scored results ranked by relevance. Supports keyword search, recent recall with omitted query or bare '*', time-only query (since/until), or both."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or phrase to search for in memory. Omit or pass bare '*' to return recent memories; non-bare wildcard terms remain keyword searches."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 5)"
                },
                "since": {
                    "type": "string",
                    "description": "Filter memories created at or after this time (RFC 3339, e.g. 2025-03-01T00:00:00Z)"
                },
                "until": {
                    "type": "string",
                    "description": "Filter memories created at or before this time (RFC 3339)"
                },
                "search_mode": {
                    "type": "string",
                    "enum": ["bm25", "embedding", "hybrid"],
                    "description": "Search strategy: bm25 (keyword), embedding (semantic), or hybrid (both). Defaults to config value."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let policy = zeroclaw_api::TOOL_LOOP_MEMORY_READ_POLICY
            .try_with(|policy| *policy)
            .unwrap_or_default();
        if policy == MemoryReadPolicy::Disabled {
            return Ok(ToolResult {
                success: true,
                output: "No memories found.".into(),
                error: None,
            });
        }
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let since = args.get("since").and_then(|v| v.as_str());
        let until = args.get("until").and_then(|v| v.as_str());

        // Validate date strings
        if let Some(s) = since
            && chrono::DateTime::parse_from_rfc3339(s).is_err()
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Invalid 'since' date: {s}. Expected RFC 3339 format, e.g. 2025-03-01T00:00:00Z"
                )),
            });
        }
        if let Some(u) = until
            && chrono::DateTime::parse_from_rfc3339(u).is_err()
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Invalid 'until' date: {u}. Expected RFC 3339 format, e.g. 2025-03-01T00:00:00Z"
                )),
            });
        }
        if let (Some(s), Some(u)) = (since, until)
            && let (Ok(s_dt), Ok(u_dt)) = (
                chrono::DateTime::parse_from_rfc3339(s),
                chrono::DateTime::parse_from_rfc3339(u),
            )
            && s_dt >= u_dt
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("'since' must be before 'until'".into()),
            });
        }

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(5, |v| v as usize);

        // Session authority comes from the executing turn, never model arguments.
        let sessions = zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
            .try_with(Clone::clone)
            .unwrap_or_default();
        // Backend session semantics vary: some exclude even unscoped durable
        // facts. Fetch at most 64 extra candidates so a small requested limit
        // is not consumed immediately by foreign rows; this is not exhaustive.
        let fetch_limit = if limit == 0 {
            0
        } else {
            limit.saturating_add(64)
        };
        let recalled = self
            .memory
            .recall(query, fetch_limit, None, since, until)
            .await
            .map(|mut entries| {
                // Backends may have weaker filtering; enforce the boundary again
                // before any result becomes model-visible.
                entries.retain(|entry| {
                    entry_is_admissible(entry, self.memory.name(), &sessions, policy)
                });
                entries.truncate(limit);
                entries
            });
        match recalled {
            Ok(entries) if entries.is_empty() => Ok(ToolResult {
                success: true,
                output: "No memories found.".into(),
                error: None,
            }),
            Ok(entries) => {
                let mut output = format!("Found {} memories:\n", entries.len());
                for entry in &entries {
                    let score = entry
                        .score
                        .map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
                    let _ = writeln!(
                        output,
                        "- [{}] {}: {}{score}",
                        entry.category, entry.key, entry.content
                    );
                }
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Memory recall failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use zeroclaw_config::schema::MemoryConfig;
    use zeroclaw_memory::{
        MemoryCategory, MemoryEntry, NoneMemory, TACHI_COMPILED, create_memory,
        is_recent_recall_query,
    };

    fn try_seeded_mem() -> Option<(TempDir, Arc<dyn Memory>)> {
        if !TACHI_COMPILED {
            return None;
        }
        let tmp = TempDir::new().unwrap();
        let cfg = MemoryConfig {
            backend: "tachi".into(),
            hygiene_enabled: false,
            embedding_provider: "none".into(),
            ..MemoryConfig::default()
        };
        let mem = create_memory(&cfg, tmp.path(), None).unwrap();
        Some((tmp, Arc::from(mem)))
    }

    async fn execute_scoped(
        tool: &MemoryRecallTool,
        policy: MemoryReadPolicy,
        sessions: Vec<String>,
        args: serde_json::Value,
    ) -> ToolResult {
        zeroclaw_api::TOOL_LOOP_MEMORY_READ_POLICY
            .scope(
                policy,
                zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS.scope(sessions, tool.execute(args)),
            )
            .await
            .unwrap()
    }

    struct QueryEchoMemory {
        last_query: Arc<Mutex<Option<String>>>,
        entries: Option<Vec<MemoryEntry>>,
    }

    #[async_trait]
    impl Memory for QueryEchoMemory {
        fn name(&self) -> &str {
            "query_echo"
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
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            *self.last_query.lock().unwrap() = Some(query.to_string());
            if let Some(entries) = &self.entries {
                return Ok(entries.clone());
            }
            if is_recent_recall_query(query) {
                Ok(vec![MemoryEntry {
                    id: "recent".into(),
                    key: "recent".into(),
                    content: "recent memory".into(),
                    category: MemoryCategory::Core,
                    timestamp: "2026-05-03T00:00:00Z".into(),
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
                }])
            } else {
                Ok(Vec::new())
            }
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

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
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
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            self.recall(query, limit, session_id, since, until).await
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for QueryEchoMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "QueryEchoMemory"
        }
    }

    #[tokio::test]
    async fn tachi_session_recall_keeps_unscoped_long_term_facts() {
        let Some((_dir, store)) = try_seeded_mem() else {
            return;
        };
        for (key, category, session) in [
            ("durable", MemoryCategory::Core, None),
            ("current", MemoryCategory::Conversation, Some("A")),
            ("foreign", MemoryCategory::Conversation, Some("B")),
            ("legacy", MemoryCategory::Conversation, None),
        ] {
            store
                .store(key, &format!("fixture {key}"), category, session)
                .await
                .unwrap();
        }
        // Recent backend rows are foreign/unattributed, ahead of eligible facts.
        let newest = store.recall("*", 1, None, None, None).await.unwrap();
        assert_eq!(newest[0].key, "legacy");
        let tool = MemoryRecallTool::new(store);
        for scopes in [vec![], vec!["A".into()]] {
            let result = execute_scoped(
                &tool,
                MemoryReadPolicy::SessionScoped,
                scopes,
                json!({"query":"*", "limit":1}),
            )
            .await;
            assert!(result.success);
            assert!(result.output.contains("Found 1 memories:"));
            assert!(!result.output.contains("fixture foreign"));
            assert!(!result.output.contains("fixture legacy"));
        }
        for query in ["*", "fixture"] {
            let result = execute_scoped(
                &tool,
                MemoryReadPolicy::SessionScoped,
                vec!["A".into()],
                json!({"query":query, "limit":20}),
            )
            .await;
            assert!(result.success);
            assert!(result.output.contains("fixture durable"));
            assert!(result.output.contains("fixture current"));
            assert!(!result.output.contains("fixture foreign"));
            assert!(!result.output.contains("fixture legacy"));
        }
    }

    #[tokio::test]
    async fn recall_enforces_session_and_protected_memory_even_if_backend_does_not() {
        let Some((_dir, store)) = try_seeded_mem() else {
            return;
        };
        for (key, session, category) in [
            ("shared_fact", None, MemoryCategory::Core),
            ("session_a", Some("A"), MemoryCategory::Conversation),
            ("session_b", Some("B"), MemoryCategory::Conversation),
            ("unattributed", None, MemoryCategory::Conversation),
            ("private_preference", Some("B"), MemoryCategory::Core),
        ] {
            store.store(key, key, category, session).await.unwrap();
        }
        let mut entries = store.recall("*", 20, None, None, None).await.unwrap();
        let mut protected = entries[0].clone();
        protected.key = "soul::private".into();
        protected.content = "protected_soul_bytes".into();
        protected.namespace = "soul".into();
        entries.push(protected);
        let tool = MemoryRecallTool::new(Arc::new(QueryEchoMemory {
            last_query: Arc::new(Mutex::new(None)),
            entries: Some(entries),
        }));
        for session in [None, Some("A".to_string())] {
            let result = execute_scoped(
                &tool,
                MemoryReadPolicy::SessionScoped,
                session.clone().into_iter().collect(),
                json!({"query":"*", "limit":20}),
            )
            .await;
            assert!(result.success);
            let output = result.output.to_string();
            assert!(output.contains("shared_fact"));
            assert_eq!(output.contains("session_a"), session.is_some());
            for forbidden in [
                "session_b",
                "unattributed",
                "private_preference",
                "protected_soul_bytes",
            ] {
                assert!(!output.contains(forbidden), "{output}");
            }
        }
    }

    #[tokio::test]
    async fn markdown_unattributed_daily_rows_cannot_expose_conversation_autosaves() {
        let dir = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::new(zeroclaw_memory::MarkdownMemory::new("test", dir.path()));
        memory
            .store("global", "global fixture", MemoryCategory::Core, None)
            .await
            .unwrap();
        memory
            .store(
                "private",
                "foreign conversation fixture",
                MemoryCategory::Conversation,
                Some("other-session"),
            )
            .await
            .unwrap();
        let tool = MemoryRecallTool::new(memory);
        for scopes in [vec![], vec!["current-session".into()]] {
            let result = execute_scoped(
                &tool,
                MemoryReadPolicy::SessionScoped,
                scopes,
                json!({"query":"fixture", "limit":20}),
            )
            .await;
            assert!(result.success);
            assert!(result.output.contains("global fixture"));
            assert!(!result.output.contains("foreign conversation fixture"));
        }
    }

    #[tokio::test]
    async fn recall_read_policy_excludes_scheduled_conversations_and_ignores_model_authority() {
        let Some((_dir, memory)) = try_seeded_mem() else {
            return;
        };
        for (key, category, session) in [
            ("global", MemoryCategory::Core, None),
            ("current", MemoryCategory::Conversation, Some("A")),
            ("current_fact", MemoryCategory::Core, Some("A")),
            ("foreign", MemoryCategory::Conversation, Some("B")),
            ("foreign_fact", MemoryCategory::Core, Some("B")),
            ("unattributed", MemoryCategory::Conversation, None),
        ] {
            memory
                .store(key, &format!("fixture {key}"), category, session)
                .await
                .unwrap();
        }
        let tool = MemoryRecallTool::new(memory);
        for policy in [
            MemoryReadPolicy::NonConversation,
            MemoryReadPolicy::SessionScoped,
        ] {
            for mut args in [
                json!({"query":"*", "limit":20}),
                json!({"query":"fixture", "limit":20}),
                json!({"since":"2020-01-01T00:00:00Z", "limit":20}),
            ] {
                // Model-authored claims cannot select a policy or foreign session.
                args["session_id"] = json!("B");
                args["read_policy"] = json!("session_scoped");
                args["origin"] = json!("interactive");
                args["surface"] = json!("web");
                let result = execute_scoped(&tool, policy, vec!["A".into()], args).await;
                assert!(result.success);
                assert!(result.output.contains("fixture global"));
                assert!(result.output.contains("fixture current_fact"));
                assert_eq!(
                    result.output.contains("- [conversation] current:"),
                    policy == MemoryReadPolicy::SessionScoped,
                    "{policy:?}: {}",
                    result.output,
                );
                for denied in ["fixture foreign", "fixture unattributed"] {
                    assert!(!result.output.contains(denied), "{}", result.output);
                }
            }
        }
        // Canonical session IDs alone are not an interactive read-policy grant.
        let result = zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
            .scope(
                vec!["A".into()],
                tool.execute(json!({"query":"*", "limit":20})),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("fixture global"));
        assert!(result.output.contains("fixture current_fact"));
        assert!(!result.output.contains("- [conversation] current:"));
        assert!(!result.output.contains("fixture foreign"));
    }

    #[tokio::test]
    async fn recall_disabled_policy_returns_empty_without_calling_backend() {
        let last_query = Arc::new(Mutex::new(None));
        let tool = MemoryRecallTool::new(Arc::new(QueryEchoMemory {
            last_query: Arc::clone(&last_query),
            entries: None,
        }));
        for args in [json!({"query":"*"}), json!({"since":"invalid"})] {
            let result =
                execute_scoped(&tool, MemoryReadPolicy::Disabled, vec!["A".into()], args).await;
            assert!(result.success);
            assert_eq!(result.output, "No memories found.");
            assert!(result.error.is_none());
            assert!(
                last_query.lock().unwrap().is_none(),
                "disabled recall must not query the backend"
            );
        }
    }

    #[tokio::test]
    async fn recall_empty() {
        let Some((_tmp, mem)) = try_seeded_mem() else {
            return;
        };
        let tool = MemoryRecallTool::new(mem);
        let result = tool.execute(json!({"query": "anything"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No memories found"));
    }

    #[tokio::test]
    async fn recall_finds_match() {
        let Some((_tmp, mem)) = try_seeded_mem() else {
            return;
        };
        mem.store("lang", "User prefers Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("tz", "Timezone is EST", MemoryCategory::Core, None)
            .await
            .unwrap();

        let tool = MemoryRecallTool::new(mem);
        let result = tool.execute(json!({"query": "Rust"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Rust"));
        assert!(result.output.contains("Found 1"));
    }

    #[tokio::test]
    async fn recall_respects_limit() {
        let Some((_tmp, mem)) = try_seeded_mem() else {
            return;
        };
        for i in 0..10 {
            mem.store(
                &format!("k{i}"),
                &format!("Rust fact {i}"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }

        let tool = MemoryRecallTool::new(mem);
        let result = tool
            .execute(json!({"query": "Rust", "limit": 3}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 3"));
    }

    #[tokio::test]
    async fn bare_recall_returns_recent_entries() {
        let Some((_tmp, mem)) = try_seeded_mem() else {
            return;
        };
        mem.store("lang", "User prefers Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        let tool = MemoryRecallTool::new(mem);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 1"));
        assert!(result.output.contains("Rust"));
    }

    #[tokio::test]
    async fn recall_star_query_returns_recent_entries() {
        let Some((_tmp, mem)) = try_seeded_mem() else {
            return;
        };
        mem.store("lang", "User prefers Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("tz", "Timezone is EST", MemoryCategory::Core, None)
            .await
            .unwrap();

        let tool = MemoryRecallTool::new(mem);
        let result = tool.execute(json!({"query": "*"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 2"));
        assert!(result.output.contains("Rust"));
        assert!(result.output.contains("EST"));
    }

    #[tokio::test]
    async fn recall_star_query_uses_backend_recent_query_contract() {
        let last_query = Arc::new(Mutex::new(None));
        let mem = Arc::new(QueryEchoMemory {
            entries: None,
            last_query: last_query.clone(),
        });
        let tool = MemoryRecallTool::new(mem);

        let result = tool.execute(json!({"query": "*"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("recent memory"));
        assert_eq!(*last_query.lock().unwrap(), Some("*".into()));
    }

    #[tokio::test]
    async fn recall_time_only_returns_entries() {
        let Some((_tmp, mem)) = try_seeded_mem() else {
            return;
        };
        mem.store("lang", "User prefers Rust", MemoryCategory::Core, None)
            .await
            .unwrap();
        let tool = MemoryRecallTool::new(mem);
        // Time-only: since far in past
        let result = tool
            .execute(json!({"since": "2020-01-01T00:00:00Z", "limit": 5}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 1"));
        assert!(result.output.contains("Rust"));
    }

    #[test]
    fn name_and_schema() {
        let mem: Arc<dyn Memory> = Arc::new(NoneMemory::new("none"));
        let tool = MemoryRecallTool::new(mem);
        assert_eq!(tool.name(), "memory_recall");
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }

    #[test]
    fn score_formatted_as_percent() {
        let score: Option<f64> = Some(0.63);
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, " [63%]");

        let score: Option<f64> = Some(0.42);
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, " [42%]");

        let score: Option<f64> = Some(1.0);
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, " [100%]");

        let score: Option<f64> = Some(0.0);
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, " [0%]");

        let score: Option<f64> = None;
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, "");
    }

    #[test]
    fn schema_includes_search_mode_parameter() {
        let mem: Arc<dyn Memory> = Arc::new(NoneMemory::new("none"));
        let tool = MemoryRecallTool::new(mem);
        let schema = tool.parameters_schema();
        let search_mode = &schema["properties"]["search_mode"];
        assert_eq!(search_mode["type"], "string");
        let enum_values = search_mode["enum"].as_array().unwrap();
        assert_eq!(enum_values.len(), 3);
        assert!(enum_values.contains(&json!("bm25")));
        assert!(enum_values.contains(&json!("embedding")));
        assert!(enum_values.contains(&json!("hybrid")));
    }
}
