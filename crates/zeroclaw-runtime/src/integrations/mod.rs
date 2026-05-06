pub mod platform;
pub mod registry;

use anyhow::Result;
use zeroclaw_config::schema::Config;

/// Integration status
///
/// Two states only: an integration is either configured (`Active`) or it
/// exists in the schema but isn't configured (`Available`). There is no
/// "coming soon" state — if it is not real, it does not get listed.
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

/// A registered integration. The `status` is computed against a
/// specific `&Config` at construction time (see
/// `registry::all_integrations`). `name` and `description` are owned
/// strings so the schema-derived path can build them at runtime from
/// the `ChannelsConfig` field set.
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
        anyhow::bail!(
            "Unknown integration: {name}. Check README for supported integrations or run `zeroclaw onboard` to configure channels/providers."
        );
    };

    let (icon, label) = match entry.status {
        IntegrationStatus::Active => ("✅", "Active"),
        IntegrationStatus::Available => ("⚪", "Available"),
    };

    println!();
    println!(
        "  {} {} — {}",
        icon,
        console::style(&entry.name).white().bold(),
        entry.description
    );
    println!("  Category: {}", entry.category.label());
    println!("  Status:   {label}");
    println!();

    // Setup hints. Channel-specific steps that are not yet covered by a
    // standalone book walkthrough stay here so `zeroclaw integration info
    // <name>` keeps producing actionable output. The Chat-category catch-all
    // handles channels with a stable onboard path and no special prerequisites.
    match entry.name.as_str() {
        "Telegram" => {
            println!("  Setup:");
            println!("    1. Message @BotFather on Telegram");
            println!("    2. Create a bot and copy the token");
            println!("    3. Run: zeroclaw onboard channels");
            println!("    4. Start: zeroclaw channel start");
        }
        "Discord" => {
            println!("  Setup:");
            println!("    1. Go to https://discord.com/developers/applications");
            println!("    2. Create app → Bot → Copy token");
            println!("    3. Enable MESSAGE CONTENT intent");
            println!("    4. Run: zeroclaw onboard channels");
        }
        "Slack" => {
            println!("  Setup:");
            println!("    1. Go to https://api.slack.com/apps");
            println!("    2. Create app → Bot Token Scopes → Install");
            println!("    3. Run: zeroclaw onboard channels");
        }
        "iMessage" => {
            println!("  Setup (macOS only):");
            println!("    Uses AppleScript bridge to send/receive iMessages.");
            println!("    Requires Full Disk Access in System Settings → Privacy.");
        }
        "OpenRouter" => {
            println!("  Setup:");
            println!("    1. Get API key at https://openrouter.ai/keys");
            println!("    2. Run: zeroclaw onboard");
            println!("    Access 200+ models with one key.");
        }
        "Ollama" => {
            println!("  Setup:");
            println!("    1. Install: brew install ollama");
            println!("    2. Pull a model: ollama pull llama3");
            println!("    3. Set provider to 'ollama' in config.toml");
        }
        "GitHub" => {
            println!("  Setup:");
            println!("    1. Create a personal access token at https://github.com/settings/tokens");
            println!("    2. Add to config: [integrations.github] token = \"ghp_...\"");
        }
        "Browser" => {
            println!("  Built-in:");
            println!("    ZeroClaw can control Chrome/Chromium for web tasks.");
            println!("    Uses headless browser automation.");
        }
        "Cron" => {
            println!("  Built-in:");
            println!("    Schedule tasks in ~/.zeroclaw/workspace/cron/");
            println!("    Run: zeroclaw cron list");
        }
        "Weather" => {
            println!("  Built-in:");
            println!("    Fetches live conditions from wttr.in, no API key required.");
            println!("    Supports city names, IATA airport codes, GPS coordinates,");
            println!("    postal/zip codes, and Unicode location names.");
        }
        _ if entry.category == IntegrationCategory::Chat => {
            println!("  Setup:");
            println!("    Run: zeroclaw onboard --channels-only");
        }
        _ => {}
    }

    println!();
    Ok(())
}

#[cfg(all(test, zeroclaw_root_crate))]
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
        let first_name = registry::all_integrations(&config)
            .first()
            .expect("registry should define at least one integration")
            .name
            .to_lowercase();

        let result = handle_command(
            crate::IntegrationCommands::Info { name: first_name },
            &config,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn handle_command_info_returns_error_for_unknown_integration() {
        let config = Config::default();
        let result = handle_command(
            crate::IntegrationCommands::Info {
                name: "definitely-not-a-real-integration".into(),
            },
            &config,
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown integration"));
    }
}
