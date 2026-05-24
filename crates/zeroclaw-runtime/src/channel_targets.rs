//! Build the configured channel targets section for system prompt injection.
//!
//! Returns `Some(string)` if any channels have `default_target` set, `None` otherwise.

use zeroclaw_config::schema::Config;

pub fn build_channel_targets(config: &Config) -> Option<String> {
    let mut entries: Vec<(String, String)> = Vec::new();

    for (alias, cfg) in &config.channels.telegram {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("telegram.{alias}"), t.clone()));
        }
    }
    for (alias, cfg) in &config.channels.discord {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("discord.{alias}"), t.clone()));
        }
    }
    for (alias, cfg) in &config.channels.slack {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("slack.{alias}"), t.clone()));
        }
    }
    for (alias, cfg) in &config.channels.mattermost {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("mattermost.{alias}"), t.clone()));
        }
    }
    for (alias, cfg) in &config.channels.matrix {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("matrix.{alias}"), t.clone()));
        }
    }
    for (alias, cfg) in &config.channels.irc {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("irc.{alias}"), t.clone()));
        }
    }
    for (alias, cfg) in &config.channels.signal {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("signal.{alias}"), t.clone()));
        }
    }
    for (alias, cfg) in &config.channels.whatsapp {
        if cfg.enabled
            && let Some(ref t) = cfg.default_target
        {
            entries.push((format!("whatsapp.{alias}"), t.clone()));
        }
    }

    if entries.is_empty() {
        return None;
    }

    let mut out = String::new();
    out.push_str("## Configured Channel Targets\n\n");
    out.push_str("When responding to the user, ALWAYS use the `channel_send` tool to deliver your final response to their configured channel. Do NOT just reply in text — invoke the tool with the composite key (e.g. `telegram.default`) as the `channel` parameter and the recipient below as the `to` parameter.\n\n");
    for (channel, target) in &entries {
        out.push_str(&format!("- {channel}: {target}\n"));
    }
    Some(out)
}
