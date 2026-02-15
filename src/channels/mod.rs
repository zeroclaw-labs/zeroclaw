pub mod cli;
pub mod discord;
pub mod imessage;
pub mod matrix;
pub mod orchestration;
pub mod slack;
pub mod telegram;
pub mod traits;
pub mod whatsapp;

pub use cli::CliChannel;
pub use discord::DiscordChannel;
pub use imessage::IMessageChannel;
pub use matrix::MatrixChannel;
pub use orchestration::{
    build_system_prompt, doctor_channels, start_channels,
};
pub use slack::SlackChannel;
pub use telegram::TelegramChannel;
pub use traits::Channel;
pub use whatsapp::WhatsAppChannel;

use crate::config::Config;
use anyhow::Result;

pub fn handle_command(command: super::ChannelCommands, config: &Config) -> Result<()> {
    match command {
        super::ChannelCommands::Start => {
            unreachable!("Start is handled in main.rs")
        }
        super::ChannelCommands::Doctor => {
            unreachable!("Doctor is handled in main.rs")
        }
        super::ChannelCommands::List => {
            println!("Channels:");
            println!("  ✅ CLI (always available)");
            for (name, configured) in [
                ("Telegram", config.channels_config.telegram.is_some()),
                ("Discord", config.channels_config.discord.is_some()),
                ("Slack", config.channels_config.slack.is_some()),
                ("Webhook", config.channels_config.webhook.is_some()),
                ("iMessage", config.channels_config.imessage.is_some()),
                ("Matrix", config.channels_config.matrix.is_some()),
                ("WhatsApp", config.channels_config.whatsapp.is_some()),
            ] {
                println!("  {} {name}", if configured { "✅" } else { "❌" });
            }
            println!("\nTo start channels: zeroclaw channel start");
            println!("To check health:    zeroclaw channel doctor");
            println!("To configure:      zeroclaw onboard");
            Ok(())
        }
        super::ChannelCommands::Add {
            channel_type,
            config: _,
        } => {
            anyhow::bail!(
                "Channel type '{channel_type}' — use `zeroclaw onboard` to configure channels"
            );
        }
        super::ChannelCommands::Remove { name } => {
            anyhow::bail!("Remove channel '{name}' — edit ~/.zeroclaw/config.toml directly");
        }
    }
}

