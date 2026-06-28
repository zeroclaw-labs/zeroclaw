//! Shared built-in slash command catalogue.
//!
//! This crate is the source of truth for built-in command metadata that must be
//! visible across channels, gateway clients, and local UI surfaces. Execution
//! remains with the owning surface; this catalogue owns discovery and token
//! recognition so command names do not drift between clients.

use serde::Serialize;

/// User-facing surface where a command can be advertised or accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSurface {
    Cli,
    Web,
    Tui,
    Channel,
}

/// Stable built-in command identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinCommandId {
    Help,
    Clear,
    New,
    Stop,
    Model,
    Models,
    ModelProvider,
    Config,
    Thinking,
    Attach,
    Attachments,
    ClearQueue,
    Detach,
    RestartSession,
    ToggleThinking,
    Goal,
}

/// Where command execution is owned today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandExecution {
    ClientLocal,
    RuntimeCommand,
    GoalAdmission,
}

/// Built-in command metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    pub id: BuiltinCommandId,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub description_key: &'static str,
    pub short_description: &'static str,
    pub surfaces: &'static [CommandSurface],
    pub execution: CommandExecution,
}

impl CommandSpec {
    pub fn supports(self, surface: CommandSurface) -> bool {
        self.surfaces.contains(&surface)
    }

    pub fn token_matches(self, token: &str) -> bool {
        self.name == token || self.aliases.contains(&token)
    }
}

/// Parsed command token before surface-specific argument handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedCommandToken {
    pub command: CommandSpec,
}

const ALL_SURFACES: &[CommandSurface] = &[
    CommandSurface::Cli,
    CommandSurface::Web,
    CommandSurface::Tui,
    CommandSurface::Channel,
];
const WEB_TUI_CHANNEL: &[CommandSurface] = &[
    CommandSurface::Web,
    CommandSurface::Tui,
    CommandSurface::Channel,
];
const TUI_ONLY: &[CommandSurface] = &[CommandSurface::Tui];
const CHANNEL_ONLY: &[CommandSurface] = &[CommandSurface::Channel];

static BUILTIN_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: BuiltinCommandId::Help,
        name: "help",
        aliases: &[],
        usage: "/help",
        description_key: "command-help-description",
        short_description: "Show command help",
        surfaces: ALL_SURFACES,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::Clear,
        name: "clear",
        aliases: &[],
        usage: "/clear",
        description_key: "command-clear-description",
        short_description: "Clear this conversation session",
        surfaces: WEB_TUI_CHANNEL,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::New,
        name: "new",
        aliases: &["new-session"],
        usage: "/new",
        description_key: "command-new-description",
        short_description: "Start a new conversation session",
        surfaces: WEB_TUI_CHANNEL,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Stop,
        name: "stop",
        aliases: &[],
        usage: "/stop",
        description_key: "command-stop-description",
        short_description: "Cancel the current in-flight task",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Model,
        name: "model",
        aliases: &[],
        usage: "/model [--user|--agent] [model]",
        description_key: "command-model-description",
        short_description: "Show or switch the current model",
        surfaces: WEB_TUI_CHANNEL,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Models,
        name: "models",
        aliases: &[],
        usage: "/models [provider]",
        description_key: "command-models-description",
        short_description: "List or switch model providers",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::ModelProvider,
        name: "model-provider",
        aliases: &[],
        usage: "/model-provider [provider]",
        description_key: "command-model-provider-description",
        short_description: "Show or switch the current model provider",
        surfaces: TUI_ONLY,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::Config,
        name: "config",
        aliases: &[],
        usage: "/config",
        description_key: "command-config-description",
        short_description: "Show current configuration",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Thinking,
        name: "thinking",
        aliases: &["think"],
        usage: "/thinking [off|low|medium|high|max|reset]",
        description_key: "command-thinking-description",
        short_description: "Show or change thinking level",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Attach,
        name: "attach",
        aliases: &[],
        usage: "/attach",
        description_key: "command-attach-description",
        short_description: "Attach a file",
        surfaces: TUI_ONLY,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::Attachments,
        name: "attachments",
        aliases: &[],
        usage: "/attachments",
        description_key: "command-attachments-description",
        short_description: "List queued attachments",
        surfaces: TUI_ONLY,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::ClearQueue,
        name: "clear-queue",
        aliases: &[],
        usage: "/clear-queue [index]",
        description_key: "command-clear-queue-description",
        short_description: "Clear queued turns",
        surfaces: TUI_ONLY,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::Detach,
        name: "detach",
        aliases: &[],
        usage: "/detach",
        description_key: "command-detach-description",
        short_description: "Remove queued attachments",
        surfaces: TUI_ONLY,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::RestartSession,
        name: "restart-session",
        aliases: &[],
        usage: "/restart-session",
        description_key: "command-restart-session-description",
        short_description: "Restart the current session",
        surfaces: TUI_ONLY,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::ToggleThinking,
        name: "toggle-thinking",
        aliases: &[],
        usage: "/toggle-thinking",
        description_key: "command-toggle-thinking-description",
        short_description: "Toggle thought visibility",
        surfaces: TUI_ONLY,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::Goal,
        name: "goal",
        aliases: &[],
        usage: "/goal <start|status|pause|resume|cancel> ...",
        description_key: "command-goal-description",
        short_description: "Manage a durable goal run",
        surfaces: WEB_TUI_CHANNEL,
        execution: CommandExecution::GoalAdmission,
    },
];

pub fn builtin_commands() -> &'static [CommandSpec] {
    BUILTIN_COMMANDS
}

pub fn commands_for_surface(
    surface: CommandSurface,
) -> impl Iterator<Item = CommandSpec> + 'static {
    BUILTIN_COMMANDS
        .iter()
        .copied()
        .filter(move |spec| spec.supports(surface))
}

pub fn command_by_name(name: &str) -> Option<CommandSpec> {
    let normalized = normalize_command_name(name)?;
    BUILTIN_COMMANDS
        .iter()
        .copied()
        .find(|spec| spec.token_matches(&normalized))
}

pub fn parse_command_token(token: &str, surface: CommandSurface) -> Option<ParsedCommandToken> {
    let command = command_by_name(token)?;
    command
        .supports(surface)
        .then_some(ParsedCommandToken { command })
}

pub fn normalize_command_name(token: &str) -> Option<String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_slash = trimmed.strip_prefix('/').unwrap_or(trimmed);
    let without_bot_suffix = without_slash.split('@').next().unwrap_or(without_slash);
    let normalized = without_bot_suffix.trim().to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

pub fn usage_for_surface(surface: CommandSurface) -> Vec<&'static str> {
    commands_for_surface(surface)
        .map(|spec| spec.usage)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lookup_accepts_slash_alias_and_bot_suffix() {
        assert_eq!(
            command_by_name("/new@zeroclaw_bot").map(|spec| spec.id),
            Some(BuiltinCommandId::New)
        );
        assert_eq!(
            command_by_name("new-session").map(|spec| spec.id),
            Some(BuiltinCommandId::New)
        );
        assert_eq!(
            command_by_name("/think").map(|spec| spec.id),
            Some(BuiltinCommandId::Thinking)
        );
    }

    #[test]
    fn surface_filter_rejects_unavailable_commands() {
        assert!(parse_command_token("/config", CommandSurface::Channel).is_some());
        assert!(parse_command_token("/config", CommandSurface::Web).is_none());
        assert!(parse_command_token("/attach", CommandSurface::Tui).is_some());
        assert!(parse_command_token("/attach", CommandSurface::Channel).is_none());
    }

    #[test]
    fn goal_is_advertised_on_user_surfaces() {
        assert!(parse_command_token("/goal", CommandSurface::Web).is_some());
        assert!(parse_command_token("/goal", CommandSurface::Tui).is_some());
        assert!(parse_command_token("/goal", CommandSurface::Channel).is_some());
    }
}
