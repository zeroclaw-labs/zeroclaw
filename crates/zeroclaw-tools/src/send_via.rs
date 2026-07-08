//! Per-turn output routing tool (`send_via`).
//!
//! Two modes, determined by whether `body` is present:
//!
//! **Routing instruction** (no `body`): sets where and how the agent's current
//! reply is delivered this turn. Does not send a message itself. The orchestrator
//! reads [`TurnRoutingHandle`] after the tool-call loop and applies the entries.
//!   - `send_via(modality: "text")` — same channel, force text even on voice-only peer
//!   - `send_via(target: "discord.main")` — redirect reply to a different channel
//!   - `send_via(target: "discord.main", modality: "voice")` — redirect + force modality
//!
//! **Immediate send** (with `body`): delivers a separate message independently of
//! the agent's main reply. The main reply still goes to the originating channel.
//! `target` is required when `body` is present.
//!   - `send_via(target: "email.default", body: "...details...")` — fanout with own content
//!
//! **Authorization**: targets are constrained to channels covered by peer groups
//! that include the active agent.
//!
//! **Modality resolution priority** (highest to lowest):
//!   1. Explicit `modality` parameter
//!   2. Peer group `output_modality`
//!   3. Text (channel default)

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::multi_agent::OutputModality;
use zeroclaw_config::multi_agent::PeerGroupConfig;
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

/// Per-tool channel-map handle — matches `zeroclaw_runtime::tools::PerToolChannelHandle`.
pub type PerToolChannelHandle = Arc<parking_lot::RwLock<HashMap<String, Arc<dyn Channel>>>>;

/// Resolves the peer groups that include the active agent, read live from config
/// at call time so a config reload (membership / `external_peers` / channel alias /
/// `output_modality`) takes effect without rebuilding the tool registry.
pub type AgentPeerGroupResolver = Arc<dyn Fn() -> HashMap<String, PeerGroupConfig> + Send + Sync>;

tokio::task_local! {
    /// The current turn's routing handle, scoped by the orchestrator around its
    /// `run_tool_call_loop` call. `send_via` writes its routing entry here so each
    /// turn sees only its own routes — even when several turns for the same agent
    /// run concurrently and share one `SendViaTool`. `None` outside a scoped turn
    /// (one-shot / non-channel entry paths), where routing instructions are ignored.
    pub static TURN_ROUTING: Option<TurnRoutingHandle>;
}

/// Return type of [`SendViaTool::resolve_target`]: resolved channel key, channel arc,
/// modality, and optional explicit recipient.
type ResolvedTarget = Result<(String, Arc<dyn Channel>, OutputModality, Option<String>), String>;

/// A single per-turn routing instruction written by `send_via` in no-body mode.
/// The orchestrator reads this after `run_tool_call_loop` returns.
#[derive(Debug, Clone)]
pub struct TurnRoutingEntry {
    /// Target channel key (e.g. `"telegram.default"`). `None` = originating channel.
    pub channel: Option<String>,
    /// Modality to apply for delivery.
    pub modality: OutputModality,
    /// Explicit recipient override. `None` = inherit from `msg.reply_target`.
    pub recipient: Option<String>,
}

/// Handle to one turn's routing state. The orchestrator creates a fresh handle
/// before each `run_tool_call_loop` call, scopes it into [`TURN_ROUTING`] for the
/// duration of the loop, and reads it back after the loop completes.
pub type TurnRoutingHandle = Arc<Mutex<Vec<TurnRoutingEntry>>>;

/// Agent-callable tool for per-turn output routing and channel fanout.
pub struct SendViaTool {
    security: Arc<SecurityPolicy>,
    channel_map: PerToolChannelHandle,
    /// Resolves the active agent's peer groups live from config at call time.
    agent_peer_groups: AgentPeerGroupResolver,
}

impl SendViaTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        channel_map: PerToolChannelHandle,
        agent_peer_groups: AgentPeerGroupResolver,
    ) -> Self {
        Self {
            security,
            channel_map,
            agent_peer_groups,
        }
    }

    /// Returns `true` if the peer group's `channel` field covers `channel_key`.
    /// A bare type (`"telegram"`) covers every alias (`"telegram.default"`).
    fn peer_group_covers_channel(pg_channel: &str, channel_key: &str) -> bool {
        if pg_channel == channel_key {
            return true;
        }
        if !pg_channel.contains('.') {
            channel_key.split('.').next() == Some(pg_channel)
        } else {
            false
        }
    }

    /// Resolve `target` to `(channel_key, channel, peer_group_output_modality)`.
    ///
    /// `target` may be a peer group name, a composite channel key, or a bare type.
    /// Returns `Err(reason)` when the target is rejected or not found.
    fn resolve_target(&self, target: &str) -> ResolvedTarget {
        let channel_map = self.channel_map.read();
        // Resolved live from config each call so reloads take effect immediately.
        let agent_peer_groups = (self.agent_peer_groups)();

        // --- 1. Try as peer group name ---
        if let Some(pg) = agent_peer_groups.get(target) {
            let pg_channel = &pg.channel;

            // Resolve the group's channel deterministically. An exact composite
            // key (`telegram.main`) is used as-is; a bare type (`telegram`)
            // resolves to `<type>.default`, matching the bare channel-target
            // path below. Scanning for the first covering alias in the channel
            // map would pick an arbitrary `telegram.*` account when several are
            // registered, delivering through the wrong sender while still
            // reporting success. Fail closed when no exact/`.default` match
            // exists so the config must name the alias it means.
            let resolved = if pg_channel.contains('.') {
                channel_map
                    .get(pg_channel.as_str())
                    .map(|ch| (pg_channel.as_str().to_string(), Arc::clone(ch)))
            } else {
                let default_key = format!("{pg_channel}.default");
                channel_map
                    .get(&default_key)
                    .map(|ch| (default_key, Arc::clone(ch)))
            };

            // Recipient: first external peer configured in this group, if any.
            let recipient = pg.external_peers.first().map(|p| p.as_str().to_string());

            return match resolved {
                Some((key, ch)) => Ok((key, ch, pg.output_modality, recipient)),
                None => Err(format!(
                    "Peer group '{target}' specifies channel '{pg_channel}' \
                     but no matching channel is registered (a bare type resolves \
                     to '<type>.default'; name an exact alias to target another)"
                )),
            };
        }

        // --- 2. Try as exact or bare channel key ---
        let (channel_key, channel) = if let Some(ch) = channel_map.get(target) {
            (target.to_string(), Arc::clone(ch))
        } else if !target.contains('.') {
            let default_key = format!("{target}.default");
            match channel_map.get(&default_key) {
                Some(ch) => (default_key, Arc::clone(ch)),
                None => {
                    return Err(format!(
                        "Channel '{target}' not found. Available: {:?}",
                        channel_map.keys().collect::<Vec<_>>()
                    ));
                }
            }
        } else {
            return Err(format!(
                "Channel '{target}' not found. Available: {:?}",
                channel_map.keys().collect::<Vec<_>>()
            ));
        };

        // --- 3. Authorize: must be within an agent peer group ---
        // The matched group decides the recipient (first external peer) and
        // inherited modality, so the match must be unambiguous: if more than
        // one peer group covers this channel, picking one out of a HashMap
        // would send to an arbitrary recipient. Fail closed and require the
        // caller to name the peer group instead.
        let mut matching: Vec<(&String, &PeerGroupConfig)> = agent_peer_groups
            .iter()
            .filter(|(_, pg)| Self::peer_group_covers_channel(&pg.channel, &channel_key))
            .collect();

        match matching.len() {
            0 => Err(format!(
                "Channel '{channel_key}' is not within any peer group this agent belongs to"
            )),
            1 => {
                let (_, pg) = matching.remove(0);
                let recipient = pg.external_peers.first().map(|p| p.as_str().to_string());
                Ok((channel_key, channel, pg.output_modality, recipient))
            }
            _ => {
                let mut names: Vec<&str> = matching.iter().map(|(name, _)| name.as_str()).collect();
                names.sort_unstable();
                Err(format!(
                    "Channel '{channel_key}' is covered by multiple peer groups ({}); \
                     use a peer group name as the target to disambiguate the recipient",
                    names.join(", ")
                ))
            }
        }
    }
}

#[async_trait]
impl Tool for SendViaTool {
    fn name(&self) -> &str {
        "send_via"
    }

    fn description(&self) -> &str {
        "Control where and how this turn's reply is delivered, or send an extra message \
         to another channel.\n\
         \n\
         WHEN TO USE: call this tool at the start of your response whenever the user requests \
         a specific reply format or destination — e.g. \"reply by text\", \"send as voice\", \
         \"text only\", \"send to my email\", \"redirect to Discord\". Do not wait for the user \
         to name the tool; infer intent from natural language just as you would use a weather \
         tool when asked for the weather.\n\
         \n\
         Without `body` (routing instruction — affects this turn's main reply):\n\
         - `send_via(modality: \"text\")` — reply by text even on a voice-only peer\n\
         - `send_via(modality: \"voice\")` — reply by voice even on a text-only peer\n\
         - `send_via(target: \"discord.main\")` — redirect reply to another channel\n\
         - `send_via(target: \"discord.main\", modality: \"voice\")` — redirect + force modality\n\
         At least one of `target` or `modality` is required when `body` is absent.\n\
         \n\
         With `body` (immediate fanout — main reply still goes to originating channel):\n\
         - `send_via(target: \"email.default\", body: \"...\")` — send separate content elsewhere\n\
         `target` is required when `body` is present.\n\
         \n\
         `target` must be a channel alias (e.g. `telegram.default`) or a peer group name \
         the active agent belongs to. `modality` defaults to the peer group's output_modality."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Channel alias (e.g. telegram.default) or peer group name. \
                                    Required when body is present. Optional otherwise (omit = same channel)."
                },
                "modality": {
                    "type": "string",
                    "enum": ["text", "voice"],
                    "description": "Delivery modality. Omit to inherit from the peer group's output_modality."
                },
                "body": {
                    "type": "string",
                    "description": "Message content for an immediate fanout send. \
                                    Omit to set a routing instruction for the current reply instead."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(e) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "send_via")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Action blocked: {e}")),
            });
        }

        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string);

        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string);

        let explicit_modality = args.get("modality").and_then(|v| v.as_str()).map(|s| {
            if s == "voice" {
                OutputModality::Voice
            } else {
                OutputModality::Text
            }
        });

        // ── Immediate send mode (body present) ───────────────────────────────
        if let Some(body) = body {
            let target_key = match target {
                Some(ref t) => t.clone(),
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: json!({
                            "status": "rejected",
                            "reason": "`target` is required when `body` is present"
                        })
                        .to_string(),
                        error: None,
                    });
                }
            };

            let (channel_key, channel, pg_modality, recipient) =
                match self.resolve_target(&target_key) {
                    Ok(v) => v,
                    Err(reason) => {
                        return Ok(ToolResult {
                            success: false,
                            output: json!({
                                "target": target_key,
                                "status": "rejected",
                                "reason": reason
                            })
                            .to_string(),
                            error: None,
                        });
                    }
                };

            // Fail closed: immediate-send requires a concrete recipient from the
            // peer group's external_peers. Without one we cannot address the message.
            let recipient = match recipient {
                Some(r) => r,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: json!({
                            "target": target_key,
                            "status": "rejected",
                            "reason": "target peer group has no external_peers configured; \
                                       cannot determine send recipient"
                        })
                        .to_string(),
                        error: None,
                    });
                }
            };

            let modality = explicit_modality.unwrap_or(pg_modality);
            let message = match modality {
                OutputModality::Voice => SendMessage::new(&body, &recipient).force_voice(),
                OutputModality::Text => SendMessage::new(&body, &recipient).suppress_voice(),
                OutputModality::Mirror => SendMessage::new(&body, &recipient),
            };

            return match channel.send(&message).await {
                Ok(()) => Ok(ToolResult {
                    success: true,
                    output: json!({
                        "target": channel_key,
                        "mode": "immediate",
                        "resolved_modality": modality_str(modality),
                        "status": "ok"
                    })
                    .to_string(),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: json!({
                        "target": channel_key,
                        "status": "failed",
                        "reason": e.to_string()
                    })
                    .to_string(),
                    error: None,
                }),
            };
        }

        // ── Routing instruction mode (no body) ───────────────────────────────
        if target.is_none() && explicit_modality.is_none() {
            return Ok(ToolResult {
                success: false,
                output: json!({
                    "status": "rejected",
                    "reason": "at least one of `target` or `modality` is required when `body` is absent"
                })
                .to_string(),
                error: None,
            });
        }

        // Resolve channel + peer-group modality if a target was given.
        let (resolved_channel, resolved_modality, resolved_recipient) = if let Some(ref t) = target
        {
            match self.resolve_target(t) {
                Ok((key, _ch, pg_modality, recipient)) => {
                    // Fail closed: a cross-channel redirect must have a concrete recipient
                    // from external_peers so the orchestrator can address the target channel.
                    // (Modality-only routing with no target uses msg.reply_target instead.)
                    if recipient.is_none() {
                        return Ok(ToolResult {
                            success: false,
                            output: json!({
                                "target": t,
                                "status": "rejected",
                                "reason": "target peer group has no external_peers configured; \
                                           cannot determine routing recipient"
                            })
                            .to_string(),
                            error: None,
                        });
                    }
                    (
                        Some(key),
                        explicit_modality.unwrap_or(pg_modality),
                        recipient,
                    )
                }
                Err(reason) => {
                    return Ok(ToolResult {
                        success: false,
                        output: json!({
                            "target": t,
                            "status": "rejected",
                            "reason": reason
                        })
                        .to_string(),
                        error: None,
                    });
                }
            }
        } else {
            // No target → same channel, use explicit modality (already validated non-None above)
            (None, explicit_modality.unwrap(), None)
        };

        let entry = TurnRoutingEntry {
            channel: resolved_channel.clone(),
            modality: resolved_modality,
            recipient: resolved_recipient,
        };

        // Write to the current turn's routing handle. Each turn scopes its own
        // handle into TURN_ROUTING, so concurrent turns sharing this one tool
        // never see each other's routes. Outside a scoped turn the instruction
        // is ignored (no orchestrator reads it back).
        let queued = TURN_ROUTING
            .try_with(|handle| {
                if let Some(handle) = handle {
                    handle.lock().unwrap_or_else(|e| e.into_inner()).push(entry);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);

        if !queued {
            return Ok(ToolResult {
                success: false,
                output: json!({
                    "target": resolved_channel.as_deref().unwrap_or("<originating>"),
                    "status": "ignored",
                    "reason": "routing is only available while handling a channel turn"
                })
                .to_string(),
                error: None,
            });
        }

        Ok(ToolResult {
            success: true,
            output: json!({
                "target": resolved_channel.as_deref().unwrap_or("<originating>"),
                "mode": "routing",
                "resolved_modality": modality_str(resolved_modality),
                "status": "queued"
            })
            .to_string(),
            error: None,
        })
    }
}

fn modality_str(m: OutputModality) -> &'static str {
    match m {
        OutputModality::Voice => "voice",
        OutputModality::Text => "text",
        OutputModality::Mirror => "mirror",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
    use zeroclaw_api::channel::ChannelMessage;
    use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};

    struct StubChannel {
        name: String,
        sent: Arc<parking_lot::RwLock<Vec<SendMessage>>>,
    }

    impl StubChannel {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                sent: Arc::new(parking_lot::RwLock::new(Vec::new())),
            }
        }
    }

    impl Attributable for StubChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Webhook)
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl Channel for StubChannel {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.write().push(message.clone());
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Test wrapper that scopes a per-turn `TURN_ROUTING` handle around each
    /// `execute`, mirroring what the orchestrator does around its tool loop.
    struct TestTool {
        inner: SendViaTool,
        routing: TurnRoutingHandle,
    }

    impl TestTool {
        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            TURN_ROUTING
                .scope(Some(Arc::clone(&self.routing)), self.inner.execute(args))
                .await
        }
    }

    fn make_tool(
        channels: Vec<(&str, Arc<dyn Channel>)>,
        peer_groups: HashMap<String, PeerGroupConfig>,
    ) -> (TestTool, TurnRoutingHandle) {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        for (name, ch) in channels {
            map.write().insert(name.to_string(), ch);
        }
        let routing: TurnRoutingHandle = Arc::new(Mutex::new(Vec::new()));
        let groups = Arc::new(peer_groups);
        let inner = SendViaTool::new(
            Arc::new(SecurityPolicy::default()),
            map,
            Arc::new(move || (*groups).clone()),
        );
        let tool = TestTool {
            inner,
            routing: Arc::clone(&routing),
        };
        (tool, routing)
    }

    fn pg(channel: &str, agents: &[&str]) -> PeerGroupConfig {
        PeerGroupConfig {
            channel: channel.into(),
            agents: agents.iter().map(|a| AgentAlias::new(*a)).collect(),
            output_modality: OutputModality::Text,
            ..PeerGroupConfig::default()
        }
    }

    fn pg_with_peers(channel: &str, agents: &[&str], peers: &[&str]) -> PeerGroupConfig {
        PeerGroupConfig {
            channel: channel.into(),
            agents: agents.iter().map(|a| AgentAlias::new(*a)).collect(),
            external_peers: peers.iter().map(|p| PeerUsername::new(*p)).collect(),
            ..PeerGroupConfig::default()
        }
    }

    fn pg_voice_with_peers(channel: &str, agents: &[&str], peers: &[&str]) -> PeerGroupConfig {
        PeerGroupConfig {
            channel: channel.into(),
            agents: agents.iter().map(|a| AgentAlias::new(*a)).collect(),
            output_modality: OutputModality::Voice,
            external_peers: peers.iter().map(|p| PeerUsername::new(*p)).collect(),
            ..PeerGroupConfig::default()
        }
    }

    // ── Routing instruction mode ──────────────────────────────────────────────

    #[tokio::test]
    async fn routing_modality_text_pushes_entry() {
        let mut groups = HashMap::new();
        groups.insert("g1".to_string(), pg("telegram", &["elisa"]));
        let (tool, routing) = make_tool(vec![], groups);

        let result = tool.execute(json!({ "modality": "text" })).await.unwrap();

        assert!(result.success, "{:?}", result.error);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "queued");
        assert_eq!(out["mode"], "routing");
        assert_eq!(out["resolved_modality"], "text");

        let entries = routing.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].channel.is_none());
        assert!(matches!(entries[0].modality, OutputModality::Text));
    }

    #[tokio::test]
    async fn routing_modality_voice_pushes_entry() {
        let mut groups = HashMap::new();
        groups.insert("g1".to_string(), pg("telegram", &["elisa"]));
        let (tool, routing) = make_tool(vec![], groups);

        let result = tool.execute(json!({ "modality": "voice" })).await.unwrap();

        assert!(result.success);
        let entries = routing.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].modality, OutputModality::Voice));
    }

    #[tokio::test]
    async fn routing_with_target_pushes_channel_entry() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let mut groups = HashMap::new();
        // Target group must have external_peers so fail-close doesn't trigger.
        groups.insert(
            "g1".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@recipient"]),
        );
        let (tool, routing) = make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({ "target": "telegram.default", "modality": "voice" }))
            .await
            .unwrap();

        assert!(result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["mode"], "routing");
        let entries = routing.lock().unwrap();
        assert_eq!(entries[0].channel.as_deref(), Some("telegram.default"));
        assert!(matches!(entries[0].modality, OutputModality::Voice));
        assert_eq!(
            entries[0].recipient.as_deref(),
            Some("@recipient"),
            "routing entry must carry the peer group's resolved recipient"
        );
    }

    #[tokio::test]
    async fn routing_with_target_rejected_when_no_external_peers() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let mut groups = HashMap::new();
        // No external_peers — cross-channel routing must fail closed.
        groups.insert("g1".to_string(), pg("telegram", &["elisa"]));
        let (tool, routing) = make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({ "target": "telegram.default", "modality": "voice" }))
            .await
            .unwrap();

        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("no external_peers")
        );
        assert!(routing.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn routing_no_target_no_modality_rejected() {
        let (tool, routing) = make_tool(vec![], HashMap::new());

        let result = tool.execute(json!({})).await.unwrap();

        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        assert!(routing.lock().unwrap().is_empty());
    }

    // ── Immediate send mode ───────────────────────────────────────────────────

    #[tokio::test]
    async fn immediate_send_with_body_and_target() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let sent = Arc::clone(&ch.sent);
        let mut groups = HashMap::new();
        groups.insert(
            "g1".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@amaury"]),
        );
        let (tool, routing) = make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({
                "target": "telegram.default",
                "body": "extra message"
            }))
            .await
            .unwrap();

        assert!(result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["mode"], "immediate");
        assert_eq!(out["status"], "ok");
        // message was actually sent, addressed to the peer group's recipient
        assert_eq!(sent.read().len(), 1);
        assert_eq!(
            sent.read()[0].recipient,
            "@amaury",
            "immediate send must deliver to the resolved external_peers recipient"
        );
        // routing state untouched
        assert!(routing.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn immediate_send_body_without_target_rejected() {
        let (tool, routing) = make_tool(vec![], HashMap::new());

        let result = tool.execute(json!({ "body": "hello" })).await.unwrap();

        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        assert!(routing.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn immediate_send_rejected_when_no_external_peers() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let mut groups = HashMap::new();
        // Peer group has no external_peers — immediate send cannot determine recipient.
        groups.insert("g1".to_string(), pg("telegram", &["elisa"]));
        let (tool, routing) = make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({ "target": "telegram.default", "body": "hi" }))
            .await
            .unwrap();

        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("no external_peers")
        );
        assert!(routing.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn immediate_send_voice_override() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let sent = Arc::clone(&ch.sent);
        let mut groups = HashMap::new();
        groups.insert(
            "g1".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@amaury"]),
        );
        let (tool, _routing) =
            make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({
                "target": "telegram.default",
                "modality": "voice",
                "body": "voice fanout"
            }))
            .await
            .unwrap();

        assert!(result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["resolved_modality"], "voice");
        assert!(sent.read()[0].force_voice);
    }

    #[tokio::test]
    async fn unauthorized_target_rejected() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let (tool, _routing) = make_tool(
            vec![("telegram.default", ch as Arc<dyn Channel>)],
            HashMap::new(),
        );

        let result = tool
            .execute(json!({ "target": "telegram.default", "body": "hi" }))
            .await
            .unwrap();

        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("not within any peer group")
        );
    }

    #[tokio::test]
    async fn peer_group_name_as_target_immediate() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let sent = Arc::clone(&ch.sent);
        let mut groups = HashMap::new();
        groups.insert(
            "amaury_tg".to_string(),
            pg_voice_with_peers("telegram", &["elisa"], &["@amaury"]),
        );
        let (tool, _routing) =
            make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({ "target": "amaury_tg", "body": "hi" }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(sent.read().len(), 1);
        assert_eq!(sent.read()[0].recipient, "@amaury");
        // inherits voice modality from peer group
        assert!(sent.read()[0].force_voice);
    }

    #[tokio::test]
    async fn bare_type_target_resolves_to_default_alias() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let sent = Arc::clone(&ch.sent);
        let mut groups = HashMap::new();
        groups.insert(
            "g1".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@amaury"]),
        );
        let (tool, _routing) =
            make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({ "target": "telegram", "body": "hi" }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(sent.read().len(), 1);
        assert_eq!(sent.read()[0].recipient, "@amaury");
    }

    #[tokio::test]
    async fn ambiguous_channel_alias_rejected() {
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let sent = Arc::clone(&ch.sent);
        let mut groups = HashMap::new();
        // Two peer groups cover the same channel with different recipients —
        // a channel-alias target cannot decide which recipient to use.
        groups.insert(
            "g1".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@amaury"]),
        );
        groups.insert(
            "g2".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@other"]),
        );
        let (tool, routing) = make_tool(vec![("telegram.default", ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({ "target": "telegram.default", "body": "hi" }))
            .await
            .unwrap();

        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("multiple peer groups")
        );
        // nothing was sent and no routing entry leaked
        assert!(sent.read().is_empty());
        assert!(routing.lock().unwrap().is_empty());

        // A peer-group name still resolves unambiguously.
        let result = tool
            .execute(json!({ "target": "g1", "body": "hi" }))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(sent.read()[0].recipient, "@amaury");
    }

    #[tokio::test]
    async fn peer_group_name_bare_channel_resolves_to_default_alias() {
        // Peer group names a bare channel type while TWO aliases of that type
        // are registered. Resolution must be deterministic (`telegram.default`),
        // never an arbitrary HashMap pick that could deliver via the wrong
        // channel account while still reporting success.
        let default_ch = Arc::new(StubChannel::new("telegram.default"));
        let main_ch = Arc::new(StubChannel::new("telegram.main"));
        let default_sent = Arc::clone(&default_ch.sent);
        let main_sent = Arc::clone(&main_ch.sent);
        let mut groups = HashMap::new();
        groups.insert(
            "amaury_tg".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@amaury"]),
        );
        let (tool, _routing) = make_tool(
            vec![
                ("telegram.default", default_ch as Arc<dyn Channel>),
                ("telegram.main", main_ch as Arc<dyn Channel>),
            ],
            groups,
        );

        let result = tool
            .execute(json!({ "target": "amaury_tg", "body": "hi" }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["target"], "telegram.default");
        assert_eq!(default_sent.read().len(), 1, "must deliver via .default");
        assert_eq!(
            main_sent.read().len(),
            0,
            "must not deliver via the non-default alias"
        );
    }

    #[tokio::test]
    async fn peer_group_name_bare_channel_without_default_rejected() {
        // Bare peer-group channel but no `.default` alias is registered: fail
        // closed rather than fall back to whichever alias happens to exist.
        let main_ch = Arc::new(StubChannel::new("telegram.main"));
        let main_sent = Arc::clone(&main_ch.sent);
        let mut groups = HashMap::new();
        groups.insert(
            "amaury_tg".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@amaury"]),
        );
        let (tool, routing) =
            make_tool(vec![("telegram.main", main_ch as Arc<dyn Channel>)], groups);

        let result = tool
            .execute(json!({ "target": "amaury_tg", "body": "hi" }))
            .await
            .unwrap();

        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("no matching channel is registered")
        );
        assert!(main_sent.read().is_empty());
        assert!(routing.lock().unwrap().is_empty());
    }

    // ── Concurrency isolation ─────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_turns_do_not_share_routing_state() {
        // Two turns for the same agent share one SendViaTool but each scopes its
        // own TURN_ROUTING handle. A route queued by one turn must never appear
        // in the other turn's handle — the privacy/correctness boundary from the
        // review. We share the tool across both tasks via Arc, exactly as the
        // per-agent registry does.
        let mut groups = HashMap::new();
        groups.insert(
            "tg".to_string(),
            pg_with_peers("telegram", &["elisa"], &["@a"]),
        );
        groups.insert(
            "mail".to_string(),
            pg_with_peers("email", &["elisa"], &["@b"]),
        );
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")) as Arc<dyn Channel>,
        );
        map.write().insert(
            "email.default".to_string(),
            Arc::new(StubChannel::new("email.default")) as Arc<dyn Channel>,
        );
        let groups = Arc::new(groups);
        let tool = Arc::new(SendViaTool::new(
            Arc::new(SecurityPolicy::default()),
            map,
            Arc::new(move || (*groups).clone()),
        ));

        // Each turn: scope a fresh handle, queue a route, hold the scope across an
        // await so the two turns genuinely overlap, then read back only our handle.
        async fn run_turn(tool: Arc<SendViaTool>, target: &str) -> Vec<TurnRoutingEntry> {
            let handle: TurnRoutingHandle = Arc::new(Mutex::new(Vec::new()));
            let target = target.to_string();
            TURN_ROUTING
                .scope(Some(Arc::clone(&handle)), async move {
                    let _ = tool.execute(json!({ "target": target })).await.unwrap();
                    tokio::task::yield_now().await;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                })
                .await;
            handle.lock().unwrap().clone()
        }

        let (tg, mail) = tokio::join!(
            run_turn(Arc::clone(&tool), "telegram.default"),
            run_turn(Arc::clone(&tool), "email.default"),
        );

        assert_eq!(
            tg.len(),
            1,
            "telegram turn should see exactly its own route"
        );
        assert_eq!(tg[0].channel.as_deref(), Some("telegram.default"));
        assert_eq!(tg[0].recipient.as_deref(), Some("@a"));

        assert_eq!(mail.len(), 1, "email turn should see exactly its own route");
        assert_eq!(mail[0].channel.as_deref(), Some("email.default"));
        assert_eq!(mail[0].recipient.as_deref(), Some("@b"));
    }

    // ── Live config authority ─────────────────────────────────────────────────

    #[tokio::test]
    async fn peer_group_authority_is_resolved_live_on_reload() {
        // The same long-lived SendViaTool must reflect a config reload (peer
        // group recipient + output_modality) without being rebuilt — it resolves
        // its authority from the live source each call rather than a snapshot.
        let ch = Arc::new(StubChannel::new("telegram.default"));
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write()
            .insert("telegram.default".to_string(), ch as Arc<dyn Channel>);

        // Mutable peer-group source standing in for the live config.
        let live: Arc<parking_lot::RwLock<HashMap<String, PeerGroupConfig>>> =
            Arc::new(parking_lot::RwLock::new(HashMap::new()));
        live.write()
            .insert("g1".into(), pg_with_peers("telegram", &["elisa"], &["@a"]));

        let resolver: AgentPeerGroupResolver = {
            let live = Arc::clone(&live);
            Arc::new(move || live.read().clone())
        };
        let tool = SendViaTool::new(Arc::new(SecurityPolicy::default()), map, resolver);

        async fn route(tool: &SendViaTool, target: &str) -> TurnRoutingEntry {
            let handle: TurnRoutingHandle = Arc::new(Mutex::new(Vec::new()));
            let result = TURN_ROUTING
                .scope(
                    Some(Arc::clone(&handle)),
                    tool.execute(json!({ "target": target })),
                )
                .await
                .unwrap();
            assert!(result.success, "{:?}", result.error);
            handle.lock().unwrap()[0].clone()
        }

        let before = route(&tool, "g1").await;
        assert_eq!(before.recipient.as_deref(), Some("@a"));

        // Reload: same group name, new recipient and a voice modality.
        live.write().insert(
            "g1".into(),
            pg_voice_with_peers("telegram", &["elisa"], &["@b"]),
        );

        let after = route(&tool, "g1").await;
        assert_eq!(
            after.recipient.as_deref(),
            Some("@b"),
            "reloaded external_peers must take effect without rebuilding the tool"
        );
        assert!(
            matches!(after.modality, OutputModality::Voice),
            "reloaded output_modality must take effect live"
        );
    }
}
