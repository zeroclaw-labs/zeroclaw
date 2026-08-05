//! Agent-loop tool that sends a message to a configured peer on a
//! shared channel.

use crate::agent::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext, TurnUsage,
    tool_loop_cost_tracking_context_for_agent,
};
use crate::cron::scheduler::deliver_announcement;
use crate::peers::resolve_peer_set;
use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
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

    fn param_domains(&self) -> Vec<(&'static str, zeroclaw_api::tool::OptionDomain)> {
        use zeroclaw_api::tool::OptionDomain;
        vec![
            ("channel", OptionDomain::ChannelRefs),
            ("target", OptionDomain::PeerTargets),
        ]
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
                output: ToolOutput::default(),
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
                output: ToolOutput::default(),
                error: Some(format!(
                    "agent {alias:?} does not list channel {channel:?} on its `channels`; \
                     add the channel ref to [agents.{alias}.channels] before sending",
                    alias = self.sender_alias,
                )),
            });
        }

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

            let cfg = (*self.config).clone();
            let sender = self.sender_alias.clone();
            let recipient_alias = canonical.clone();
            let body = message.clone();
            // Build the recipient's cost-tracking context from `&cfg` before
            // `cfg` moves into `process_message` below — a detached
            // `zeroclaw_spawn::spawn!` task does not inherit the caller's
            // task-locals, so the recipient's turn would otherwise run with
            // no cost context and its spend would go unrecorded.
            let cost_ctx = tool_loop_cost_tracking_context_for_agent(&cfg, &recipient_alias);
            let turn_usage = cost_ctx
                .as_ref()
                .map(|_| Arc::new(Mutex::new(TurnUsage::default())));
            zeroclaw_spawn::spawn!(async move {
                let turn = crate::agent::loop_::process_message(
                    cfg,
                    &recipient_alias,
                    &body,
                    None,
                    zeroclaw_api::ingress::TurnOrigin::AgentDirect,
                );
                if let Err(e) = deliver_peer_turn_with_cost_scope(cost_ctx, turn_usage, turn).await
                {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"sender": sender, "recipient": recipient_alias, "error": format!("{}", e)})), "peer-message in-process delivery failed");
                }
            });

            return Ok(ToolResult {
                success: true,
                output: format!(
                    "accepted for in-process delivery to peer agent {canonical:?} (recipient runs detached; observe its agent loop for the actual outcome)"
                ).into(),
                error: None,
            });
        }

        match deliver_announcement(&self.config, &channel, &target, None, &message).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("delivered to external peer {target:?} on {channel}").into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("delivery failed: {e:#}")),
            }),
        }
    }
}

/// Run `inner` under the recipient's cost-tracking task-locals so its usage
/// is recorded with recipient attribution and its provider calls participate
/// in the process-wide budget, mirroring the scope the gateway chat path
/// installs around `process_message` in
/// `zeroclaw-gateway/src/lib.rs`. Split out from `execute` so the
/// scope-install itself can be exercised directly in tests, independent of
/// the detached-spawn plumbing around it.
async fn deliver_peer_turn_with_cost_scope<F, T>(
    cost_ctx: Option<ToolLoopCostTrackingContext>,
    turn_usage: Option<Arc<Mutex<TurnUsage>>>,
    inner: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    TOOL_LOOP_TURN_USAGE
        .scope(
            turn_usage,
            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(cost_ctx, inner),
        )
        .await
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
                channel: "telegram".into(),
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
                channel: "telegram".into(),
                agents: vec![AgentAlias::new("aa"), AgentAlias::new("beta")],
                external_peers: vec![PeerUsername::new("@Operator")],
                ..PeerGroupConfig::default()
            },
        );
        config.peer_groups.insert(
            "dev_ops".to_string(),
            PeerGroupConfig {
                channel: "telegram.dev".into(),
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
                channel: "telegram".into(),
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
                    channel: "telegram".into(),
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
                        channel: channel_type.clone().into(),
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

    #[tokio::test]
    async fn peer_turn_cost_scope_records_recipient_usage_and_enforces_shared_budget() {
        use crate::agent::cost::record_tool_loop_cost_usage;
        use crate::agent::turn::provider_call::enforce_tool_loop_budget;
        use crate::cost::CostTracker;
        use std::collections::HashMap;
        use zeroclaw_config::schema::CostConfig;
        use zeroclaw_providers::traits::TokenUsage;

        let workspace = tempfile::TempDir::new().expect("temp dir");
        let tracker = Arc::new(
            CostTracker::new(
                CostConfig {
                    enabled: true,
                    track_per_agent: true,
                    ..CostConfig::default()
                },
                workspace.path(),
            )
            .expect("cost tracker should initialize"),
        );
        let pricing = Arc::new(HashMap::from([(
            "mock-provider".to_string(),
            HashMap::from([
                ("test-model.input".to_string(), 3.0),
                ("test-model.output".to_string(), 15.0),
            ]),
        )]));
        let usage = TokenUsage {
            input_tokens: Some(1_000),
            cached_input_tokens: None,
            output_tokens: Some(200),
        };

        // Vacuity control: outside of any scope (as the detached spawn body
        // ran before this fix), the real consumer must be a no-op — this
        // proves the scope-install below is load-bearing, not incidental.
        let unscoped = record_tool_loop_cost_usage("mock-provider", "test-model", &usage);
        assert!(
            unscoped.is_none(),
            "record_tool_loop_cost_usage must be a no-op with no cost context scoped"
        );
        assert_eq!(
            tracker.get_summary().expect("cost summary").request_count,
            0,
            "vacuity control must not have persisted a record"
        );

        // Same two task-locals `deliver_peer_turn_with_cost_scope` installs,
        // aliased to the RECIPIENT (not the sender) — exercising the real
        // production consumer (`record_tool_loop_cost_usage`) and the real
        // budget gate (`enforce_tool_loop_budget`) through it.
        let cost_ctx = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), pricing)
            .with_agent_alias("recipient");
        let turn_usage = Arc::new(Mutex::new(TurnUsage::default()));

        let recorded = deliver_peer_turn_with_cost_scope(
            Some(cost_ctx),
            Some(Arc::clone(&turn_usage)),
            async { record_tool_loop_cost_usage("mock-provider", "test-model", &usage) },
        )
        .await;

        let (total_tokens, cost_usd) =
            recorded.expect("scoped consumer must record the recipient's usage");
        let expected_cost = (1_000.0 * 3.0 / 1_000_000.0) + (200.0 * 15.0 / 1_000_000.0);
        assert_eq!(total_tokens, 1_200);
        assert!((cost_usd - expected_cost).abs() < 1e-12);

        let summary = tracker.get_summary().expect("cost summary");
        assert_eq!(
            summary.request_count, 1,
            "the recipient's turn usage must be persisted to the shared tracker"
        );

        let recipient_summary = tracker
            .get_summary_for_agent("recipient")
            .expect("recipient summary");
        assert_eq!(
            recipient_summary.request_count, 1,
            "spend must be attributed to the recipient alias, not left unattributed"
        );
        assert!((recipient_summary.session_cost_usd - expected_cost).abs() < 1e-9);

        let sender_summary = tracker
            .get_summary_for_agent("sender")
            .expect("sender summary");
        assert_eq!(
            sender_summary.request_count, 0,
            "spend must not bleed onto an unrelated (sender) alias"
        );

        let snapshot = *turn_usage.lock();
        assert_eq!(snapshot.input_tokens, 1_000);
        assert_eq!(snapshot.output_tokens, 200);

        // Budget enforcement is process-wide, unlike the per-agent ledger
        // attribution above. Record spend as an unrelated sender, then prove
        // a recipient call scoped through this helper observes the shared
        // daily ceiling instead of bypassing it.
        let capped_workspace = tempfile::TempDir::new().expect("second temp dir");
        let capped_tracker = Arc::new(
            CostTracker::new(
                CostConfig {
                    enabled: true,
                    track_per_agent: true,
                    daily_limit_usd: expected_cost / 2.0,
                    ..CostConfig::default()
                },
                capped_workspace.path(),
            )
            .expect("capped cost tracker should initialize"),
        );
        let capped_pricing = Arc::new(HashMap::from([(
            "mock-provider".to_string(),
            HashMap::from([
                ("test-model.input".to_string(), 3.0),
                ("test-model.output".to_string(), 15.0),
            ]),
        )]));
        let capped_ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&capped_tracker),
            Arc::clone(&capped_pricing),
        )
        .with_agent_alias("recipient");
        let capped_turn_usage = Arc::new(Mutex::new(TurnUsage::default()));

        let sender_ctx = ToolLoopCostTrackingContext::new(
            Arc::clone(&capped_tracker),
            Arc::clone(&capped_pricing),
        )
        .with_agent_alias("sender");
        let sender_usage = deliver_peer_turn_with_cost_scope(
            Some(sender_ctx),
            Some(Arc::new(Mutex::new(TurnUsage::default()))),
            async { record_tool_loop_cost_usage("mock-provider", "test-model", &usage) },
        )
        .await;
        assert!(
            sender_usage.is_some(),
            "setup spend must be recorded against the unrelated sender"
        );

        let budget_result =
            deliver_peer_turn_with_cost_scope(Some(capped_ctx), Some(capped_turn_usage), async {
                enforce_tool_loop_budget()
            })
            .await;

        assert!(
            budget_result.is_err(),
            "a recipient turn must observe the process-wide daily budget even \
             when unrelated sender spend exhausted it"
        );
    }

    /// The test above exercises `deliver_peer_turn_with_cost_scope` directly
    /// with isolated trackers. This test drives the REAL tool boundary:
    /// `execute()` -> canonical-recipient resolution -> cost-context
    /// builder -> detached-spawn -> `process_message` -> a real (mocked at
    /// the HTTP layer) provider dispatch -> `record_tool_loop_cost_usage`.
    /// A local Ollama-shaped HTTP server stands in for the network so the
    /// recipient's turn completes deterministically without a real model
    /// provider dependency.
    #[tokio::test]
    async fn peer_turn_cost_scope_through_execute_boundary_attributes_recipient_and_shares_budget()
    {
        use crate::agent::turn::provider_call::enforce_tool_loop_budget;
        use crate::cost::CostTracker;
        use axum::{Json, Router, extract::State, routing::post};
        use std::collections::HashMap;
        use zeroclaw_config::schema::{
            CostConfig, ModelProviderConfig, OllamaModelProviderConfig, RiskProfileConfig,
        };

        // `[providers.models.ollama.*]` dispatches through the shared
        // OpenAI-compatible client (`factory::build_ollama_compat_provider`),
        // not the native `/api/chat` wire format, so the fake stands in at
        // the OpenAI-compatible `/v1/chat/completions` boundary.
        type RequestCount = Arc<Mutex<u32>>;
        async fn capture_chat(
            State(count): State<RequestCount>,
            Json(_body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            *count.lock() += 1;
            Json(serde_json::json!({
                "choices": [{"message": {"content": "peer turn complete"}}],
                "usage": {"prompt_tokens": 1_000, "completion_tokens": 200}
            }))
        }

        let request_count: RequestCount = Arc::new(Mutex::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock provider listener should bind");
        let mock_addr = listener.local_addr().expect("mock provider addr");
        let app = Router::new()
            .route("/v1/chat/completions", post(capture_chat))
            .with_state(request_count.clone());
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app)
                .await
                .expect("mock provider server should run");
        });

        // Isolated data_dir: `Config::default()` points at the real
        // `~/.zeroclaw/data`, and `execute()` resolves cost tracking off the
        // process-global `CostTracker` singleton, so an isolated, unique
        // ledger path is required to avoid touching real user state and to
        // give this test a clean-slate budget.
        let workspace = tempfile::TempDir::new().expect("temp data dir");
        let mut config = Config {
            data_dir: workspace.path().to_path_buf(),
            config_path: workspace.path().join("config.toml"),
            ..Config::default()
        };
        config.cost = CostConfig {
            enabled: true,
            track_per_agent: true,
            ..CostConfig::default()
        };
        config.providers.models.ollama.insert(
            "default".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("mock-model".to_string()),
                    uri: Some(format!("http://{mock_addr}")),
                    timeout_secs: Some(5),
                    pricing: HashMap::from([
                        ("mock-model.input".to_string(), 3.0),
                        ("mock-model.output".to_string(), 15.0),
                    ]),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        config.agents.insert(
            "sender".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                ..AliasedAgentConfig::default()
            },
        );
        config.agents.insert(
            "recipient".to_string(),
            AliasedAgentConfig {
                channels: vec!["telegram.prod".into()],
                model_provider: "ollama.default".into(),
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config.peer_groups.insert(
            "ops".to_string(),
            PeerGroupConfig {
                channel: "telegram".into(),
                agents: vec![AgentAlias::new("sender"), AgentAlias::new("recipient")],
                ..PeerGroupConfig::default()
            },
        );
        config
            .risk_profiles
            .insert("default".to_string(), RiskProfileConfig::default());

        let tool = SendMessageToPeerTool::new(Arc::new(config.clone()), "sender");
        let result = tool
            .execute(json!({
                "channel": "telegram.prod",
                "target": "recipient",
                "message": "produce a one-line status update for the on-call rotation"
            }))
            .await
            .expect("execute should accept the peer send for detached delivery");
        assert!(
            result.success,
            "execute should accept the send for in-process delivery: {result:?}"
        );

        // `execute()` already resolved the recipient's cost context off the
        // process-global tracker (synchronously, before the detached spawn).
        // Fetch the SAME Arc: same data_dir means same ledger path, so
        // `CostTracker::resolve_global` reuses it rather than building a
        // second one, to observe what the detached turn records.
        let tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir)
            .expect("execute() must have resolved a process-global cost tracker for an enabled cost config");

        // The recipient's turn runs inside a detached `zeroclaw_spawn::spawn!`
        // task that `execute()` does not join. Poll for its completion with a
        // bounded, sleep-free yield loop (the same pattern already used for
        // detached-task synchronization in `rpc/dispatch.rs`), never a fixed
        // sleep.
        let mut recipient_summary = None;
        for _ in 0..20_000 {
            let summary = tracker
                .get_summary_for_agent("recipient")
                .expect("recipient cost summary should be queryable");
            if summary.request_count >= 1 {
                recipient_summary = Some(summary);
                break;
            }
            tokio::task::yield_now().await;
        }
        let recipient_summary = recipient_summary.expect(
            "the detached recipient turn must complete and record usage against the \
             recipient alias; if this fires, execute() is no longer threading the \
             cost-tracking scope into the spawned process_message future",
        );

        // (1) Usage lands on the recipient alias in the per-agent ledger,
        // through the REAL tool boundary end to end - not just the helper.
        assert_eq!(recipient_summary.request_count, 1);
        assert_eq!(recipient_summary.total_tokens, 1_200);
        let expected_cost = (1_000.0 * 3.0 / 1_000_000.0) + (200.0 * 15.0 / 1_000_000.0);
        assert!(
            (recipient_summary.session_cost_usd - expected_cost).abs() < 1e-9,
            "unexpected recipient cost: {} vs expected {expected_cost}",
            recipient_summary.session_cost_usd
        );

        let sender_summary = tracker
            .get_summary_for_agent("sender")
            .expect("sender cost summary should be queryable");
        assert_eq!(
            sender_summary.request_count, 0,
            "spend must not bleed onto the sending agent's alias"
        );

        assert_eq!(
            *request_count.lock(),
            1,
            "the recipient turn must have made exactly one real provider call \
             through the mock endpoint"
        );

        // (2) The recipient turn's provider call participates in the
        // shared, process-wide budget: lower the daily cap below what the
        // recipient just spent, then prove an UNRELATED agent's subsequent
        // budget check - through the same tracker Arc `execute()` resolved
        // - now observes it as exceeded. This proves the recipient's real,
        // execute()-triggered spend landed in the one ledger every agent's
        // budget check consults, not a silo the recipient alias owns
        // exclusively.
        tracker.update_config(CostConfig {
            enabled: true,
            track_per_agent: true,
            daily_limit_usd: recipient_summary.session_cost_usd / 2.0,
            ..CostConfig::default()
        });
        let unrelated_ctx =
            ToolLoopCostTrackingContext::new(Arc::clone(&tracker), Arc::new(HashMap::new()))
                .with_agent_alias("unrelated-agent");
        let budget_result = TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(unrelated_ctx), async { enforce_tool_loop_budget() })
            .await;
        assert!(
            budget_result.is_err(),
            "the recipient turn's real spend must count against the shared \
             process-wide daily budget, blocking an unrelated agent's next call"
        );

        server.abort();
    }
}
