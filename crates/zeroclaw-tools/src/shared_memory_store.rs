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
use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};
use zeroclaw_memory::{Memory, MemoryCategory};

/// Cached, locale-resolved descriptions. Resolved once per tier from the tools
/// Fluent catalogue so `description()` can return a `&'static str` without
/// re-running Fluent on every call (mirrors `file_download`'s pattern).
static SHARED_DESCRIPTION: OnceLock<String> = OnceLock::new();
static SYSTEM_DESCRIPTION: OnceLock<String> = OnceLock::new();

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

    /// Fluent key for this tier's model-facing tool description.
    fn description_key(self) -> &'static str {
        match self {
            SharedTier::Shared => "tool-shared-memory-store",
            SharedTier::System => "tool-system-memory-store",
        }
    }

    /// Locale-resolved description, cached per tier.
    fn description(self) -> &'static str {
        let cell = match self {
            SharedTier::Shared => &SHARED_DESCRIPTION,
            SharedTier::System => &SYSTEM_DESCRIPTION,
        };
        cell.get_or_init(|| crate::i18n::get_required_tool_string(self.description_key()))
            .as_str()
    }

    /// Stable tier-label token ('shared' / 'system'). Passed as the `$tier`
    /// Fluent argument for the refusal/success/error messages; it is an
    /// identifier, not translated prose.
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

    /// Resolve a tools-catalogue Fluent string with no arguments.
    fn tool_msg(key: &str) -> String {
        crate::i18n::get_required_tool_string(key)
    }

    /// Resolve a tools-catalogue Fluent string with external arguments.
    fn tool_msg_with_args(key: &str, args: &[(&str, &str)]) -> String {
        crate::i18n::get_required_tool_string_with_args(key, args)
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
                    "description": Self::tool_msg("tool-shared-memory-store-param-key")
                },
                "content": {
                    "type": "string",
                    "description": Self::tool_msg("tool-shared-memory-store-param-content")
                },
                "category": {
                    "type": "string",
                    "description": Self::tool_msg("tool-shared-memory-store-param-category")
                }
            },
            "required": ["key", "content"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
            anyhow::Error::msg(Self::tool_msg("tool-shared-memory-store-error-missing-key"))
        })?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                anyhow::Error::msg(Self::tool_msg(
                    "tool-shared-memory-store-error-missing-content",
                ))
            })?;

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
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        // The backend must expose the shared-write capability. Non-hindsight
        // backends (or a misconfigured install) degrade gracefully rather than
        // erroring, mirroring the driver's degrade-gracefully style.
        let Some(writable) = self.memory.as_shared_writable() else {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(Self::tool_msg_with_args(
                    "tool-shared-memory-store-error-not-available",
                    &[("tier", self.tier.tier_label())],
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
                output: ToolOutput::default(),
                error: Some(Self::tool_msg_with_args(
                    "tool-shared-memory-store-error-no-bank",
                    &[("tier", self.tier.tier_label())],
                )),
            });
        }

        let result = match self.tier {
            SharedTier::Shared => writable.store_to_shared(key, content, category).await,
            SharedTier::System => writable.store_to_system(key, content, category).await,
        };
        match result {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: Self::tool_msg_with_args(
                    "tool-shared-memory-store-success",
                    &[("tier", self.tier.tier_label()), ("key", key)],
                )
                .into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(Self::tool_msg_with_args(
                    "tool-shared-memory-store-error-failed",
                    &[("tier", self.tier.tier_label()), ("error", &e.to_string())],
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

    #[test]
    fn descriptions_and_schema_resolve_through_fluent() {
        // The model-facing surface must resolve through the tools Fluent
        // catalogue, not bare literals: a missing key would surface as a
        // `{tool-...}` stub, so assert the resolved strings are real prose.
        let mem = hindsight_mem("http://127.0.0.1:1", Some("zeroclaw-house"), None);
        let shared = SharedMemoryStoreTool::new_shared(mem.clone(), test_security());
        let system = SharedMemoryStoreTool::new_system(mem, test_security());

        for tool in [&shared, &system] {
            let desc = tool.description();
            assert!(
                !desc.starts_with('{') && !desc.is_empty(),
                "description must resolve through Fluent, got: {desc}"
            );
        }
        // Tier-specific wording differs between the two descriptions.
        assert_ne!(shared.description(), system.description());

        let schema = shared.parameters_schema();
        for param in ["key", "content", "category"] {
            let d = schema["properties"][param]["description"]
                .as_str()
                .unwrap_or("");
            assert!(
                !d.is_empty() && !d.starts_with('{'),
                "param {param} description must resolve through Fluent, got: {d}"
            );
        }
    }

    #[test]
    fn active_locale_resolves_all_shared_keys() {
        // In the active (CI: English) locale, every key must resolve to real
        // prose and never the `{key}` missing-string stub.
        let keys = [
            "tool-shared-memory-store",
            "tool-system-memory-store",
            "tool-shared-memory-store-param-key",
            "tool-shared-memory-store-param-content",
            "tool-shared-memory-store-param-category",
            "tool-shared-memory-store-error-missing-key",
            "tool-shared-memory-store-error-missing-content",
        ];
        for key in keys {
            let value = crate::i18n::get_required_tool_string(key);
            assert!(
                !value.starts_with('{') && !value.is_empty(),
                "{key} must resolve in the active locale, got: {value}"
            );
        }
        // Argumented messages must inline their $tier / $key / $error values.
        let not_avail = crate::i18n::get_required_tool_string_with_args(
            "tool-shared-memory-store-error-not-available",
            &[("tier", "shared")],
        );
        assert!(not_avail.contains("shared"), "got: {not_avail}");
        let success = crate::i18n::get_required_tool_string_with_args(
            "tool-shared-memory-store-success",
            &[("tier", "system"), ("key", "trash_day")],
        );
        assert!(success.contains("system") && success.contains("trash_day"));
    }

    #[test]
    fn shared_fluent_keys_present_in_all_maintained_locales() {
        // Each locale the repo ships a tools.ftl for must define every shared/
        // system key, so switching locale never degrades the surface to a stub.
        // Parse the embedded catalogues directly (hermetic, no process locale).
        let catalogues = [
            (
                "en",
                include_str!("../../zeroclaw-runtime/locales/en/tools.ftl"),
            ),
            (
                "es",
                include_str!("../../zeroclaw-runtime/locales/es/tools.ftl"),
            ),
            (
                "fr",
                include_str!("../../zeroclaw-runtime/locales/fr/tools.ftl"),
            ),
            (
                "ja",
                include_str!("../../zeroclaw-runtime/locales/ja/tools.ftl"),
            ),
            (
                "zh-CN",
                include_str!("../../zeroclaw-runtime/locales/zh-CN/tools.ftl"),
            ),
        ];
        let keys = [
            "tool-shared-memory-store",
            "tool-system-memory-store",
            "tool-shared-memory-store-param-key",
            "tool-shared-memory-store-param-content",
            "tool-shared-memory-store-param-category",
            "tool-shared-memory-store-error-missing-key",
            "tool-shared-memory-store-error-missing-content",
            "tool-shared-memory-store-error-not-available",
            "tool-shared-memory-store-error-no-bank",
            "tool-shared-memory-store-success",
            "tool-shared-memory-store-error-failed",
        ];
        for (locale, ftl) in catalogues {
            for key in keys {
                let defined = ftl
                    .lines()
                    .any(|l| l.trim_start().starts_with(&format!("{key} =")));
                assert!(defined, "locale {locale} is missing Fluent key {key}");
            }
        }
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
