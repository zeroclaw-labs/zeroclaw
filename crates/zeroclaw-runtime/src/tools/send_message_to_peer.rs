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
//!   `tokio::spawn`, so the sender's `ToolResult.success = true`
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
}

impl SendMessageToPeerTool {
    pub fn new(config: Arc<Config>, sender_alias: impl Into<String>) -> Self {
        Self {
            config,
            sender_alias: sender_alias.into(),
        }
    }
}

#[async_trait]
impl Tool for SendMessageToPeerTool {
    fn name(&self) -> &str {
        "send_message_to_peer"
    }

    fn description(&self) -> &str {
        "Send a message to a peer agent or external peer (human, external bot) \
         on a shared channel. The target must be a member of a peer group both \
         this agent and the target agree on (or an external peer listed on the \
         shared group's `external_peers`). Cross-agent sends to non-peers are \
         rejected at the tool boundary; the channel send only happens after \
         the peer-set check passes."
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
            .ok_or_else(|| anyhow::anyhow!("Missing or empty 'channel' parameter"))?
            .to_string();
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing or empty 'target' parameter"))?
            .to_string();
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing or empty 'message' parameter"))?
            .to_string();

        // Peer-groups bind to channel TYPE, not <type>.<alias>.
        let channel_type = channel
            .split_once('.')
            .map(|(t, _)| t)
            .unwrap_or(channel.as_str());
        let resolved = resolve_peer_set(&self.config, &self.sender_alias);

        if !resolved.is_known_peer(channel_type, &target) {
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
            tokio::spawn(async move {
                if let Err(e) =
                    crate::agent::loop_::process_message(cfg, &recipient_alias, &body, None).await
                {
                    tracing::warn!(
                        sender = %sender,
                        recipient = %recipient_alias,
                        error = %e,
                        "peer-message in-process delivery failed",
                    );
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

        match deliver_announcement(&self.config, &channel, &target, &message).await {
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
