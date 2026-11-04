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

use zeroclaw_config::opt_in_tools::OptInTool;

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

/// The build group required by a tool's owning implementation.
///
/// This does not grant runtime selection, configured prerequisites or execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BuiltinToolCompilation {
    /// The implementation is not behind an opt-in tool feature.
    Always,
    /// Native adapters compiled by `tools-external`.
    External,
    /// An existing SaaS or coding-CLI compilation group.
    OptIn(OptInTool),
}

impl BuiltinToolCompilation {
    /// Whether this build carries the required compilation group.
    pub const fn compiled(self) -> bool {
        match self {
            Self::Always => true,
            Self::External => cfg!(feature = "tools-external"),
            Self::OptIn(tool) => {
                // A consumer can enable config features without compiling the concrete owner.
                let owner_compiled = match tool {
                    OptInTool::Jira => cfg!(feature = "tool-jira"),
                    OptInTool::Notion => cfg!(feature = "tool-notion"),
                    OptInTool::LinkedIn => cfg!(feature = "tool-linkedin"),
                    OptInTool::Composio => cfg!(feature = "tool-composio"),
                    OptInTool::GoogleWorkspace => cfg!(feature = "tool-google-workspace"),
                    OptInTool::Microsoft365 => cfg!(feature = "tool-microsoft365"),
                    OptInTool::ProjectIntel => cfg!(feature = "tool-project-intel"),
                    OptInTool::ClaudeCode => cfg!(feature = "tool-claude-code"),
                    OptInTool::ClaudeCodeRunner => cfg!(feature = "tool-claude-code-runner"),
                    OptInTool::CodexCli => cfg!(feature = "tool-codex-cli"),
                    OptInTool::GeminiCli => cfg!(feature = "tool-gemini-cli"),
                    OptInTool::OpenCodeCli => cfg!(feature = "tool-opencode-cli"),
                };
                owner_compiled && tool.compiled()
            }
        }
    }
}

/// One row of [`BUILTIN_TOOLS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BuiltinToolSpec {
    /// The exact string the tool's `name()` returns.
    pub name: &'static str,
    /// The tier the docs tables record for the tool.
    pub tier: ToolTier,
    /// The implementation's build requirement, independent of its tier.
    pub compilation: BuiltinToolCompilation,
}

impl BuiltinToolSpec {
    /// Whether the row's build group is compiled, before runtime selection and policy.
    pub const fn compiled(&self) -> bool {
        self.compilation.compiled()
    }
}

/// Every built-in agent tool, grouped by tier in the order of the docs tables.
pub const BUILTIN_TOOLS: &[BuiltinToolSpec] = &[
    // Tier 1: core.
    BuiltinToolSpec {
        name: "shell",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "file_read",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "file_write",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "file_edit",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "glob_search",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "content_search",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "git_operations",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "memory_store",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "memory_recall",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "memory_forget",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "web_fetch",
        tier: ToolTier::Core,
        compilation: BuiltinToolCompilation::Always,
    },
    // Tier 2: host-coupled. Scheduling.
    BuiltinToolSpec {
        name: "cron_add",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "cron_list",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "cron_remove",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "cron_update",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "cron_run",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "cron_runs",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "schedule",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Memory plane.
    BuiltinToolSpec {
        name: "memory_export",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "memory_purge",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Agent execution.
    BuiltinToolSpec {
        name: "spawn_subagent",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "delegate",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "send_message_to_peer",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Control plane.
    BuiltinToolSpec {
        name: "model_switch",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "model_routing_config",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "proxy_config",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Channel bridging.
    BuiltinToolSpec {
        name: "ask_user",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "escalate_to_human",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "reaction",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "poll",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "channel_room",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "send_via",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "git_forge",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Sessions.
    BuiltinToolSpec {
        name: "sessions_current",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sessions_list",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sessions_history",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sessions_send",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // SOP.
    BuiltinToolSpec {
        name: "sop_list",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sop_execute",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sop_advance",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sop_approve",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sop_status",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "sop_workshop",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Skills, task list, canvas, ACP delivery, and the provider-bound task.
    BuiltinToolSpec {
        name: "read_skill",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "TodoWrite",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "canvas",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "deliver_file",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "llm_task",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Sandbox-bound coding CLIs.
    BuiltinToolSpec {
        name: "claude_code",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::ClaudeCode),
    },
    BuiltinToolSpec {
        name: "codex_cli",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::CodexCli),
    },
    BuiltinToolSpec {
        name: "gemini_cli",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::GeminiCli),
    },
    BuiltinToolSpec {
        name: "opencode_cli",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::OpenCodeCli),
    },
    // Live config.
    BuiltinToolSpec {
        name: "a2a_discover",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "a2a_send",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "a2a_get_task",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "a2a_cancel",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "file_download",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Runtime-defined.
    BuiltinToolSpec {
        name: "security_ops",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Built outside the registry factory, by the scoped registry assembly.
    BuiltinToolSpec {
        name: "execute_pipeline",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "tool_search",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "mcp_resources",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "mcp_prompts",
        tier: ToolTier::Host,
        compilation: BuiltinToolCompilation::Always,
    },
    // Tier 3: optional. Utilities.
    BuiltinToolSpec {
        name: "calculator",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "weather",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "pushover",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "screenshot",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "image_info",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    // Browser.
    BuiltinToolSpec {
        name: "browser_open",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "browser",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "browser_delegate",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "text_browser",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    // Network.
    BuiltinToolSpec {
        name: "http_request",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "web_search_tool",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    // SaaS integrations.
    BuiltinToolSpec {
        name: "notion",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::Notion),
    },
    BuiltinToolSpec {
        name: "jira",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::Jira),
    },
    BuiltinToolSpec {
        name: "linkedin",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::LinkedIn),
    },
    BuiltinToolSpec {
        name: "composio",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::Composio),
    },
    BuiltinToolSpec {
        name: "google_workspace",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::GoogleWorkspace),
    },
    BuiltinToolSpec {
        name: "microsoft365",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::Microsoft365),
    },
    // Reporting.
    BuiltinToolSpec {
        name: "project_intel",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::ProjectIntel),
    },
    BuiltinToolSpec {
        name: "report_template",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::ProjectIntel),
    },
    // Coding runner.
    BuiltinToolSpec {
        name: "claude_code_runner",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::OptIn(OptInTool::ClaudeCodeRunner),
    },
    // Ops.
    BuiltinToolSpec {
        name: "backup",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "data_management",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "cloud_ops",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "cloud_patterns",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    // Media and transfer.
    BuiltinToolSpec {
        name: "image_gen",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "file_upload",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "file_upload_bundle",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    // Channel companions.
    BuiltinToolSpec {
        name: "discord_search",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
    },
    BuiltinToolSpec {
        name: "email_search",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    BuiltinToolSpec {
        name: "email_read",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::External,
    },
    // Knowledge.
    BuiltinToolSpec {
        name: "knowledge",
        tier: ToolTier::Optional,
        compilation: BuiltinToolCompilation::Always,
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

    #[test]
    fn external_compilation_rows_match_canonical_names() {
        let external: BTreeSet<&str> = BUILTIN_TOOLS
            .iter()
            .filter(|spec| spec.compilation == BuiltinToolCompilation::External)
            .map(|spec| spec.name)
            .collect();
        let expected: BTreeSet<&str> = zeroclaw_config::builtin_tools::EXTERNAL_TOOL_NAMES
            .iter()
            .copied()
            .collect();
        assert_eq!(
            external, expected,
            "external compilation requirements must cover exactly the canonical adapters"
        );
    }

    #[test]
    fn opt_in_compilation_rows_cover_canonical_groups() {
        let groups: BTreeSet<&str> = BUILTIN_TOOLS
            .iter()
            .filter_map(|spec| match spec.compilation {
                BuiltinToolCompilation::OptIn(tool) => Some(tool.feature()),
                _ => None,
            })
            .collect();
        let expected: BTreeSet<&str> = zeroclaw_config::opt_in_tools::OptInTool::ALL
            .iter()
            .map(|tool| tool.feature())
            .collect();
        assert_eq!(
            groups, expected,
            "inventory compilation groups must cover every canonical opt-in feature"
        );
    }

    #[test]
    #[ignore = "requires only zeroclaw-config/tool-notion; executed by the opt-in CI job"]
    fn config_only_notion_feature_does_not_compile_owner() {
        assert!(
            OptInTool::Notion.compiled(),
            "positive control: this graph must enable the canonical config feature"
        );
        assert!(
            !std::hint::black_box(cfg!(feature = "tool-notion")),
            "this graph must leave the concrete tools owner feature disabled"
        );
        let notion = builtin_tool("notion").expect("canonical Notion inventory row");
        assert_eq!(
            notion.compilation,
            BuiltinToolCompilation::OptIn(OptInTool::Notion)
        );
        assert!(
            !notion.compiled(),
            "a config-only feature must not report the unbuilt concrete tool as compiled"
        );
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
