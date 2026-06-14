//! Agent-loop tool that sends a message to a configured peer on a
//! shared channel.
//!
//! Validates the target against [`crate::peers::ResolvedPeers`] for
//! the calling agent on the requested channel: peers must mutually
//! opt in via a `[peer_groups.<name>]` block whose `agents` lists
//! both, OR appear on the group's `external_peers` list, before this
//! tool will deliver. Cross-channel sends from outside the resolver's
//! authorization surface are rejected.
//!
//! Delivery splits by target type:
//!
//! - **Agent-alias targets** route in-process via
//!   [`crate::agent::loop_::process_message`]: alpha calls
//!   `send_message_to_peer(target = "beta", ...)` and beta's agent
//!   loop runs the message. The two agents share the channel's bot
//!   identity, so an outbound to the channel would loop the bot's
//!   own handle back through inbound; the in-process path avoids
//!   that and lets the orchestrator deliver beta's reply (if any)
//!   through the same channel beta is configured on.
//!
//!   This path is fire-and-forget: the recipient runs on a detached
//!   `zeroclaw_spawn::spawn!`, so the sender's `ToolResult.success = true`
//!   means "accepted for processing", not "completed". Recipient
//!   errors do NOT surface to the sender; they are emitted via
//!   `tracing::warn!` inside the spawned task and via the recipient
//!   agent's own observability (audit log, runtime trace, channel
//!   reply). Observers diagnosing a missing peer message should look
//!   at the recipient's spans, not the sender's tool output.
//! - **External peers** (humans, external bots) route through
//!   [`crate::cron::scheduler::deliver_announcement`] with the
//!   external username as the platform target. The channel registry
//!   the binary registers at startup forwards the send to the live
//!   channel instance. This path is synchronous: the
//!   `deliver_announcement` future resolves before the tool returns,
//!   so a `success = false` here genuinely reflects a delivery
//!   failure.

use crate::cron::scheduler::deliver_announcement;
use crate::peers::resolve_peer_set;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;

/// Send a message to a peer on a shared channel. Bound to a single
/// calling agent's alias; the tool validates every send against that
/// agent's resolved peer set.
pub struct SendMessageToPeerTool {
    config: Arc<Config>,
    sender_alias: String,
    description: String,
}

impl SendMessageToPeerTool {
    pub fn new(config: Arc<Config>, sender_alias: impl Into<String>) -> Self {
        let sender_alias = sender_alias.into();
        let description = build_description();
        Self {
            config,
            sender_alias,
            description,
        }
    }
}

#[async_trait]
impl Tool for SendMessageToPeerTool {
    fn name(&self) -> &str {
        "send_message_to_peer"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel ref to deliver on (e.g. 'telegram.prod'). Must be one of the agent's configured channels and a channel the target peer also listens on."
                },
                "target": {
                    "type": "string",
                    "description": "Recipient identifier — a peer agent's alias or an external peer's username (e.g. '@operator')."
                },
                "message": {
                    "type": "string",
                    "description": "The message body to deliver."
                }
            },
            "required": ["channel", "target", "message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "channel"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing or empty 'channel' parameter")
            })?
            .to_string();
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "target"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing or empty 'target' parameter")
            })?
            .to_string();
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "message"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing or empty 'message' parameter")
            })?
            .to_string();

        let fallback_channel_type = channel.split_once('.').map(|(t, _)| t);
        let resolved = resolve_peer_set(&self.config, &self.sender_alias);

        if !resolved.is_known_peer(&channel, &target)
            && !fallback_channel_type
                .is_some_and(|channel_type| resolved.is_known_peer(channel_type, &target))
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "target {target:?} is not on agent {alias:?}'s resolved peer set for channel {channel:?}; \
                     add a [peer_groups.<name>] entry that lists both this agent and the target before sending",
                    alias = self.sender_alias,
                )),
            });
        }

        // The agent must itself listen on the channel — the target may
        // be reachable on it via a peer group, but a sender can't
        // dispatch on a channel it isn't configured for.
        let agent_listens_on_channel = self
            .config
            .agents
            .get(&self.sender_alias)
            .map(|a| a.channels.iter().any(|c| c.as_str() == channel.as_str()))
            .unwrap_or(false);
        if !agent_listens_on_channel {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "agent {alias:?} does not list channel {channel:?} on its `channels`; \
                     add the channel ref to [agents.{alias}.channels] before sending",
                    alias = self.sender_alias,
                )),
            });
        }

        // Agent-alias targets route in-process. The channel's bot
        // identity is shared between alpha and beta, so an outbound
        // to the channel would loop right back into inbound and the
        // self-loop guard would drop it. Agent-to-agent messaging is
        // process-internal by design; the channel registry only sees
        // sends with external recipients.
        let target_norm = target.trim_start_matches('@').to_ascii_lowercase();
        let target_is_agent = self
            .config
            .agents
            .keys()
            .any(|alias| alias.to_ascii_lowercase() == target_norm);

        if target_is_agent {
            // The target's resolved alias may differ in case from the
            // raw input ("@Beta" -> "beta"). Look up the canonical
            // alias once so the agent loop's `agent_alias` field
            // matches the [agents.<alias>] config key.
            let canonical = self
                .config
                .agents
                .keys()
                .find(|alias| alias.to_ascii_lowercase() == target_norm)
                .cloned()
                .unwrap_or_else(|| target.clone());

            // Fire-and-forget: agent-to-agent peer messages do not
            // synchronously block the sender on the recipient's full
            // turn (that's what the SubAgent surface is for). The
            // recipient processes on its own event loop and surfaces
            // its result via its own observability.
            let cfg = (*self.config).clone();
            let sender = self.sender_alias.clone();
            let recipient_alias = canonical.clone();
            let body = message.clone();
            zeroclaw_spawn::spawn!(async move {
                if let Err(e) =
                    crate::agent::loop_::process_message(cfg, &recipient_alias, &body, None).await
                {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"sender": sender, "recipient": recipient_alias, "error": format!("{}", e)})), "peer-message in-process delivery failed");
                }
            });

            return Ok(ToolResult {
                success: true,
                output: format!(
                    "accepted for in-process delivery to peer agent {canonical:?} (recipient runs detached; observe its agent loop for the actual outcome)"
                ),
                error: None,
            });
        }

        match deliver_announcement(&self.config, &channel, &target, None, &message).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("delivered to external peer {target:?} on {channel}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("delivery failed: {e:#}")),
            }),
        }
    }
}

fn build_description() -> String {
    String::from(
        "Send a message to a peer agent or external peer (human, external bot) \
         on a shared channel. The target must be a member of a peer group both \
         this agent and the target agree on (or an external peer listed on the \
         shared group's `external_peers`). Cross-agent sends to non-peers are \
         rejected at the tool boundary; the channel send only happens after \
         the peer-set check passes. Use the current channel ref unless the user \
         explicitly names another allowed channel. Do not pass peer group names \
         as the `channel` parameter.",
    )
}

const MAX_PEER_MAP_CHANNELS: usize = 6;
const MAX_PEER_MAP_ITEMS: usize = 8;
const MAX_PEER_MAP_CHARS: usize = 4_000;
const MAX_PROMPT_VALUE_CHARS: usize = 80;

/// Render current-channel peer guidance for the channel system prompt.
///
/// Source of truth is [`Config`]: this materializes a per-turn view from the
/// inbound channel ref rather than storing channel-scoped state in the static
/// tool registry.
#[must_use]
pub fn render_sender_peer_map_for_channel(
    config: &Config,
    sender_alias: &str,
    current_channel_ref: &str,
) -> String {
    let Some(agent) = config.agents.get(sender_alias) else {
        return String::new();
    };

    let resolved = resolve_peer_set(config, sender_alias);
    let agent_channels: Vec<&str> = agent.channels.iter().map(|c| c.as_str()).collect();
    let channel_keys: std::collections::BTreeSet<String> = resolved
        .agent_peers
        .keys()
        .chain(resolved.external_peers.keys())
        .filter(|channel_key| channel_matches_group(current_channel_ref, channel_key))
        .cloned()
        .collect();
    if channel_keys.is_empty() {
        return String::new();
    }

    let total_channels = channel_keys.len();
    let shown_channels: Vec<String> = channel_keys
        .iter()
        .take(MAX_PEER_MAP_CHANNELS)
        .cloned()
        .collect();

    let mut rows = Vec::new();
    for channel_key in shown_channels {
        let channel_refs =
            matching_current_channel_refs(&agent_channels, &channel_key, current_channel_ref);
        let channel_hint = if channel_refs.is_empty() {
            format!(
                "current channel ref {} is not listed in this agent's configured channels",
                prompt_value(current_channel_ref)
            )
        } else {
            format!("use channel ref {}", format_prompt_list(&channel_refs))
        };

        let group_names = group_names_for_channel(config, sender_alias, &channel_key);
        let agent_peers = agent_peers_for_current_channel(
            config,
            resolved.agent_peers.get(&channel_key),
            current_channel_ref,
        );
        let external_peers = set_values(resolved.external_peers.get(&channel_key));

        rows.push(format!(
            "- Channel scope {}: {channel_hint}; peer groups: {}; agent peers: {}; external peers: {}.",
            prompt_value(&channel_key),
            format_prompt_list(&group_names),
            format_prompt_list(&agent_peers),
            format_prompt_list(&external_peers)
        ));
    }

    let mut omitted_channels = total_channels.saturating_sub(MAX_PEER_MAP_CHANNELS);
    let mut peer_map = format_peer_map(sender_alias, &rows, omitted_channels);
    while peer_map.len() > MAX_PEER_MAP_CHARS && !rows.is_empty() {
        rows.pop();
        omitted_channels += 1;
        peer_map = format_peer_map(sender_alias, &rows, omitted_channels);
    }

    peer_map
}

fn format_peer_map(sender_alias: &str, rows: &[String], omitted_channels: usize) -> String {
    let mut row_text = rows.join("\n");
    if omitted_channels > 0 {
        if !row_text.is_empty() {
            row_text.push('\n');
        }
        row_text.push_str(&format!(
            "- {omitted_channels} more channel scopes omitted from this prompt map."
        ));
    }

    format!(
        "Current-channel peer map for agent {}:\n{}\n\
         For collective wording like \"the whole team\" or a peer group name, send only to the listed agent peers unless the user explicitly names external peers. \
         Do not pass peer group names as the `channel` parameter; pass one of the listed channel refs.",
        prompt_value(sender_alias),
        row_text
    )
}

fn group_names_for_channel(config: &Config, sender_alias: &str, channel_key: &str) -> Vec<String> {
    let mut names: Vec<String> = config
        .peer_groups
        .iter()
        .filter(|(_, group)| {
            group.channel == channel_key
                && group
                    .agents
                    .iter()
                    .any(|member| member.as_str() == sender_alias)
        })
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

fn set_values(set: Option<&std::collections::BTreeSet<String>>) -> Vec<String> {
    set.map(|set| set.iter().cloned().collect())
        .unwrap_or_default()
}

fn agent_peers_for_current_channel(
    config: &Config,
    set: Option<&std::collections::BTreeSet<String>>,
    current_channel_ref: &str,
) -> Vec<String> {
    set.map(|set| {
        set.iter()
            .filter(|alias| agent_listens_on_channel_ref(config, alias, current_channel_ref))
            .cloned()
            .collect()
    })
    .unwrap_or_default()
}

fn agent_listens_on_channel_ref(config: &Config, alias: &str, channel_ref: &str) -> bool {
    config
        .agents
        .iter()
        .find(|(configured_alias, _)| configured_alias.eq_ignore_ascii_case(alias))
        .is_some_and(|(_, agent)| agent.channels.iter().any(|channel| channel == channel_ref))
}

fn matching_current_channel_refs(
    agent_channels: &[&str],
    group_channel: &str,
    current_channel_ref: &str,
) -> Vec<String> {
    if !channel_matches_group(current_channel_ref, group_channel) {
        return Vec::new();
    }

    if agent_channels.contains(&current_channel_ref) {
        return vec![current_channel_ref.to_string()];
    }

    Vec::new()
}

fn channel_matches_group(channel_ref: &str, group_channel: &str) -> bool {
    if group_channel.contains('.') {
        return channel_ref == group_channel;
    }
    channel_ref == group_channel
        || channel_ref
            .split_once('.')
            .map(|(channel_type, _)| channel_type == group_channel)
            .unwrap_or(false)
}

fn prompt_value(value: &str) -> String {
    let mut chars = value.chars();
    let mut value: String = chars.by_ref().take(MAX_PROMPT_VALUE_CHARS).collect();
    if chars.next().is_some() {
        value.push_str("...");
    }
    serde_json::to_string(&value).unwrap_or_else(|_| "\"<invalid>\"".to_string())
}

fn format_prompt_list(values: &[String]) -> String {
    if values.is_empty() {
        return "none".to_string();
    }

    let mut parts: Vec<String> = values
        .iter()
        .take(MAX_PEER_MAP_ITEMS)
        .map(|value| prompt_value(value))
        .collect();
    if values.len() > MAX_PEER_MAP_ITEMS {
        parts.push(format!("and {} more", values.len() - MAX_PEER_MAP_ITEMS));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};
    use zeroclaw_config::schema::AliasedAgentConfig;

    #[test]
    fn description_stays_channel_agnostic() {
        let mut config = Config::default();
        config.agents.insert(
            "aa".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "beta".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "gamma".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.peer_groups.insert(
            "research".to_string(),
            PeerGroupConfig {
                channel: "telegram".to_string(),
                agents: vec![AgentAlias::new("aa"), AgentAlias::new("beta")],
                external_peers: vec![PeerUsername::new("@Operator")],
                ..PeerGroupConfig::default()
            },
        );

        let tool = SendMessageToPeerTool::new(Arc::new(config), "aa");
        assert!(crate::i18n::get_tool_description(tool.name()).is_none());
        let spec = tool.spec();
        let description = spec.description.as_str();

        assert!(!description.contains("Configured peer map for agent \"aa\""));
        assert!(!description.contains("Current-channel peer map for agent \"aa\""));
        assert!(!description.contains("peer groups: \"research\""));
        assert!(!description.contains("use channel ref \"telegram.prod\""));
        assert!(!description.contains("agent peers: \"beta\""));
        assert!(!description.contains("external peers: \"operator\""));
        assert!(!description.contains("\"@Operator\""));
        assert!(!description.contains("\"gamma\""));
        assert!(description.contains("Do not pass peer group names as the `channel` parameter"));
    }

    #[test]
    fn peer_map_for_channel_filters_to_current_channel_ref() {
        let mut config = Config::default();
        config.agents.insert(
            "aa".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into(), "telegram.dev".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "beta".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "gamma".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.dev".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.peer_groups.insert(
            "prod_ops".to_string(),
            PeerGroupConfig {
                channel: "telegram".to_string(),
                agents: vec![AgentAlias::new("aa"), AgentAlias::new("beta")],
                external_peers: vec![PeerUsername::new("@Operator")],
                ..PeerGroupConfig::default()
            },
        );
        config.peer_groups.insert(
            "dev_ops".to_string(),
            PeerGroupConfig {
                channel: "telegram.dev".to_string(),
                agents: vec![AgentAlias::new("aa"), AgentAlias::new("gamma")],
                ..PeerGroupConfig::default()
            },
        );

        let peer_map = render_sender_peer_map_for_channel(&config, "aa", "telegram.prod");

        assert!(peer_map.contains("Current-channel peer map for agent \"aa\""));
        assert!(peer_map.contains("peer groups: \"prod_ops\""));
        assert!(peer_map.contains("use channel ref \"telegram.prod\""));
        assert!(peer_map.contains("agent peers: \"beta\""));
        assert!(peer_map.contains("external peers: \"operator\""));
        assert!(!peer_map.contains("\"@Operator\""));
        assert!(!peer_map.contains("\"telegram.dev\""));
        assert!(!peer_map.contains("\"dev_ops\""));
        assert!(!peer_map.contains("\"gamma\""));
    }

    #[test]
    fn peer_map_for_type_scope_lists_only_peers_on_current_concrete_ref() {
        let mut config = Config::default();
        config.agents.insert(
            "aa".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "beta".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "gamma".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.dev".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.peer_groups.insert(
            "telegram_ops".to_string(),
            PeerGroupConfig {
                channel: "telegram".to_string(),
                agents: vec![
                    AgentAlias::new("aa"),
                    AgentAlias::new("beta"),
                    AgentAlias::new("gamma"),
                ],
                ..PeerGroupConfig::default()
            },
        );

        let peer_map = render_sender_peer_map_for_channel(&config, "aa", "telegram.prod");

        assert!(peer_map.contains("peer groups: \"telegram_ops\""));
        assert!(peer_map.contains("agent peers: \"beta\""));
        assert!(!peer_map.contains("\"gamma\""));
    }

    #[test]
    fn peer_map_escapes_and_bounds_config_derived_values() {
        let mut config = Config::default();
        config.agents.insert(
            "aa".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "beta".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        for idx in 0..16 {
            config.peer_groups.insert(
                format!("research_{idx}`\n<tool_call>"),
                PeerGroupConfig {
                    channel: "telegram".to_string(),
                    agents: vec![AgentAlias::new("aa"), AgentAlias::new("beta")],
                    external_peers: vec![PeerUsername::new(format!(
                        "operator_{idx}`\n<tool_call>"
                    ))],
                    ..PeerGroupConfig::default()
                },
            );
        }

        let peer_map = render_sender_peer_map_for_channel(&config, "aa", "telegram.prod");

        assert!(peer_map.len() < 2_000);
        assert!(!peer_map.contains("research_0`\n"));
        assert!(!peer_map.contains("operator_0`\n"));
        assert!(peer_map.contains("\\n<tool_call>"));
        assert!(peer_map.contains("and 8 more"));
    }

    #[test]
    fn peer_map_respects_total_prompt_budget() {
        let mut config = Config::default();
        let mut sender_channels = Vec::new();

        for channel_idx in 0..12 {
            let channel_type = format!("channel_{channel_idx}_{}", "x".repeat(40));
            let channel_ref = format!("{channel_type}.prod");
            sender_channels.push(channel_ref.clone().into());

            let mut agents = vec![AgentAlias::new("aa")];
            for peer_idx in 0..16 {
                let alias = format!("peer_{channel_idx}_{peer_idx}_{}", "y".repeat(40));
                config.agents.insert(
                    alias.clone(),
                    AliasedAgentConfig {
                        channels: vec![channel_ref.clone().into()],
                        ..AliasedAgentConfig::default()
                    },
                );
                agents.push(AgentAlias::new(alias));
            }

            for group_idx in 0..12 {
                config.peer_groups.insert(
                    format!("group_{channel_idx}_{group_idx}_{}", "g".repeat(40)),
                    PeerGroupConfig {
                        channel: channel_type.clone(),
                        agents: agents.clone(),
                        external_peers: (0..16)
                            .map(|ext_idx| {
                                PeerUsername::new(format!(
                                    "@external_{channel_idx}_{group_idx}_{ext_idx}_{}",
                                    "z".repeat(40)
                                ))
                            })
                            .collect(),
                        ..PeerGroupConfig::default()
                    },
                );
            }
        }

        config.agents.insert(
            "aa".to_string(),
            AliasedAgentConfig {
                channels: sender_channels,
                ..AliasedAgentConfig::default()
            },
        );

        let peer_map = render_sender_peer_map_for_channel(
            &config,
            "aa",
            "channel_0_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.prod",
        );

        assert!(
            peer_map.len() <= MAX_PEER_MAP_CHARS,
            "peer map length {} exceeded budget {}",
            peer_map.len(),
            MAX_PEER_MAP_CHARS
        );
    }
}
