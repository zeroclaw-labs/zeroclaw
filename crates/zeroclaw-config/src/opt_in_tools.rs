//! The opt-in tools a build carries.
//!
//! The SaaS and coding-CLI integrations compile only under their `tool-*`
//! features, but their config sections parse in every build, so a config
//! written for a full build loads anywhere. Validation checks a tool's own
//! settings only when the tool is compiled in: a build that cannot run a tool
//! never demands its credentials or resources, and
//! [`Config::collect_warnings`](crate::schema::Config::collect_warnings)
//! reports each enabled section it lacks once, under
//! [`TOOL_COMPILED_OUT`](crate::validation_warnings::TOOL_COMPILED_OUT).

use crate::schema::Config;

/// One opt-in tool: the config section that enables it and the Cargo feature
/// that compiles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptInTool {
    Jira,
    Notion,
    LinkedIn,
    Composio,
    GoogleWorkspace,
    Microsoft365,
    ProjectIntel,
    ClaudeCode,
    ClaudeCodeRunner,
    CodexCli,
    GeminiCli,
    OpenCodeCli,
}

impl OptInTool {
    pub const ALL: [Self; 12] = [
        Self::Jira,
        Self::Notion,
        Self::LinkedIn,
        Self::Composio,
        Self::GoogleWorkspace,
        Self::Microsoft365,
        Self::ProjectIntel,
        Self::ClaudeCode,
        Self::ClaudeCodeRunner,
        Self::CodexCli,
        Self::GeminiCli,
        Self::OpenCodeCli,
    ];

    /// The top-level config section that enables the tool.
    pub const fn section(self) -> &'static str {
        match self {
            Self::Jira => "jira",
            Self::Notion => "notion",
            Self::LinkedIn => "linkedin",
            Self::Composio => "composio",
            Self::GoogleWorkspace => "google_workspace",
            Self::Microsoft365 => "microsoft365",
            Self::ProjectIntel => "project_intel",
            Self::ClaudeCode => "claude_code",
            Self::ClaudeCodeRunner => "claude_code_runner",
            Self::CodexCli => "codex_cli",
            Self::GeminiCli => "gemini_cli",
            Self::OpenCodeCli => "opencode_cli",
        }
    }

    /// The Cargo feature that compiles the tool.
    pub const fn feature(self) -> &'static str {
        match self {
            Self::Jira => "tool-jira",
            Self::Notion => "tool-notion",
            Self::LinkedIn => "tool-linkedin",
            Self::Composio => "tool-composio",
            Self::GoogleWorkspace => "tool-google-workspace",
            Self::Microsoft365 => "tool-microsoft365",
            Self::ProjectIntel => "tool-project-intel",
            Self::ClaudeCode => "tool-claude-code",
            Self::ClaudeCodeRunner => "tool-claude-code-runner",
            Self::CodexCli => "tool-codex-cli",
            Self::GeminiCli => "tool-gemini-cli",
            Self::OpenCodeCli => "tool-opencode-cli",
        }
    }

    /// Whether this build carries the tool.
    pub const fn compiled(self) -> bool {
        match self {
            Self::Jira => cfg!(feature = "tool-jira"),
            Self::Notion => cfg!(feature = "tool-notion"),
            Self::LinkedIn => cfg!(feature = "tool-linkedin"),
            Self::Composio => cfg!(feature = "tool-composio"),
            Self::GoogleWorkspace => cfg!(feature = "tool-google-workspace"),
            Self::Microsoft365 => cfg!(feature = "tool-microsoft365"),
            Self::ProjectIntel => cfg!(feature = "tool-project-intel"),
            Self::ClaudeCode => cfg!(feature = "tool-claude-code"),
            Self::ClaudeCodeRunner => cfg!(feature = "tool-claude-code-runner"),
            Self::CodexCli => cfg!(feature = "tool-codex-cli"),
            Self::GeminiCli => cfg!(feature = "tool-gemini-cli"),
            Self::OpenCodeCli => cfg!(feature = "tool-opencode-cli"),
        }
    }

    /// Whether `config` enables the tool's section.
    pub fn enabled_in(self, config: &Config) -> bool {
        match self {
            Self::Jira => config.jira.enabled,
            Self::Notion => config.notion.enabled,
            Self::LinkedIn => config.linkedin.enabled,
            Self::Composio => config.composio.enabled,
            Self::GoogleWorkspace => config.google_workspace.enabled,
            Self::Microsoft365 => config.microsoft365.enabled,
            Self::ProjectIntel => config.project_intel.enabled,
            Self::ClaudeCode => config.claude_code.enabled,
            Self::ClaudeCodeRunner => config.claude_code_runner.enabled,
            Self::CodexCli => config.codex_cli.enabled,
            Self::GeminiCli => config.gemini_cli.enabled,
            Self::OpenCodeCli => config.opencode_cli.enabled,
        }
    }

    /// Whether `config` enables the tool and this build can run it, which is
    /// when the tool's own settings are validated.
    pub fn runs_in(self, config: &Config) -> bool {
        self.compiled() && self.enabled_in(config)
    }
}
