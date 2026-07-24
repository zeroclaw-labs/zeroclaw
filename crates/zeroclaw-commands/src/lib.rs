//! Shared built-in channel slash command catalogue.

use serde::Serialize;

/// User-facing surface where a command can be advertised or accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandSurface {
    /// Command is available from the local CLI.
    Cli,
    /// Command is available from the Web UI/API surface.
    Web,
    /// Command is available from the terminal UI.
    Tui,
    /// Command is available from message-channel ingress.
    Channel,
}

/// Stable built-in command identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinCommandId {
    /// Show runtime command help.
    Help,
    /// Clear local conversation history.
    Clear,
    /// Start a fresh conversation/session.
    New,
    /// Stop current work where the owning surface supports it.
    Stop,
    /// Show or change the selected model.
    Model,
    /// List configured/known models.
    Models,
    /// Show runtime config visible to the surface.
    Config,
    /// Show or change model thinking/reasoning effort.
    Thinking,
    /// Manage durable goal-mode work.
    Goal,
}

/// Where command execution is owned today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandExecution {
    /// The client surface handles the command without runtime admission.
    ClientLocal,
    /// The channel/runtime command handler owns the command.
    RuntimeCommand,
    /// The durable goal controller/admission path owns the command.
    GoalAdmission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CommandSpec {
    /// Stable id for code that should not branch on display text.
    pub id: BuiltinCommandId,
    /// Canonical slash command name without the leading slash.
    pub name: &'static str,
    /// Additional names accepted by the same handler.
    pub aliases: &'static [&'static str],
    /// Human-readable usage shape shown in help.
    pub usage: &'static str,
    /// Fluent key for the localized command description.
    pub description_key: &'static str,
    /// Surfaces where this command may be advertised or accepted.
    pub surfaces: &'static [CommandSurface],
    /// Current owner of command execution.
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

impl CommandSurface {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Web => "web",
            Self::Tui => "tui",
            Self::Channel => "channel",
        }
    }
}

/// Parsed command token before surface-specific argument handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedCommandToken {
    /// Catalogue entry matched by the leading slash token.
    pub command: CommandSpec,
}

const CHANNEL_ONLY: &[CommandSurface] = &[CommandSurface::Channel];
const CHANNEL_AND_TUI: &[CommandSurface] = &[CommandSurface::Channel, CommandSurface::Tui];

static BUILTIN_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: BuiltinCommandId::Help,
        name: "help",
        aliases: &[],
        usage: "/help",
        description_key: "command-help-description",
        surfaces: CHANNEL_AND_TUI,
        execution: CommandExecution::ClientLocal,
    },
    CommandSpec {
        id: BuiltinCommandId::Clear,
        name: "clear",
        aliases: &[],
        usage: "/clear",
        description_key: "command-clear-description",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::New,
        name: "new",
        aliases: &["new-session"],
        usage: "/new",
        description_key: "command-new-description",
        surfaces: CHANNEL_AND_TUI,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Stop,
        name: "stop",
        aliases: &[],
        usage: "/stop",
        description_key: "command-stop-description",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Model,
        name: "model",
        aliases: &[],
        usage: "/model [--user|--agent] [model]",
        description_key: "command-model-description",
        surfaces: CHANNEL_AND_TUI,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Models,
        name: "models",
        aliases: &[],
        usage: "/models [provider]",
        description_key: "command-models-description",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Config,
        name: "config",
        aliases: &[],
        usage: "/config",
        description_key: "command-config-description",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Thinking,
        name: "thinking",
        aliases: &["think"],
        usage: "/thinking [off|low|medium|high|max|reset]",
        description_key: "command-thinking-description",
        surfaces: CHANNEL_ONLY,
        execution: CommandExecution::RuntimeCommand,
    },
    CommandSpec {
        id: BuiltinCommandId::Goal,
        name: "goal",
        aliases: &[],
        usage: "/goal <start <objective>|objective <objective>|status|budget|pause|resume [reason]|cancel|help> ...",
        description_key: "command-goal-description",
        surfaces: CHANNEL_ONLY,
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
    fn command_lookup_normalizes_case_whitespace_and_bot_suffix() {
        assert_eq!(
            normalize_command_name("  /MODEL@ZeroClaw_Bot  "),
            Some("model".to_string())
        );
        assert_eq!(
            command_by_name("  /THINK@ZeroClaw_Bot  ").map(|spec| spec.id),
            Some(BuiltinCommandId::Thinking)
        );
        assert_eq!(
            parse_command_token("  /NEW-SESSION@ZeroClaw_Bot  ", CommandSurface::Channel)
                .map(|parsed| parsed.command.id),
            Some(BuiltinCommandId::New)
        );
    }

    #[test]
    fn surface_filter_rejects_unavailable_commands() {
        assert!(parse_command_token("/config", CommandSurface::Channel).is_some());
        assert!(parse_command_token("/config", CommandSurface::Web).is_none());
        assert!(parse_command_token("/attach", CommandSurface::Tui).is_none());
        assert!(parse_command_token("/attach", CommandSurface::Channel).is_none());
    }

    #[test]
    fn normalize_command_name_empty_and_whitespace_returns_none() {
        assert_eq!(normalize_command_name(""), None);
        assert_eq!(normalize_command_name("   "), None);
        assert_eq!(normalize_command_name("\t\n"), None);
    }

    #[test]
    fn normalize_command_name_pure_slash_or_at_suffix_returns_none() {
        assert_eq!(normalize_command_name("/"), None);
        assert_eq!(normalize_command_name("@bot"), None);
        assert_eq!(normalize_command_name("/@bot"), None);
        assert_eq!(normalize_command_name("  /  @bot  "), None);
    }

    #[test]
    fn normalize_command_name_unicode_preserved() {
        assert_eq!(normalize_command_name("/新"), Some("新".to_string()));
        assert_eq!(normalize_command_name("/新@my_bot"), Some("新".to_string()));
    }

    #[test]
    fn tui_surface_advertises_help_model_and_new_only() {
        let tui_ids: Vec<BuiltinCommandId> = commands_for_surface(CommandSurface::Tui)
            .map(|spec| spec.id)
            .collect();
        assert_eq!(
            tui_ids,
            vec![
                BuiltinCommandId::Help,
                BuiltinCommandId::New,
                BuiltinCommandId::Model,
            ]
        );
        assert!(parse_command_token("/help", CommandSurface::Tui).is_some());
        assert!(parse_command_token("/model", CommandSurface::Tui).is_some());
        assert!(parse_command_token("/new", CommandSurface::Tui).is_some());
        assert!(parse_command_token("/new-session", CommandSurface::Tui).is_some());
        assert!(parse_command_token("/clear", CommandSurface::Tui).is_none());
    }

    #[test]
    fn goal_is_advertised_only_where_admission_is_implemented() {
        assert!(parse_command_token("/goal", CommandSurface::Web).is_none());
        assert!(parse_command_token("/goal", CommandSurface::Tui).is_none());
        assert!(parse_command_token("/goal", CommandSurface::Channel).is_some());
        let goal = command_by_name("/goal").expect("goal command should be registered");
        assert!(
            goal.usage.contains("start <objective>"),
            "goal command usage must advertise the required start objective"
        );
        assert!(
            goal.usage.contains("objective <objective>"),
            "goal command usage must advertise objective amendment syntax"
        );
    }
}
