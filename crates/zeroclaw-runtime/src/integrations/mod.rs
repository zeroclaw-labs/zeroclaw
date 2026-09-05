pub mod platform;
pub mod registry;

use crate::i18n::{get_required_cli_string, get_required_cli_string_with_args};
use anyhow::Result;
use zeroclaw_config::schema::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum IntegrationStatus {
    /// Fully implemented and ready to use
    Available,
    /// Configured and active
    Active,
}

/// Integration category
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum IntegrationCategory {
    Chat,
    AiModel,
    ToolsAutomation,
    Platform,
}

impl IntegrationCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat Providers",
            Self::AiModel => "AI Models",
            Self::ToolsAutomation => "Tools & Automation",
            Self::Platform => "Platforms",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Chat,
            Self::AiModel,
            Self::ToolsAutomation,
            Self::Platform,
        ]
    }
}

pub struct IntegrationEntry {
    pub name: String,
    pub description: String,
    pub category: IntegrationCategory,
    pub status: IntegrationStatus,
}

/// Handle the `integrations` CLI command
pub fn show_integration_info(config: &Config, name: &str) -> Result<()> {
    let entries = registry::all_integrations(config);
    let name_lower = name.to_lowercase();

    let Some(entry) = entries.iter().find(|e| e.name.to_lowercase() == name_lower) else {
        let message = get_required_cli_string_with_args(
            "cli-integrations-unknown",
            &[
                ("name", name),
                ("quickstart", "`zeroclaw quickstart`"),
                (
                    "channel_config",
                    "`zeroclaw config set channels.<name>.<field>=<value>`",
                ),
            ],
        );
        anyhow::bail!("{message}");
    };

    let icon = match entry.status {
        IntegrationStatus::Active => "✅",
        IntegrationStatus::Available => "⚪",
    };
    let status = localized_integration_status(entry.status);
    let category_heading = get_required_cli_string("cli-integrations-category-heading");
    let status_heading = get_required_cli_string("cli-integrations-status-heading");

    println!();
    println!(
        "  {} {} — {}",
        icon,
        console::style(&entry.name).white().bold(),
        entry.description
    );
    println!(
        "  {}: {}",
        category_heading,
        localized_integration_category(entry.category)
    );
    println!("  {}: {}", status_heading, status);
    println!();

    // Setup hints. Channel-specific steps that are not yet covered by a
    // standalone book walkthrough stay here so `zeroclaw integration info
    // <name>` keeps producing actionable output. The Chat-category catch-all
    // points operators at the per-channel config keys.
    match entry.name.as_str() {
        "Telegram" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-heading")
            );
            println!("    1. Message @BotFather on Telegram");
            println!("    2. Create a bot and copy the token");
            println!("    3. Run: zeroclaw config set channels.telegram.default.bot_token <token>");
            println!("    4. Start: zeroclaw channel start");
        }
        "Discord" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-heading")
            );
            println!("    1. Go to https://discord.com/developers/applications");
            println!("    2. Create app → Bot → Copy token");
            println!("    3. Enable MESSAGE CONTENT intent");
            println!("    4. Run: zeroclaw config set channels.discord.default.bot-token <token>");
        }
        "Slack" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-heading")
            );
            println!("    1. Go to https://api.slack.com/apps");
            println!("    2. Create app → Bot Token Scopes → Install");
            println!("    3. Run: zeroclaw config set channels.slack.default.bot-token <token>");
        }
        "iMessage" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-macos-heading")
            );
            println!("    Uses AppleScript bridge to send/receive iMessages.");
            println!("    Requires Full Disk Access in System Settings → Privacy.");
        }
        "OpenRouter" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-heading")
            );
            println!("    1. Get API key at https://openrouter.ai/keys");
            println!("    2. Run: zeroclaw quickstart --model-provider openrouter --api-key <key>");
            println!("    Access 200+ models with one key.");
        }
        "Ollama" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-heading")
            );
            println!("    1. Install: brew install ollama");
            println!("    2. Pull a model: ollama pull llama3");
            println!("    3. Set model_provider to 'ollama' in config.toml");
        }
        "Git" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-heading")
            );
            println!("    1. Create a GitHub App (Settings → Developer settings → GitHub Apps)");
            println!(
                "       Permissions: Issues R/W, Pull requests R/W, Metadata R. Webhook: off."
            );
            println!("    2. Generate a private key (.pem) and install the app on your repos");
            println!("    3. Run: zeroclaw config set channels.git.default.provider github");
            println!("       Run: zeroclaw config set channels.git.default.app-id <id>");
            println!(
                "       Run: zeroclaw config set channels.git.default.private-key-path <path>"
            );
            println!("       Run: zeroclaw config set channels.git.default.enabled true");
            println!("    4. Start: zeroclaw channel start");
        }
        "Browser" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-builtin-heading")
            );
            println!("    ZeroClaw can control Chrome/Chromium for web tasks.");
            println!("    Uses headless browser automation.");
        }
        "Cron" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-builtin-heading")
            );
            println!("    Schedule tasks in ~/.zeroclaw/workspace/cron/");
            println!("    Run: zeroclaw cron list");
        }
        "Weather" => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-builtin-heading")
            );
            println!("    Fetches live conditions from wttr.in, no API key required.");
            println!("    Supports city names, IATA airport codes, GPS coordinates,");
            println!("    postal/zip codes, and Unicode location names.");
        }
        _ if entry.category == IntegrationCategory::Chat => {
            println!(
                "  {}:",
                get_required_cli_string("cli-integrations-setup-heading")
            );
            println!("    Run: zeroclaw config set channels.<name>.<field>=<value>");
            println!("    (see docs/book/src/channels/overview.md for the per-channel field list)");
        }
        _ => {}
    }

    println!();
    Ok(())
}

fn localized_integration_category(category: IntegrationCategory) -> String {
    let key = match category {
        IntegrationCategory::Chat => "cli-integrations-category-chat",
        IntegrationCategory::AiModel => "cli-integrations-category-ai-model",
        IntegrationCategory::ToolsAutomation => "cli-integrations-category-tools-automation",
        IntegrationCategory::Platform => "cli-integrations-category-platform",
    };
    get_required_cli_string(key)
}

fn localized_integration_status(status: IntegrationStatus) -> String {
    let key = match status {
        IntegrationStatus::Available => "cli-integrations-status-available",
        IntegrationStatus::Active => "cli-integrations-status-active",
    };
    get_required_cli_string(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_category_all_includes_every_variant_once() {
        let all = IntegrationCategory::all();
        assert_eq!(all.len(), 4);

        let labels: Vec<&str> = all.iter().map(|cat| cat.label()).collect();
        assert!(labels.contains(&"Chat Providers"));
        assert!(labels.contains(&"AI Models"));
        assert!(labels.contains(&"Tools & Automation"));
        assert!(labels.contains(&"Platforms"));
    }

    #[test]
    fn handle_command_info_is_case_insensitive_for_known_integrations() {
        let config = Config::default();
        let entries = registry::all_integrations(&config);
        assert!(
            !entries.is_empty(),
            "registry should define at least one integration"
        );

        let upper_name = entries[0].name.to_uppercase();
        assert!(show_integration_info(&config, &upper_name).is_ok());
    }

    #[test]
    fn handle_command_info_returns_error_for_unknown_integration() {
        let config = Config::default();
        let requested_name = "definitely-not-a-real-integration";
        let err = show_integration_info(&config, requested_name).unwrap_err();
        let message = err.to_string();
        assert!(message.contains(requested_name));
        assert!(message.contains("`zeroclaw quickstart`"));
        assert!(message.contains("`zeroclaw config set channels.<name>.<field>=<value>`"));
    }
}
