//! Compiled inventory of the built-in agent tools.
//!
//! [`BUILTIN_TOOLS`] is the canonical list of built-in agent-registry tool
//! names and the owner of each tool's tier. It is meant to become the compiled
//! built-in inventory that ADR-015 names as the owner of built-in availability;
//! that ADR is still proposed. A row's `name` is the exact string the tool's
//! `name()` returns; the tool itself still owns its behavior.
//!
//! The tier test, recorded on the tool inventory docs page, is defined against
//! the runtime composition contract and its `ToolRequest`, both proposed but
//! not yet in code.
//!
//! The inventory covers every tool the runtime's registry factory can
//! construct, plus the four that the scoped registry assembly mints:
//! `execute_pipeline`, `tool_search`, `mcp_resources`, and `mcp_prompts`. It
//! deliberately leaves out:
//!
//! - peripheral tools, which the hardware crate builds from the configured
//!   boards;
//! - WASM plugin tools, skill-defined tools, and MCP wrappers, whose names are
//!   decided at runtime;
//! - `vi_verify`, which is withheld from the model-visible registry;
//! - the session reset and delete tools, which the agent registry does not
//!   register;
//! - `skills_list`, `skill_view`, and `skill_manage`, which only the opt-in
//!   background skill review registers.
//!
//! Tests check this table against the tier tables in
//! `docs/book/src/developing/tool-inventory.md`, against the registry the
//! runtime assembles under a maximal configuration, against the literal tool
//! names in the production tool sources, and against several name-keyed tool
//! tables. That catches a renamed tool, a new tool that the maximal
//! configuration registers, and a new tool with a literal name. Edit the table
//! and the docs page together.

/// Where a built-in tool sits under the proposed runtime composition contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ToolTier {
    /// The retained core set.
    Core,
    /// Host-coupled: the runtime keeps constructing it, because it needs a
    /// runtime handle that `ToolRequest` would not carry, because the runtime
    /// keys execution behavior on its name, or by a recorded judgment call.
    Host,
    /// Optional: constructible from `ToolRequest` alone.
    Optional,
}

/// One row of [`BUILTIN_TOOLS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BuiltinToolSpec {
    /// The exact string the tool's `name()` returns.
    pub name: &'static str,
    /// The tier the docs tables record for the tool.
    pub tier: ToolTier,
}

/// Every built-in agent tool, grouped by tier in the order of the docs tables.
pub const BUILTIN_TOOLS: &[BuiltinToolSpec] = &[
    // Tier 1: core.
    BuiltinToolSpec {
        name: "shell",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "file_read",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "file_write",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "file_edit",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "glob_search",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "content_search",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "git_operations",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "memory_store",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "memory_recall",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "memory_forget",
        tier: ToolTier::Core,
    },
    BuiltinToolSpec {
        name: "web_fetch",
        tier: ToolTier::Core,
    },
    // Tier 2: host-coupled. Scheduling.
    BuiltinToolSpec {
        name: "cron_add",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "cron_list",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "cron_remove",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "cron_update",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "cron_run",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "cron_runs",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "schedule",
        tier: ToolTier::Host,
    },
    // Memory plane.
    BuiltinToolSpec {
        name: "memory_export",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "memory_purge",
        tier: ToolTier::Host,
    },
    // Agent execution.
    BuiltinToolSpec {
        name: "spawn_subagent",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "delegate",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "send_message_to_peer",
        tier: ToolTier::Host,
    },
    // Control plane.
    BuiltinToolSpec {
        name: "model_switch",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "model_routing_config",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "proxy_config",
        tier: ToolTier::Host,
    },
    // Channel bridging.
    BuiltinToolSpec {
        name: "ask_user",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "escalate_to_human",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "reaction",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "poll",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "channel_room",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "send_via",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "git_forge",
        tier: ToolTier::Host,
    },
    // Sessions.
    BuiltinToolSpec {
        name: "session_prompt_list",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "session_prompt_set",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "session_prompt_delete",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sessions_current",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sessions_list",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sessions_history",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sessions_send",
        tier: ToolTier::Host,
    },
    // SOP.
    BuiltinToolSpec {
        name: "sop_list",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sop_execute",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sop_advance",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sop_approve",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sop_status",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "sop_workshop",
        tier: ToolTier::Host,
    },
    // Skills, task list, canvas, ACP delivery, and the provider-bound task.
    BuiltinToolSpec {
        name: "read_skill",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "TodoWrite",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "canvas",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "deliver_file",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "llm_task",
        tier: ToolTier::Host,
    },
    // Sandbox-bound coding CLIs.
    BuiltinToolSpec {
        name: "claude_code",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "codex_cli",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "gemini_cli",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "opencode_cli",
        tier: ToolTier::Host,
    },
    // Live config.
    BuiltinToolSpec {
        name: "a2a_discover",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "a2a_send",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "a2a_get_task",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "a2a_cancel",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "file_download",
        tier: ToolTier::Host,
    },
    // Runtime-defined.
    BuiltinToolSpec {
        name: "security_ops",
        tier: ToolTier::Host,
    },
    // Built outside the registry factory, by the scoped registry assembly.
    BuiltinToolSpec {
        name: "execute_pipeline",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "tool_search",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "mcp_resources",
        tier: ToolTier::Host,
    },
    BuiltinToolSpec {
        name: "mcp_prompts",
        tier: ToolTier::Host,
    },
    // Tier 3: optional. Utilities.
    BuiltinToolSpec {
        name: "calculator",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "weather",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "pushover",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "screenshot",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "image_info",
        tier: ToolTier::Optional,
    },
    // Browser.
    BuiltinToolSpec {
        name: "browser_open",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "browser",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "browser_delegate",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "text_browser",
        tier: ToolTier::Optional,
    },
    // Network.
    BuiltinToolSpec {
        name: "http_request",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "web_search_tool",
        tier: ToolTier::Optional,
    },
    // SaaS integrations.
    BuiltinToolSpec {
        name: "notion",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "jira",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "linkedin",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "composio",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "google_workspace",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "microsoft365",
        tier: ToolTier::Optional,
    },
    // Reporting.
    BuiltinToolSpec {
        name: "project_intel",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "report_template",
        tier: ToolTier::Optional,
    },
    // Coding runner.
    BuiltinToolSpec {
        name: "claude_code_runner",
        tier: ToolTier::Optional,
    },
    // Ops.
    BuiltinToolSpec {
        name: "backup",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "data_management",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "cloud_ops",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "cloud_patterns",
        tier: ToolTier::Optional,
    },
    // Media and transfer.
    BuiltinToolSpec {
        name: "image_gen",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "file_upload",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "file_upload_bundle",
        tier: ToolTier::Optional,
    },
    // Channel companions.
    BuiltinToolSpec {
        name: "discord_search",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "email_search",
        tier: ToolTier::Optional,
    },
    BuiltinToolSpec {
        name: "email_read",
        tier: ToolTier::Optional,
    },
    // Knowledge.
    BuiltinToolSpec {
        name: "knowledge",
        tier: ToolTier::Optional,
    },
];

/// The inventory row for `name`, if `name` is a built-in tool.
pub fn builtin_tool(name: &str) -> Option<&'static BuiltinToolSpec> {
    BUILTIN_TOOLS.iter().find(|spec| spec.name == name)
}

/// Whether `name` is a built-in tool name.
pub fn is_builtin_tool_name(name: &str) -> bool {
    builtin_tool(name).is_some()
}

/// The rows of one tier, in inventory order.
pub fn builtin_tools_in(tier: ToolTier) -> impl Iterator<Item = &'static BuiltinToolSpec> {
    BUILTIN_TOOLS.iter().filter(move |spec| spec.tier == tier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Names in `names` that the inventory does not list.
    fn uninventoried<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        names
            .into_iter()
            .filter(|name| !is_builtin_tool_name(name))
            .collect()
    }

    #[test]
    fn names_are_unique() {
        let mut seen = BTreeSet::new();
        for spec in BUILTIN_TOOLS {
            assert!(
                seen.insert(spec.name),
                "built-in tool `{}` appears more than once in BUILTIN_TOOLS",
                spec.name
            );
        }
    }

    #[test]
    fn names_are_non_empty_without_whitespace() {
        for spec in BUILTIN_TOOLS {
            assert!(
                !spec.name.is_empty(),
                "BUILTIN_TOOLS has a row with an empty name"
            );
            assert!(
                !spec.name.chars().any(char::is_whitespace),
                "built-in tool name `{}` contains whitespace",
                spec.name
            );
        }
    }

    /// Deliberately pins the retained core set, the FND-001 D5 decision, here in
    /// code as well as in the docs table: changing the core set is a decision to
    /// record, not a table edit.
    #[test]
    fn core_rows_are_the_retained_core_set() {
        let core: BTreeSet<&str> = builtin_tools_in(ToolTier::Core)
            .map(|spec| spec.name)
            .collect();
        let expected = BTreeSet::from([
            "shell",
            "file_read",
            "file_write",
            "file_edit",
            "glob_search",
            "content_search",
            "git_operations",
            "memory_store",
            "memory_recall",
            "memory_forget",
            "web_fetch",
        ]);
        assert_eq!(
            core, expected,
            "the Core tier must be exactly the retained core set"
        );
    }

    #[test]
    fn lookup_finds_rows_and_rejects_unknown_names() {
        let shell = builtin_tool("shell").expect("shell is inventoried");
        assert_eq!(shell.name, "shell");
        assert_eq!(shell.tier, ToolTier::Core);
        assert_eq!(
            builtin_tool("TodoWrite").map(|spec| spec.tier),
            Some(ToolTier::Host)
        );
        assert_eq!(
            builtin_tool("calculator").map(|spec| spec.tier),
            Some(ToolTier::Optional)
        );
        assert!(is_builtin_tool_name("web_search_tool"));

        for unknown in ["", "no_such_tool", "todowrite", "web_search", "vi_verify"] {
            assert_eq!(
                builtin_tool(unknown),
                None,
                "`{unknown}` must not resolve to a built-in tool"
            );
            assert!(!is_builtin_tool_name(unknown));
        }
    }

    #[test]
    fn memory_tool_names_are_inventoried() {
        let missing = uninventoried(crate::MEMORY_TOOL_NAMES.iter().copied());
        assert!(
            missing.is_empty(),
            "MEMORY_TOOL_NAMES lists tools missing from BUILTIN_TOOLS: {missing:?}"
        );
    }

    #[test]
    fn default_approval_and_otp_tool_lists_are_inventoried() {
        let profile = zeroclaw_config::schema::RiskProfileConfig::default();
        let otp = zeroclaw_config::schema::OtpConfig::default();
        for (list, names) in [
            ("risk profile auto_approve", &profile.auto_approve),
            ("risk profile always_ask", &profile.always_ask),
            ("security.otp gated_actions", &otp.gated_actions),
        ] {
            let missing = uninventoried(names.iter().map(String::as_str));
            assert!(
                missing.is_empty(),
                "the default {list} list names tools missing from BUILTIN_TOOLS: {missing:?}"
            );
        }
    }

    #[test]
    fn shell_command_tools_are_inventoried() {
        let missing = uninventoried(
            zeroclaw_api::runtime_traits::SHELL_COMMAND_TOOLS
                .iter()
                .copied(),
        );
        assert!(
            missing.is_empty(),
            "SHELL_COMMAND_TOOLS names tools missing from BUILTIN_TOOLS: {missing:?}"
        );
    }
}
