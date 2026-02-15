use anyhow::Result;
use console::style;
use dialoguer::{Input, Select};

use crate::config::{
    ChannelsConfig, DiscordConfig, IMessageConfig, SlackConfig, TelegramConfig,
};
use crate::onboard::channel_setup_integrations::{
    setup_matrix_channel, setup_webhook_channel, setup_whatsapp_channel,
};
use crate::onboard::common::print_bullet;

#[allow(clippy::too_many_lines)]
pub(crate) fn setup_channels() -> Result<ChannelsConfig> {
    print_bullet("Channels let you talk to ZeroClaw from anywhere.");
    print_bullet("CLI is always available. Connect more channels now.");
    println!();

    let mut config = ChannelsConfig {
        cli: true,
        telegram: None,
        discord: None,
        slack: None,
        webhook: None,
        imessage: None,
        matrix: None,
        whatsapp: None,
    };

    loop {
        let options = vec![
            format!(
                "Telegram   {}",
                if config.telegram.is_some() {
                    "✅ connected"
                } else {
                    "— connect your bot"
                }
            ),
            format!(
                "Discord    {}",
                if config.discord.is_some() {
                    "✅ connected"
                } else {
                    "— connect your bot"
                }
            ),
            format!(
                "Slack      {}",
                if config.slack.is_some() {
                    "✅ connected"
                } else {
                    "— connect your bot"
                }
            ),
            format!(
                "iMessage   {}",
                if config.imessage.is_some() {
                    "✅ configured"
                } else {
                    "— macOS only"
                }
            ),
            format!(
                "Matrix     {}",
                if config.matrix.is_some() {
                    "✅ connected"
                } else {
                    "— self-hosted chat"
                }
            ),
            format!(
                "WhatsApp   {}",
                if config.whatsapp.is_some() {
                    "✅ connected"
                } else {
                    "— Business Cloud API"
                }
            ),
            format!(
                "Webhook    {}",
                if config.webhook.is_some() {
                    "✅ configured"
                } else {
                    "— HTTP endpoint"
                }
            ),
            "Done — finish setup".to_string(),
        ];

        let choice = Select::new()
            .with_prompt("  Connect a channel (or Done to continue)")
            .items(&options)
            .default(7)
            .interact()?;

        match choice {
            0 => setup_telegram_channel(&mut config)?,
            1 => setup_discord_channel(&mut config)?,
            2 => setup_slack_channel(&mut config)?,
            3 => setup_imessage_channel(&mut config)?,
            4 => setup_matrix_channel(&mut config)?,
            5 => setup_whatsapp_channel(&mut config)?,
            6 => setup_webhook_channel(&mut config)?,
            _ => break,
        }
        println!();
    }

    let mut active: Vec<&str> = vec!["CLI"];
    if config.telegram.is_some() {
        active.push("Telegram");
    }
    if config.discord.is_some() {
        active.push("Discord");
    }
    if config.slack.is_some() {
        active.push("Slack");
    }
    if config.imessage.is_some() {
        active.push("iMessage");
    }
    if config.matrix.is_some() {
        active.push("Matrix");
    }
    if config.whatsapp.is_some() {
        active.push("WhatsApp");
    }
    if config.webhook.is_some() {
        active.push("Webhook");
    }

    println!(
        "  {} Channels: {}",
        style("✓").green().bold(),
        style(active.join(", ")).green()
    );

    Ok(config)
}

fn setup_telegram_channel(config: &mut ChannelsConfig) -> Result<()> {
    println!();
    println!(
        "  {} {}",
        style("Telegram Setup").white().bold(),
        style("— talk to ZeroClaw from Telegram").dim()
    );
    print_bullet("1. Open Telegram and message @BotFather");
    print_bullet("2. Send /newbot and follow the prompts");
    print_bullet("3. Copy the bot token and paste it below");
    println!();

    let token: String = Input::new()
        .with_prompt("  Bot token (from @BotFather)")
        .interact_text()?;

    if token.trim().is_empty() {
        println!("  {} Skipped", style("→").dim());
        return Ok(());
    }

    print!("  {} Testing connection... ", style("⏳").dim());
    let client = reqwest::blocking::Client::new();
    let url = format!("https://api.telegram.org/bot{token}/getMe");
    match client.get(&url).send() {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().unwrap_or_default();
            let bot_name = data
                .get("result")
                .and_then(|r| r.get("username"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            println!(
                "\r  {} Connected as @{bot_name}        ",
                style("✅").green().bold()
            );
        }
        _ => {
            println!(
                "\r  {} Connection failed — check your token and try again",
                style("❌").red().bold()
            );
            return Ok(());
        }
    }

    print_bullet("Allowlist your own Telegram identity first (recommended for secure + fast setup).");
    print_bullet(
        "Use your @username without '@' (example: argenis), or your numeric Telegram user ID.",
    );
    print_bullet("Use '*' only for temporary open testing.");

    let users_str: String = Input::new()
        .with_prompt(
            "  Allowed Telegram identities (comma-separated: username without '@' and/or numeric user ID, '*' for all)",
        )
        .allow_empty(true)
        .interact_text()?;

    let allowed_users = if users_str.trim() == "*" {
        vec!["*".into()]
    } else {
        users_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    if allowed_users.is_empty() {
        println!(
            "  {} No users allowlisted — Telegram inbound messages will be denied until you add your username/user ID or '*'.",
            style("⚠").yellow().bold()
        );
    }

    config.telegram = Some(TelegramConfig {
        bot_token: token,
        allowed_users,
    });

    Ok(())
}

fn setup_discord_channel(config: &mut ChannelsConfig) -> Result<()> {
    println!();
    println!(
        "  {} {}",
        style("Discord Setup").white().bold(),
        style("— talk to ZeroClaw from Discord").dim()
    );
    print_bullet("1. Go to https://discord.com/developers/applications");
    print_bullet("2. Create a New Application → Bot → Copy token");
    print_bullet("3. Enable MESSAGE CONTENT intent under Bot settings");
    print_bullet("4. Invite bot to your server with messages permission");
    println!();

    let token: String = Input::new().with_prompt("  Bot token").interact_text()?;

    if token.trim().is_empty() {
        println!("  {} Skipped", style("→").dim());
        return Ok(());
    }

    print!("  {} Testing connection... ", style("⏳").dim());
    let client = reqwest::blocking::Client::new();
    match client
        .get("https://discord.com/api/v10/users/@me")
        .header("Authorization", format!("Bot {token}"))
        .send()
    {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().unwrap_or_default();
            let bot_name = data
                .get("username")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            println!(
                "\r  {} Connected as {bot_name}        ",
                style("✅").green().bold()
            );
        }
        _ => {
            println!(
                "\r  {} Connection failed — check your token and try again",
                style("❌").red().bold()
            );
            return Ok(());
        }
    }

    let guild: String = Input::new()
        .with_prompt("  Server (guild) ID (optional, Enter to skip)")
        .allow_empty(true)
        .interact_text()?;

    print_bullet("Allowlist your own Discord user ID first (recommended).");
    print_bullet(
        "Get it in Discord: Settings -> Advanced -> Developer Mode (ON), then right-click your profile -> Copy User ID.",
    );
    print_bullet("Use '*' only for temporary open testing.");

    let allowed_users_str: String = Input::new()
        .with_prompt(
            "  Allowed Discord user IDs (comma-separated, recommended: your own ID, '*' for all)",
        )
        .allow_empty(true)
        .interact_text()?;

    let allowed_users = if allowed_users_str.trim().is_empty() {
        vec![]
    } else {
        allowed_users_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    if allowed_users.is_empty() {
        println!(
            "  {} No users allowlisted — Discord inbound messages will be denied until you add IDs or '*'.",
            style("⚠").yellow().bold()
        );
    }

    config.discord = Some(DiscordConfig {
        bot_token: token,
        guild_id: if guild.is_empty() { None } else { Some(guild) },
        allowed_users,
    });

    Ok(())
}

#[allow(clippy::too_many_lines)]
fn setup_slack_channel(config: &mut ChannelsConfig) -> Result<()> {
    println!();
    println!(
        "  {} {}",
        style("Slack Setup").white().bold(),
        style("— talk to ZeroClaw from Slack").dim()
    );
    print_bullet("1. Go to https://api.slack.com/apps → Create New App");
    print_bullet("2. Add Bot Token Scopes: chat:write, channels:history");
    print_bullet("3. Install to workspace and copy the Bot Token");
    println!();

    let token: String = Input::new()
        .with_prompt("  Bot token (xoxb-...)")
        .interact_text()?;

    if token.trim().is_empty() {
        println!("  {} Skipped", style("→").dim());
        return Ok(());
    }

    print!("  {} Testing connection... ", style("⏳").dim());
    let client = reqwest::blocking::Client::new();
    match client
        .get("https://slack.com/api/auth.test")
        .bearer_auth(&token)
        .send()
    {
        Ok(resp) if resp.status().is_success() => {
            let data: serde_json::Value = resp.json().unwrap_or_default();
            let ok = data
                .get("ok")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let team = data
                .get("team")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            if ok {
                println!(
                    "\r  {} Connected to workspace: {team}        ",
                    style("✅").green().bold()
                );
            } else {
                let err = data
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown error");
                println!("\r  {} Slack error: {err}", style("❌").red().bold());
                return Ok(());
            }
        }
        _ => {
            println!(
                "\r  {} Connection failed — check your token",
                style("❌").red().bold()
            );
            return Ok(());
        }
    }

    let app_token: String = Input::new()
        .with_prompt("  App token (xapp-..., optional, Enter to skip)")
        .allow_empty(true)
        .interact_text()?;

    let channel: String = Input::new()
        .with_prompt("  Default channel ID (optional, Enter to skip)")
        .allow_empty(true)
        .interact_text()?;

    print_bullet("Allowlist your own Slack member ID first (recommended).");
    print_bullet(
        "Member IDs usually start with 'U' (open your Slack profile -> More -> Copy member ID).",
    );
    print_bullet("Use '*' only for temporary open testing.");

    let allowed_users_str: String = Input::new()
        .with_prompt(
            "  Allowed Slack user IDs (comma-separated, recommended: your own member ID, '*' for all)",
        )
        .allow_empty(true)
        .interact_text()?;

    let allowed_users = if allowed_users_str.trim().is_empty() {
        vec![]
    } else {
        allowed_users_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    if allowed_users.is_empty() {
        println!(
            "  {} No users allowlisted — Slack inbound messages will be denied until you add IDs or '*'.",
            style("⚠").yellow().bold()
        );
    }

    config.slack = Some(SlackConfig {
        bot_token: token,
        app_token: if app_token.is_empty() {
            None
        } else {
            Some(app_token)
        },
        channel_id: if channel.is_empty() {
            None
        } else {
            Some(channel)
        },
        allowed_users,
    });

    Ok(())
}

fn setup_imessage_channel(config: &mut ChannelsConfig) -> Result<()> {
    println!();
    println!(
        "  {} {}",
        style("iMessage Setup").white().bold(),
        style("— macOS only, reads from Messages.app").dim()
    );

    if !cfg!(target_os = "macos") {
        println!(
            "  {} iMessage is only available on macOS.",
            style("⚠").yellow().bold()
        );
        return Ok(());
    }

    print_bullet("ZeroClaw reads your iMessage database and replies via AppleScript.");
    print_bullet("You need to grant Full Disk Access to your terminal in System Settings.");
    println!();

    let contacts_str: String = Input::new()
        .with_prompt("  Allowed contacts (comma-separated phone/email, or * for all)")
        .default("*".into())
        .interact_text()?;

    let allowed_contacts = if contacts_str.trim() == "*" {
        vec!["*".into()]
    } else {
        contacts_str
            .split(',')
            .map(|s| s.trim().to_string())
            .collect()
    };

    config.imessage = Some(IMessageConfig { allowed_contacts });
    println!(
        "  {} iMessage configured (contacts: {})",
        style("✅").green().bold(),
        style(&contacts_str).cyan()
    );

    Ok(())
}
