//! Explicit shared/system memory write tools.
//!
//! Unlike [`memory_store`](crate::memory_store), which writes an agent's
//! PRIVATE memory (and is also the sink for automatic per-turn consolidation),
//! these tools write the shared/family and system tiers of a hindsight-backed
//! memory. They are DELIBERATE: the only path to a non-private bank is an
//! explicit call to one of these tools, so casual auto-ingest can never leak
//! into shared memory.
//!
//! Per-agent gating: because the tiers are separate TOOL NAMES
//! (`shared_memory_store`, `system_memory_store`), the existing risk-profile
//! `allowed_tools` / `excluded_tools` filter gates WHO can write each tier with
//! no new mechanism. An agent denied the tool never sees it, so the model
//! simply cannot write that tier and says so.
//!
//! Backend requirement: both tools need the shared-write capability
//! ([`SharedWritable`]), which only the hindsight backend implements. The tool
//! assembly constructs them only when `memory.as_shared_writable()` is `Some`,
//! so non-hindsight agents never get them.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};
use zeroclaw_memory::{Memory, MemoryCategory};

/// Which shared tier a write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SharedTier {
    /// Shared/family tier: permitted agents may write via `shared_memory_store`.
    Shared,
    /// System tier: admin agents may write via `system_memory_store`.
    System,
}

impl SharedTier {
    fn tool_name(self) -> &'static str {
        match self {
            SharedTier::Shared => "shared_memory_store",
            SharedTier::System => "system_memory_store",
        }
    }

    fn description(self) -> &'static str {
        match self {
            SharedTier::Shared => {
                "Store a fact into the SHARED household memory that every agent can read. \
                 Use ONLY when the user explicitly asks to remember something for everyone / \
                 the whole household. Not for private notes (use memory_store for those)."
            }
            SharedTier::System => {
                "Store a fact into the SYSTEM memory that every agent can read. Admin-only. \
                 Use ONLY when explicitly asked to record a system-wide operating fact. \
                 Not for private or household notes."
            }
        }
    }

    /// Human label for the "no bank configured" graceful-refusal message.
    fn tier_label(self) -> &'static str {
        match self {
            SharedTier::Shared => "shared",
            SharedTier::System => "system",
        }
    }
}

/// A native tool that writes one shared tier (shared or system) of a
/// hindsight-backed memory.
pub struct SharedMemoryStoreTool {
    memory: Arc<dyn Memory>,
    security: Arc<SecurityPolicy>,
    tier: SharedTier,
}

impl SharedMemoryStoreTool {
    /// Build the shared/family-tier write tool (`shared_memory_store`).
    #[must_use]
    pub fn new_shared(memory: Arc<dyn Memory>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            memory,
            security,
            tier: SharedTier::Shared,
        }
    }

    /// Build the system-tier write tool (`system_memory_store`).
    #[must_use]
    pub fn new_system(memory: Arc<dyn Memory>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            memory,
            security,
            tier: SharedTier::System,
        }
    }

    /// Whether the backing memory can write this tier (i.e. is hindsight AND
    /// has the relevant bank configured). Used by the assembler to skip
    /// registering a tool that could only ever refuse.
    #[must_use]
    pub fn is_supported(memory: &Arc<dyn Memory>, tier_is_system: bool) -> bool {
        memory.as_shared_writable().is_some_and(|w| {
            if tier_is_system {
                w.system_bank().is_some()
            } else {
                w.shared_bank().is_some()
            }
        })
    }
}

#[async_trait]
impl Tool for SharedMemoryStoreTool {
    fn name(&self) -> &str {
        self.tier.tool_name()
    }

    fn description(&self) -> &str {
        self.tier.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Unique key for this memory (e.g. 'house_wifi', 'trash_day')"
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
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("Missing 'key' parameter"))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("Missing 'content' parameter"))?;

        let category = match args.get("category").and_then(|v| v.as_str()) {
            Some("core") | None => MemoryCategory::Core,
            Some("daily") => MemoryCategory::Daily,
            Some("conversation") => MemoryCategory::Conversation,
            Some(other) => MemoryCategory::Custom(other.to_string()),
        };

        // Same write gate as the private memory tool: shared/system writes are
        // an Act and must respect read-only / rate-limit posture.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, self.name())
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // The backend must expose the shared-write capability. Non-hindsight
        // backends (or a misconfigured install) degrade gracefully rather than
        // erroring, mirroring the driver's degrade-gracefully style.
        let Some(writable) = self.memory.as_shared_writable() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "{} memory is not available on this backend",
                    self.tier.tier_label()
                )),
            });
        };

        let bank = match self.tier {
            SharedTier::Shared => writable.shared_bank(),
            SharedTier::System => writable.system_bank(),
        };
        if bank.is_none() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("no {} bank configured", self.tier.tier_label())),
            });
        }

        let result = match self.tier {
            SharedTier::Shared => writable.store_to_shared(key, content, category).await,
            SharedTier::System => writable.store_to_system(key, content, category).await,
        };
        match result {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Stored {} memory: {key}", self.tier.tier_label()),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to store {} memory: {e}",
                    self.tier.tier_label()
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_memory::HindsightMemory;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    /// A hindsight memory pointed at `base_url` with the given shared/system
    /// banks, exercised without touching the network unless a write fires.
    fn hindsight_mem(
        base_url: &str,
        shared: Option<&str>,
        system: Option<&str>,
    ) -> Arc<dyn Memory> {
        Arc::new(HindsightMemory::for_test(
            "tester",
            base_url,
            "zeroclaw-tester",
            shared,
            system,
            "test-token",
        ))
    }

    #[test]
    fn names_are_tier_specific() {
        let mem = hindsight_mem("http://127.0.0.1:1", Some("zeroclaw-house"), None);
        let shared = SharedMemoryStoreTool::new_shared(mem.clone(), test_security());
        let system = SharedMemoryStoreTool::new_system(mem, test_security());
        assert_eq!(shared.name(), "shared_memory_store");
        assert_eq!(system.name(), "system_memory_store");
        let schema = shared.parameters_schema();
        assert!(schema["properties"]["key"].is_object());
        assert!(schema["properties"]["content"].is_object());
    }

    #[tokio::test]
    async fn shared_write_to_named_bank_succeeds() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-house/memories"))
            .and(body_partial_json(serde_json::json!({
                "items": [{ "content": "trash goes out Tuesday", "context": "trash_day" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let mem = hindsight_mem(&server.uri(), Some("zeroclaw-house"), None);
        let tool = SharedMemoryStoreTool::new_shared(mem, test_security());
        let result = tool
            .execute(serde_json::json!({
                "key": "trash_day",
                "content": "trash goes out Tuesday"
            }))
            .await
            .unwrap();
        assert!(result.success, "expected success, got {result:?}");
        assert!(result.output.contains("shared"));
    }

    #[tokio::test]
    async fn no_shared_bank_returns_graceful_failure() {
        // No shared bank configured -> graceful refusal, no network call.
        let mem = hindsight_mem("http://127.0.0.1:1", None, None);
        let tool = SharedMemoryStoreTool::new_shared(mem, test_security());
        let result = tool
            .execute(serde_json::json!({"key": "k", "content": "v"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("no shared bank configured")
        );
    }

    #[tokio::test]
    async fn system_write_uses_system_bank() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/default/banks/zeroclaw-system/memories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let mem = hindsight_mem(
            &server.uri(),
            Some("zeroclaw-house"),
            Some("zeroclaw-system"),
        );
        let tool = SharedMemoryStoreTool::new_system(mem, test_security());
        let result = tool
            .execute(serde_json::json!({"key": "policy", "content": "reboot weekly"}))
            .await
            .unwrap();
        assert!(result.success, "expected success, got {result:?}");
        assert!(result.output.contains("system"));
    }

    #[tokio::test]
    async fn blocked_in_readonly_mode() {
        let mem = hindsight_mem("http://127.0.0.1:1", Some("zeroclaw-house"), None);
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = SharedMemoryStoreTool::new_shared(mem, readonly);
        let result = tool
            .execute(serde_json::json!({"key": "k", "content": "v"}))
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
    }

    #[test]
    fn is_supported_reflects_configured_banks() {
        let with_shared = hindsight_mem("http://127.0.0.1:1", Some("zeroclaw-house"), None);
        assert!(SharedMemoryStoreTool::is_supported(&with_shared, false));
        assert!(!SharedMemoryStoreTool::is_supported(&with_shared, true));

        let with_system = hindsight_mem(
            "http://127.0.0.1:1",
            Some("zeroclaw-house"),
            Some("zeroclaw-system"),
        );
        assert!(SharedMemoryStoreTool::is_supported(&with_system, true));

        let none = hindsight_mem("http://127.0.0.1:1", None, None);
        assert!(!SharedMemoryStoreTool::is_supported(&none, false));
        assert!(!SharedMemoryStoreTool::is_supported(&none, true));
    }
}
