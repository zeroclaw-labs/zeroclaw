use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_memory::{Memory, MemoryCategory};
use zeroclaw_policy::SecurityPolicy;
use zeroclaw_policy::ToolOperation;

/// Let the agent store memories — its own brain writes
pub struct MemoryStoreTool {
    memory: Arc<dyn Memory>,
    security: Arc<SecurityPolicy>,
}

impl MemoryStoreTool {
    pub fn new(memory: Arc<dyn Memory>, security: Arc<SecurityPolicy>) -> Self {
        Self { memory, security }
    }
}

#[async_trait]
impl Tool for MemoryStoreTool {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn description(&self) -> &str {
        "Store a fact, preference, or note in long-term memory. Use category 'core' for permanent facts, 'daily' for session notes, 'conversation' for chat context, or a custom category name."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Unique key for this memory (e.g. 'user_lang', 'project_stack')"
                },
                "content": {
                    "type": "string",
                    "description": "The information to remember"
                },
                "category": {
                    "type": "string",
                    "description": "Memory category: 'core' (permanent), 'daily' (session), 'conversation' (chat), or a custom category name. Defaults to 'core'."
                }
            },
            "required": ["key", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "key"})),
                "memory_store: missing key parameter"
            );
            anyhow::Error::msg("Missing 'key' parameter")
        })?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "content"})),
                    "memory_store: missing content parameter"
                );
                anyhow::Error::msg("Missing 'content' parameter")
            })?;

        let category = match args.get("category").and_then(|v| v.as_str()) {
            Some("core") | None => MemoryCategory::Core,
            Some("daily") => MemoryCategory::Daily,
            Some("conversation") => MemoryCategory::Conversation,
            Some(other) => MemoryCategory::Custom(other.to_string()),
        };

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "memory_store")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let session = if matches!(category, MemoryCategory::Conversation) {
            let session = zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
                .try_with(|sessions| sessions.first().cloned())
                .ok()
                .flatten();
            if session
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
                || self.memory.name() == "markdown"
            {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(crate::i18n::get_required_tool_string(
                        "memory-store-conversation-scope-required",
                    )),
                });
            }
            session
        } else {
            None
        };
        match self
            .memory
            .store(key, content, category, session.as_deref())
            .await
        {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Stored memory: {key}").into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to store memory: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::schema::MemoryConfig;
    use zeroclaw_memory::{NoneMemory, TACHI_COMPILED, create_memory};
    use zeroclaw_policy::SecurityPolicy;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn try_test_mem() -> Option<(TempDir, Arc<dyn Memory>)> {
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

    #[test]
    fn name_and_schema() {
        let mem: Arc<dyn Memory> = Arc::new(NoneMemory::new("none"));
        let tool = MemoryStoreTool::new(mem, test_security());
        assert_eq!(tool.name(), "memory_store");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["key"].is_object());
        assert!(schema["properties"]["content"].is_object());
    }

    #[tokio::test]
    async fn store_core() {
        let Some((_tmp, mem)) = try_test_mem() else {
            return;
        };
        let tool = MemoryStoreTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"key": "lang", "content": "Prefers Rust"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("lang"));

        let entry = mem.get("lang").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "Prefers Rust");
    }

    #[tokio::test]
    async fn store_with_category() {
        let Some((_tmp, mem)) = try_test_mem() else {
            return;
        };
        let tool = MemoryStoreTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"key": "note", "content": "Fixed bug", "category": "daily"}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn store_with_custom_category() {
        let Some((_tmp, mem)) = try_test_mem() else {
            return;
        };
        let tool = MemoryStoreTool::new(mem.clone(), test_security());
        let result = tool
            .execute(
                json!({"key": "proj_note", "content": "Uses async runtime", "category": "project"}),
            )
            .await
            .unwrap();
        assert!(result.success);

        let entry = mem.get("proj_note").await.unwrap().unwrap();
        assert_eq!(entry.content, "Uses async runtime");
        assert_eq!(entry.category, MemoryCategory::Custom("project".into()));
    }

    #[tokio::test]
    async fn conversation_store_is_recallable_only_in_its_canonical_memory_scope() {
        let Some((_tmp, memory)) = try_test_mem() else {
            return;
        };
        let store = MemoryStoreTool::new(Arc::clone(&memory), test_security());
        let args =
            json!({"key":"conversation", "content":"private fixture", "category":"conversation"});
        assert!(!store.execute(args.clone()).await.unwrap().success);
        let result = zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
            .scope(
                vec!["A".into(), "shared-sender".into()],
                store.execute(args.clone()),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(
            memory
                .get("conversation")
                .await
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("A")
        );
        let recall = crate::memory_recall::MemoryRecallTool::new(memory);
        for (scope, visible) in [("A", true), ("B", false)] {
            let result = zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
                .scope(
                    vec![scope.into(), "shared-sender".into()],
                    // Direct-tool callers supply the host policy explicitly.
                    zeroclaw_api::TOOL_LOOP_MEMORY_READ_POLICY.scope(
                        zeroclaw_api::memory_traits::MemoryReadPolicy::SessionScoped,
                        recall.execute(json!({"query":"*"})),
                    ),
                )
                .await
                .unwrap();
            assert_eq!(result.output.contains("private fixture"), visible);
        }
        let dir = tempfile::tempdir().unwrap();
        let markdown = MemoryStoreTool::new(
            Arc::new(zeroclaw_memory::MarkdownMemory::new("test", dir.path())),
            test_security(),
        );
        let denied = zeroclaw_api::TOOL_LOOP_MEMORY_SESSIONS
            .scope(vec!["A".into()], markdown.execute(args))
            .await
            .unwrap();
        assert!(!denied.success);
        assert!(!dir.path().join("memory").exists());
    }

    #[tokio::test]
    async fn store_missing_key() {
        let mem: Arc<dyn Memory> = Arc::new(NoneMemory::new("none"));
        let tool = MemoryStoreTool::new(mem, test_security());
        let result = tool.execute(json!({"content": "no key"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn store_missing_content() {
        let mem: Arc<dyn Memory> = Arc::new(NoneMemory::new("none"));
        let tool = MemoryStoreTool::new(mem, test_security());
        let result = tool.execute(json!({"key": "no_content"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn store_blocked_in_readonly_mode() {
        let Some((_tmp, mem)) = try_test_mem() else {
            return;
        };
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = MemoryStoreTool::new(mem.clone(), readonly);
        let result = tool
            .execute(json!({"key": "lang", "content": "Prefers Rust"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("read-only mode")
        );
        assert!(mem.get("lang").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn store_blocked_when_rate_limited() {
        let Some((_tmp, mem)) = try_test_mem() else {
            return;
        };
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = MemoryStoreTool::new(mem.clone(), limited);
        let result = tool
            .execute(json!({"key": "lang", "content": "Prefers Rust"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Rate limit exceeded")
        );
        assert!(mem.get("lang").await.unwrap().is_none());
    }
}
