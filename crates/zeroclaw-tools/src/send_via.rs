//! Per-turn output routing tool (`send_via`).

use crate::helpers::outbound_attachment::read_workspace_attachment;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::media::MediaAttachment;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::multi_agent::OutputModality;
use zeroclaw_config::multi_agent::PeerGroupConfig;
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};
use zeroclaw_config::schema::SendViaToolConfig;

/// Per-tool channel-map handle — matches `zeroclaw_runtime::tools::PerToolChannelHandle`.
pub type PerToolChannelHandle = Arc<parking_lot::RwLock<HashMap<String, Arc<dyn Channel>>>>;

/// Resolves the peer groups that include the active agent, read live from config
/// at call time so a config reload (membership / `external_peers` / channel alias /
/// `output_modality`) takes effect without rebuilding the tool registry.
pub type AgentPeerGroupResolver = Arc<dyn Fn() -> HashMap<String, PeerGroupConfig> + Send + Sync>;

/// Resolves the attachment count and size limits, read live from config at
/// call time so a config reload takes effect on the next send and in the
/// advertised schema without rebuilding the tool registry.
pub type SendViaLimitsResolver = Arc<dyn Fn() -> SendViaToolConfig + Send + Sync>;

tokio::task_local! {
    pub static TURN_ROUTING: Option<TurnRoutingHandle>;
}

/// Return type of [`SendViaTool::resolve_target`]: resolved channel key, channel arc,
/// modality, optional explicit recipient, and the peer group that granted the target.
type ResolvedTarget = Result<
    (
        String,
        Arc<dyn Channel>,
        OutputModality,
        Option<String>,
        ResolvedPeerGroup,
    ),
    String,
>;

/// The peer group a target resolved through, as far as attachments need it.
struct ResolvedPeerGroup {
    name: String,
    attachments_enabled: bool,
}

impl ResolvedPeerGroup {
    fn new(name: &str, pg: &PeerGroupConfig) -> Self {
        Self {
            name: name.to_string(),
            attachments_enabled: pg.attachments_enabled,
        }
    }
}

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

/// Static routing/modality wording that suggests a `send_via` call. Each entry
/// is a multiword phrase that, taken whole, requests a destination or delivery
/// modality — chosen to survive the trait's word-boundary matching contract
/// without firing on ordinary prose. Deliberately excluded: bare deadline /
/// style wording like "reply by" ("reply by Friday"), "respond by", "reply in"
/// ("reply in 5 minutes"), and "reply as" ("reply as soon as you can"), which
/// carry no routing intent. Live channel and peer-group names are added per
/// call on top.
const STATIC_INVOCATION_TRIGGERS: &[&str] = &[
    "send this to",
    "send it to",
    "send that to",
    "send to my",
    "forward this to",
    "forward it to",
    "redirect to",
    "reply by voice",
    "reply by text",
    "reply via",
    "by voice",
    "via voice",
    "as voice",
    "voice message",
    "voice note",
    "text only",
    "via email",
    "email this",
    "email it",
    "email me",
];

/// Dynamic trigger entries shorter than this are dropped. Word-boundary
/// matching already stops `dev` from firing inside `device`, but very short
/// names (1–2 characters) collide with too many standalone words to be useful
/// triggers. Compared by character count, not byte length.
const MIN_DYNAMIC_TRIGGER_LEN: usize = 3;

/// Agent-callable tool for per-turn output routing and channel fanout.
pub struct SendViaTool {
    security: Arc<SecurityPolicy>,
    channel_map: PerToolChannelHandle,
    /// Resolves the active agent's peer groups live from config at call time.
    agent_peer_groups: AgentPeerGroupResolver,
    /// Resolves the count and size limits for files attached to an immediate
    /// send live from config at call time.
    attachment_limits: SendViaLimitsResolver,
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
            attachment_limits: Arc::new(SendViaToolConfig::default),
        }
    }

    /// Replace the default attachment limits with a resolver over the
    /// configured ones.
    pub fn with_attachment_limits(mut self, limits: SendViaLimitsResolver) -> Self {
        self.attachment_limits = limits;
        self
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
    /// `target` may be a peer group name, a composite channel key, or a bare type.
    /// Returns `Err(reason)` when the target is rejected or not found.
    fn resolve_target(&self, target: &str) -> ResolvedTarget {
        let channel_map = self.channel_map.read();
        // Resolved live from config each call so reloads take effect immediately.
        let agent_peer_groups = (self.agent_peer_groups)();

        // --- 1. Try as peer group name ---
        if let Some(pg) = agent_peer_groups.get(target) {
            let pg_channel = &pg.channel;

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
                Some((key, ch)) => Ok((
                    key,
                    ch,
                    pg.output_modality,
                    recipient,
                    ResolvedPeerGroup::new(target, pg),
                )),
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

        let mut matching: Vec<(&String, &PeerGroupConfig)> = agent_peer_groups
            .iter()
            .filter(|(_, pg)| Self::peer_group_covers_channel(&pg.channel, &channel_key))
            .collect();

        match matching.len() {
            0 => Err(format!(
                "Channel '{channel_key}' is not within any peer group this agent belongs to"
            )),
            1 => {
                let (name, pg) = matching.remove(0);
                let recipient = pg.external_peers.first().map(|p| p.as_str().to_string());
                Ok((
                    channel_key,
                    channel,
                    pg.output_modality,
                    recipient,
                    ResolvedPeerGroup::new(name, pg),
                ))
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

    /// Check that `channel` may carry attachments to this peer group, then load
    /// every path within the configured limits. All or nothing: the first
    /// refusal is returned and no attachment is kept. The file reads run on one
    /// blocking task so a slow filesystem never stalls the async worker.
    async fn load_attachments(
        &self,
        channel_key: &str,
        channel: &dyn Channel,
        peer_group: &ResolvedPeerGroup,
        paths: &[String],
    ) -> Result<Vec<MediaAttachment>, String> {
        if !channel.supports_outbound_attachments() {
            return Err(crate::i18n::get_required_tool_string_with_args(
                "tool-send-via-error-channel-no-attachments",
                &[("channel", channel_key)],
            ));
        }
        if !peer_group.attachments_enabled {
            return Err(crate::i18n::get_required_tool_string_with_args(
                "tool-send-via-error-attachments-disabled",
                &[("peer_group", &peer_group.name)],
            ));
        }

        let limits = (self.attachment_limits)();
        let max_attachments = usize::try_from(limits.max_attachments).unwrap_or(usize::MAX);
        if paths.len() > max_attachments {
            return Err(crate::i18n::get_required_tool_string_with_args(
                "tool-send-via-error-too-many-attachments",
                &[
                    ("count", &paths.len().to_string()),
                    ("limit", &max_attachments.to_string()),
                ],
            ));
        }

        let security = Arc::clone(&self.security);
        let paths = paths.to_vec();
        let max_attachment_bytes = limits.max_attachment_bytes;
        let max_total_attachment_bytes = limits.max_total_attachment_bytes;
        tokio::task::spawn_blocking(move || {
            read_attachments(
                &security,
                &paths,
                max_attachment_bytes,
                max_total_attachment_bytes,
            )
        })
        .await
        .unwrap_or_else(|join_error| {
            Err(crate::i18n::get_required_tool_string_with_args(
                "tool-send-via-error-attachments-load-failed",
                &[("err", &join_error.to_string())],
            ))
        })
    }
}

/// Read every path from the workspace, refusing the whole set as soon as one
/// file is refused or the running total passes `max_total_attachment_bytes`.
/// Blocking: call it from a blocking task.
fn read_attachments(
    security: &SecurityPolicy,
    paths: &[String],
    max_attachment_bytes: u64,
    max_total_attachment_bytes: u64,
) -> Result<Vec<MediaAttachment>, String> {
    let mut total: u64 = 0;
    let mut attachments = Vec::with_capacity(paths.len());
    for path in paths {
        let attachment = read_workspace_attachment(security, path, max_attachment_bytes)?;
        total = total.saturating_add(attachment.data.len() as u64);
        if total > max_total_attachment_bytes {
            return Err(crate::i18n::get_required_tool_string_with_args(
                "tool-send-via-error-attachments-total-too-large",
                &[
                    ("limit", &max_total_attachment_bytes.to_string()),
                    ("path", path),
                ],
            ));
        }
        attachments.push(attachment);
    }
    Ok(attachments)
}

/// Read the optional `attachments` argument as a list of path strings. Absent or
/// `null` means none.
fn attachment_paths(args: &serde_json::Value) -> Result<Vec<String>, String> {
    match args.get("attachments") {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    crate::i18n::get_required_tool_string(
                        "tool-send-via-error-attachments-not-array",
                    )
                })
            })
            .collect(),
        Some(_) => Err(crate::i18n::get_required_tool_string(
            "tool-send-via-error-attachments-not-array",
        )),
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
         Modality applies to this turn's final reply. Progress already streamed \
         earlier in the turn (e.g. Telegram multi_message narration) keeps its \
         streamed modality and is not retracted.\n\
         \n\
         With `body` (immediate fanout — main reply still goes to originating channel):\n\
         - `send_via(target: \"email.default\", body: \"...\")` — send separate content elsewhere\n\
         - `send_via(target: \"email.default\", body: \"...\", attachments: [\"out/report.pdf\"])` \
         — also attach workspace files\n\
         `target` is required when `body` is present. `attachments` is accepted only here.\n\
         \n\
         `target` must be a channel alias (e.g. `telegram.default`) or a peer group name \
         the active agent belongs to. `modality` defaults to the peer group's output_modality."
    }

    fn invocation_triggers(&self) -> Vec<String> {
        let mut triggers: std::collections::BTreeSet<String> = STATIC_INVOCATION_TRIGGERS
            .iter()
            .map(|t| (*t).to_string())
            .collect();

        // Character count, not byte length: the rule excludes 1–2 *character*
        // names, and a short multibyte name (e.g. a 1-char CJK alias) has a
        // byte length >= 3 that a `.len()` check would wrongly admit. Counted
        // on the ORIGINAL configured name, before lowercasing: Unicode
        // lowercase can expand one scalar into several (e.g. 'İ' → "i̇"), and
        // counting afterwards would let an excluded-length name through.
        let long_enough = |s: &str| s.chars().count() >= MIN_DYNAMIC_TRIGGER_LEN;

        // Live views, mirroring resolve_target: recomputed per call so config
        // reloads (channels, peer groups) take effect without a registry rebuild.
        for key in self.channel_map.read().keys() {
            if let Some((channel_type, alias)) = key.split_once('.') {
                if long_enough(channel_type) {
                    triggers.insert(channel_type.to_lowercase());
                }
                let alias_lower = alias.to_lowercase();
                // "default" is an implementation alias, not user wording.
                if alias_lower != "default" && long_enough(alias) {
                    triggers.insert(alias_lower);
                }
            }
            if long_enough(key) {
                triggers.insert(key.to_lowercase());
            }
        }
        for group in (self.agent_peer_groups)().keys() {
            if long_enough(group) {
                triggers.insert(group.to_lowercase());
            }
        }

        triggers.into_iter().collect()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let limits = (self.attachment_limits)();
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
                },
                "attachments": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": crate::i18n::get_required_tool_string_with_args(
                        "tool-send-via-param-attachments",
                        &[
                            (
                                "max_attachments",
                                &limits.max_attachments.to_string(),
                            ),
                            (
                                "max_attachment_bytes",
                                &limits.max_attachment_bytes.to_string(),
                            ),
                            (
                                "max_total_attachment_bytes",
                                &limits.max_total_attachment_bytes.to_string(),
                            ),
                        ],
                    )
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
                output: ToolOutput::default(),
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

        let attachment_paths = match attachment_paths(&args) {
            Ok(paths) => paths,
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::json(json!({
                        "status": "rejected",
                        "reason": reason
                    })),
                    error: None,
                });
            }
        };

        // ── Immediate send mode (body present) ───────────────────────────────
        if let Some(body) = body {
            let target_key = match target {
                Some(ref t) => t.clone(),
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::json(json!({
                            "status": "rejected",
                            "reason": "`target` is required when `body` is present"
                        })),
                        error: None,
                    });
                }
            };

            let (channel_key, channel, pg_modality, recipient, peer_group) =
                match self.resolve_target(&target_key) {
                    Ok(v) => v,
                    Err(reason) => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::json(json!({
                                "target": target_key,
                                "status": "rejected",
                                "reason": reason
                            })),
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
                        output: ToolOutput::json(json!({
                            "target": target_key,
                            "status": "rejected",
                            "reason": "target peer group has no external_peers configured; \
                                       cannot determine send recipient"
                        })),
                        error: None,
                    });
                }
            };

            // Every attachment is checked and loaded before anything is sent, so
            // a refused file never leaves a partial delivery behind.
            let attachments = if attachment_paths.is_empty() {
                Vec::new()
            } else {
                match self
                    .load_attachments(
                        &channel_key,
                        channel.as_ref(),
                        &peer_group,
                        &attachment_paths,
                    )
                    .await
                {
                    Ok(attachments) => attachments,
                    Err(reason) => {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::json(json!({
                                "target": target_key,
                                "status": "rejected",
                                "reason": reason
                            })),
                            error: None,
                        });
                    }
                }
            };
            let attachment_names: Vec<String> = attachments
                .iter()
                .map(|attachment| attachment.file_name.clone())
                .collect();

            let modality = explicit_modality.unwrap_or(pg_modality);
            let message = match modality {
                OutputModality::Voice => SendMessage::new(&body, &recipient).force_voice(),
                OutputModality::Text => SendMessage::new(&body, &recipient).suppress_voice(),
                OutputModality::Mirror => SendMessage::new(&body, &recipient),
            }
            .with_attachments(attachments)
            .with_inline_attachments(false);

            return match channel.send(&message).await {
                Ok(()) => {
                    let mut output = json!({
                        "target": channel_key,
                        "mode": "immediate",
                        "resolved_modality": modality_str(modality),
                        "status": "ok"
                    });
                    if !attachment_names.is_empty() {
                        output["attachments"] = json!(attachment_names);
                    }
                    Ok(ToolResult {
                        success: true,
                        output: ToolOutput::json(output),
                        error: None,
                    })
                }
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: ToolOutput::json(json!({
                        "target": channel_key,
                        "status": "failed",
                        "reason": e.to_string()
                    })),
                    error: None,
                }),
            };
        }

        // ── Routing instruction mode (no body) ───────────────────────────────
        if !attachment_paths.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::json(json!({
                    "status": "rejected",
                    "reason": crate::i18n::get_required_tool_string(
                        "tool-send-via-error-attachments-need-immediate-send"
                    )
                })),
                error: None,
            });
        }

        if target.is_none() && explicit_modality.is_none() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::json(json!({
                    "status": "rejected",
                    "reason": "at least one of `target` or `modality` is required when `body` is absent"
                })),
                error: None,
            });
        }

        // Resolve channel + peer-group modality if a target was given.
        let (resolved_channel, resolved_modality, resolved_recipient) = if let Some(ref t) = target
        {
            match self.resolve_target(t) {
                Ok((key, _ch, pg_modality, recipient, _peer_group)) => {
                    // Fail closed: a cross-channel redirect must have a concrete recipient
                    // from external_peers so the orchestrator can address the target channel.
                    // (Modality-only routing with no target uses msg.reply_target instead.)
                    if recipient.is_none() {
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::json(json!({
                                "target": t,
                                "status": "rejected",
                                "reason": "target peer group has no external_peers configured; \
                                           cannot determine routing recipient"
                            })),
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
                        output: ToolOutput::json(json!({
                            "target": t,
                            "status": "rejected",
                            "reason": reason
                        })),
                        error: None,
                    });
                }
            }
        } else {
            // No target → same channel. Keep this branch total even if the
            // validation above is later rearranged.
            let Some(modality) = explicit_modality else {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::json(json!({
                        "status": "rejected",
                        "reason": "modality is required when target and body are absent"
                    })),
                    error: None,
                });
            };
            (None, modality, None)
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
                output: ToolOutput::json(json!({
                    "target": resolved_channel.as_deref().unwrap_or("<originating>"),
                    "status": "ignored",
                    "reason": "routing is only available while handling a channel turn"
                })),
                error: None,
            });
        }

        Ok(ToolResult {
            success: true,
            output: ToolOutput::json(json!({
                "target": resolved_channel.as_deref().unwrap_or("<originating>"),
                "mode": "routing",
                "resolved_modality": modality_str(resolved_modality),
                "status": "queued"
            })),
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
        /// What `supports_outbound_attachments` reports.
        attachments: bool,
    }

    impl StubChannel {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                sent: Arc::new(parking_lot::RwLock::new(Vec::new())),
                attachments: false,
            }
        }

        fn with_attachments(name: &str) -> Self {
            Self {
                attachments: true,
                ..Self::new(name)
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
        fn supports_outbound_attachments(&self) -> bool {
            self.attachments
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

    #[test]
    fn invocation_triggers_combine_static_and_live_names() {
        let (tool, _routing) = make_tool(
            vec![
                ("telegram.default", Arc::new(StubChannel::new("telegram"))),
                ("email.work", Arc::new(StubChannel::new("email"))),
            ],
            HashMap::from([("Family".to_string(), pg("telegram", &["ana"]))]),
        );
        let triggers = tool.inner.invocation_triggers();
        let has = |t: &str| triggers.iter().any(|x| x == t);

        // Static routing/modality wording.
        assert!(has("send it to"));
        assert!(has("via voice"));
        // Channel types, full keys, and non-default aliases.
        assert!(has("telegram"));
        assert!(has("telegram.default"));
        assert!(has("email"));
        assert!(has("email.work"));
        assert!(has("work"));
        // Peer-group names, lowercased.
        assert!(has("family"));
        // The implementation alias never becomes a bare trigger word.
        assert!(!has("default"));
        // Deadline / style wording that carries no routing intent must NOT be
        // a trigger (would false-positive under the matching contract on
        // "reply by Friday", "respond by noon", "reply as soon as you can").
        for prose in [
            "reply by",
            "respond by",
            "reply in",
            "reply as",
            "deliver to",
        ] {
            assert!(
                !has(prose),
                "ambiguous phrase must not be a trigger: {prose}"
            );
        }
        // Everything is lowercase: the scan contract is case-insensitive
        // matching against a lowercased message.
        assert!(triggers.iter().all(|t| t == &t.to_lowercase()));
        // Under word-boundary matching (zeroclaw_api::tool::
        // invocation_trigger_matches), a short channel type like "email" is a
        // safe whole-word trigger and must not be dropped for length.
        assert!(has("email"));
    }

    #[test]
    fn invocation_triggers_follow_live_config_changes() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let groups: Arc<parking_lot::RwLock<HashMap<String, PeerGroupConfig>>> =
            Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let resolver: AgentPeerGroupResolver = {
            let groups = Arc::clone(&groups);
            Arc::new(move || groups.read().clone())
        };
        let tool = SendViaTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::clone(&map),
            resolver,
        );

        let before = tool.invocation_triggers();
        assert!(!before.iter().any(|t| t == "family"));
        assert!(!before.iter().any(|t| t == "discord"));

        map.write().insert(
            "discord.main".to_string(),
            Arc::new(StubChannel::new("discord")) as Arc<dyn Channel>,
        );
        groups
            .write()
            .insert("family".to_string(), pg("discord", &["ana"]));

        let after = tool.invocation_triggers();
        assert!(after.iter().any(|t| t == "discord"));
        assert!(after.iter().any(|t| t == "main"));
        assert!(after.iter().any(|t| t == "family"));
    }

    #[test]
    fn short_multibyte_names_are_excluded_by_character_count() {
        // A one-character CJK peer-group name is 3 bytes but 1 character; the
        // min-length rule excludes 1–2 character names, so it must not become
        // a trigger (a byte-length check would wrongly admit it). A
        // three-character CJK name clears the rule.
        let (tool, _routing) = make_tool(
            vec![("telegram.default", Arc::new(StubChannel::new("telegram")))],
            HashMap::from([
                ("東".to_string(), pg("telegram", &["ana"])),
                ("東京都".to_string(), pg("telegram", &["ana"])),
            ]),
        );
        let triggers = tool.inner.invocation_triggers();
        assert!(!triggers.iter().any(|t| t == "東"));
        assert!(triggers.iter().any(|t| t == "東京都"));
    }

    #[test]
    fn name_length_is_counted_before_lowercase_expansion() {
        // Unicode lowercase can expand a scalar: 'İ' (U+0130) lowercases to
        // "i\u{307}" (two scalars). A two-character configured name like "İs"
        // becomes three scalars after lowercasing, so a post-lowercase count
        // would wrongly admit a name the 1–2 character rule excludes. The
        // floor must be applied to the original configured name.
        let name = "İs";
        assert_eq!(name.chars().count(), 2);
        assert!(name.to_lowercase().chars().count() >= MIN_DYNAMIC_TRIGGER_LEN);

        let (tool, _routing) = make_tool(
            vec![("telegram.default", Arc::new(StubChannel::new("telegram")))],
            HashMap::from([(name.to_string(), pg("telegram", &["ana"]))]),
        );
        let triggers = tool.inner.invocation_triggers();
        assert!(
            !triggers.iter().any(|t| t == &name.to_lowercase()),
            "case-expanding two-character name must stay excluded; got {triggers:?}"
        );

        // A three-character name containing the same expanding scalar still
        // clears the rule and lands lowercased.
        let (tool, _routing) = make_tool(
            vec![("telegram.default", Arc::new(StubChannel::new("telegram")))],
            HashMap::from([("İst".to_string(), pg("telegram", &["ana"]))]),
        );
        let triggers = tool.inner.invocation_triggers();
        assert!(
            triggers.iter().any(|t| t == &"İst".to_lowercase()),
            "three-character name must remain a trigger after lowercasing"
        );
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

    // ── Attachments ───────────────────────────────────────────────────────────

    /// A canonical workspace inside a temp root, plus a sibling directory
    /// outside the workspace.
    struct AttachmentFixture {
        _root: tempfile::TempDir,
        workspace: std::path::PathBuf,
        outside: std::path::PathBuf,
    }

    impl AttachmentFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            let outside = root.path().join("outside");
            std::fs::create_dir(&workspace).unwrap();
            std::fs::create_dir(&outside).unwrap();
            Self {
                workspace: workspace.canonicalize().unwrap(),
                outside: outside.canonicalize().unwrap(),
                _root: root,
            }
        }

        fn write(&self, relative: &str, bytes: &[u8]) {
            let path = self.workspace.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, bytes).unwrap();
        }

        fn security(&self) -> SecurityPolicy {
            SecurityPolicy {
                workspace_dir: self.workspace.clone(),
                ..SecurityPolicy::default()
            }
        }
    }

    fn mail_group(attachments_enabled: bool) -> HashMap<String, PeerGroupConfig> {
        HashMap::from([(
            "mail".to_string(),
            PeerGroupConfig {
                attachments_enabled,
                ..pg_with_peers("email", &["elisa"], &["ana@example.com"])
            },
        )])
    }

    fn attachment_tool(
        security: SecurityPolicy,
        channel: Arc<StubChannel>,
        peer_groups: HashMap<String, PeerGroupConfig>,
        limits: SendViaToolConfig,
    ) -> (TestTool, TurnRoutingHandle) {
        attachment_tool_with_limits(
            security,
            channel,
            peer_groups,
            Arc::new(move || limits.clone()),
        )
    }

    fn attachment_tool_with_limits(
        security: SecurityPolicy,
        channel: Arc<StubChannel>,
        peer_groups: HashMap<String, PeerGroupConfig>,
        limits: SendViaLimitsResolver,
    ) -> (TestTool, TurnRoutingHandle) {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write()
            .insert("email.default".to_string(), channel as Arc<dyn Channel>);
        let routing: TurnRoutingHandle = Arc::new(Mutex::new(Vec::new()));
        let groups = Arc::new(peer_groups);
        let inner = SendViaTool::new(Arc::new(security), map, Arc::new(move || (*groups).clone()))
            .with_attachment_limits(limits);
        let tool = TestTool {
            inner,
            routing: Arc::clone(&routing),
        };
        (tool, routing)
    }

    fn send_with(attachments: serde_json::Value) -> serde_json::Value {
        json!({
            "target": "mail",
            "body": "see attached",
            "attachments": attachments
        })
    }

    /// Assert a rejection whose reason contains `needle`, with nothing sent.
    fn assert_refused(
        result: &ToolResult,
        sent: &parking_lot::RwLock<Vec<SendMessage>>,
        needle: &str,
    ) {
        assert!(!result.success);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "rejected");
        let reason = out["reason"].as_str().unwrap();
        assert!(
            reason.contains(needle),
            "reason must contain {needle:?}: {reason}"
        );
        assert!(sent.read().is_empty(), "a refused call must send nothing");
    }

    #[tokio::test]
    async fn attachments_are_sent_through_a_capable_opted_in_channel() {
        let fx = AttachmentFixture::new();
        let pdf = b"%PDF-1.7\n%fixture\n";
        fx.write("out/report.bin", pdf);
        fx.write("notes.csv", b"a,b\n1,2\n");
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(true),
            SendViaToolConfig::default(),
        );

        let result = tool
            .execute(send_with(json!(["out/report.bin", "notes.csv"])))
            .await
            .unwrap();

        assert!(result.success, "{}", result.output);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["status"], "ok");
        assert_eq!(out["attachments"], json!(["report.bin", "notes.csv"]));
        let sent = sent.read();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].recipient, "ana@example.com");
        assert_eq!(sent[0].content, "see attached");
        let attachments = &sent[0].attachments;
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].file_name, "report.bin");
        assert_eq!(attachments[0].data, pdf);
        // Content sniffing wins over the misleading extension.
        assert_eq!(attachments[0].mime_type.as_deref(), Some("application/pdf"));
        assert!(attachments[0].marker.is_none());
        assert_eq!(attachments[1].file_name, "notes.csv");
        assert_eq!(attachments[1].mime_type.as_deref(), Some("text/csv"));
        assert!(routing.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_attachments_list_sends_plain_message() {
        let fx = AttachmentFixture::new();
        // No capability and no opt-in: an empty list must not trip either check.
        let ch = Arc::new(StubChannel::new("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(false),
            SendViaToolConfig::default(),
        );

        let result = tool.execute(send_with(json!([]))).await.unwrap();

        assert!(result.success, "{}", result.output);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(out.get("attachments").is_none());
        assert_eq!(sent.read().len(), 1);
        assert!(sent.read()[0].attachments.is_empty());
    }

    // The capability and opt-in tests name a missing file: loading it would
    // fail with a different reason, so the expected one proves the check runs
    // before any path is read.

    #[tokio::test]
    async fn attachments_refused_when_channel_cannot_carry_them() {
        let fx = AttachmentFixture::new();
        let ch = Arc::new(StubChannel::new("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(true),
            SendViaToolConfig::default(),
        );

        let result = tool
            .execute(send_with(json!(["missing.txt"])))
            .await
            .unwrap();

        assert_refused(&result, &sent, "Channel 'email.default' cannot deliver");
    }

    #[tokio::test]
    async fn attachments_refused_without_peer_group_opt_in() {
        let fx = AttachmentFixture::new();
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(false),
            SendViaToolConfig::default(),
        );

        let result = tool
            .execute(send_with(json!(["missing.txt"])))
            .await
            .unwrap();

        assert_refused(&result, &sent, "not enabled for peer group 'mail'");
    }

    /// Naming the channel alias instead of the peer group reaches the same
    /// group, so its opt-in decides in both directions.
    #[tokio::test]
    async fn attachments_via_channel_alias_follow_the_peer_group_opt_in() {
        let fx = AttachmentFixture::new();
        fx.write("a.txt", b"a");
        let by_alias = json!({
            "target": "email.default",
            "body": "see attached",
            "attachments": ["a.txt"]
        });

        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(false),
            SendViaToolConfig::default(),
        );
        let result = tool.execute(by_alias.clone()).await.unwrap();
        assert_refused(&result, &sent, "not enabled for peer group 'mail'");

        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(true),
            SendViaToolConfig::default(),
        );
        let result = tool.execute(by_alias).await.unwrap();
        assert!(result.success, "{}", result.output);
        let sent = sent.read();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].recipient, "ana@example.com");
        assert_eq!(sent[0].attachments.len(), 1);
        assert_eq!(sent[0].attachments[0].file_name, "a.txt");
        assert_eq!(sent[0].attachments[0].data, b"a");
    }

    /// An immediate send turns off the channel's inline attachments, so a
    /// file marker or a bare path in `body` arrives as text and only
    /// `attachments` can carry files.
    #[tokio::test]
    async fn immediate_send_delivers_body_as_text_without_inline_attachments() {
        for body in [
            "report [FILE:/x]",
            "see [ image : chart.png ]",
            "[IMA<tool></tool>GE:chart.png]",
            "out/report.pdf",
        ] {
            let fx = AttachmentFixture::new();
            let ch = Arc::new(StubChannel::with_attachments("email"));
            let sent = Arc::clone(&ch.sent);
            let (tool, _routing) = attachment_tool(
                fx.security(),
                ch,
                mail_group(true),
                SendViaToolConfig::default(),
            );

            let result = tool
                .execute(json!({ "target": "mail", "body": body }))
                .await
                .unwrap();

            assert!(result.success, "{body}: {}", result.output);
            let sent = sent.read();
            assert_eq!(sent.len(), 1, "{body}");
            assert_eq!(sent[0].content, body);
            assert!(sent[0].attachments.is_empty(), "{body}");
            assert!(!sent[0].inline_attachments, "{body}");
        }
    }
    #[tokio::test]
    async fn attachments_refused_in_routing_mode() {
        let fx = AttachmentFixture::new();
        fx.write("a.txt", b"a");
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(true),
            SendViaToolConfig::default(),
        );

        let result = tool
            .execute(json!({ "target": "mail", "attachments": ["a.txt"] }))
            .await
            .unwrap();

        assert_refused(&result, &sent, "need an immediate send");
        assert!(routing.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn attachments_must_be_an_array_of_strings() {
        let fx = AttachmentFixture::new();
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(true),
            SendViaToolConfig::default(),
        );

        for bad in [json!("a.txt"), json!([1]), json!([["a.txt"]])] {
            let result = tool.execute(send_with(bad)).await.unwrap();
            assert_refused(&result, &sent, "array of file path strings");
        }
    }

    #[tokio::test]
    async fn attachment_paths_are_refused_by_the_read_policy() {
        let fx = AttachmentFixture::new();
        fx.write("ok.txt", b"fine");
        std::fs::create_dir(fx.workspace.join("folder")).unwrap();
        std::fs::write(fx.outside.join("secret.txt"), b"secret").unwrap();
        let outside_file = fx.outside.join("secret.txt").display().to_string();
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(true),
            SendViaToolConfig::default(),
        );

        // A valid file first: a later refusal must still send nothing.
        for (path, needle) in [
            (outside_file.as_str(), "not allowed by security policy"),
            ("../outside/secret.txt", "'..' path components are refused"),
            ("folder", "not a regular file"),
            ("missing.txt", "cannot be resolved"),
        ] {
            let result = tool
                .execute(send_with(json!(["ok.txt", path])))
                .await
                .unwrap();
            assert_refused(&result, &sent, needle);
            let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
            assert!(
                out["reason"].as_str().unwrap().contains(path),
                "the refusal must name the path as given: {}",
                out["reason"]
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attachment_symlink_pointing_outside_is_refused() {
        let fx = AttachmentFixture::new();
        std::fs::write(fx.outside.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(fx.outside.join("secret.txt"), fx.workspace.join("link.txt"))
            .unwrap();
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(
            fx.security(),
            ch,
            mail_group(true),
            SendViaToolConfig::default(),
        );

        let result = tool.execute(send_with(json!(["link.txt"]))).await.unwrap();

        assert_refused(
            &result,
            &sent,
            "'link.txt' is outside the readable workspace",
        );
    }

    #[tokio::test]
    async fn attachment_runtime_config_file_is_refused() {
        let fx = AttachmentFixture::new();
        fx.write("config.toml", b"[secrets]\n");
        let security = SecurityPolicy {
            config_path: Some(fx.workspace.join("config.toml")),
            ..fx.security()
        };
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) =
            attachment_tool(security, ch, mail_group(true), SendViaToolConfig::default());

        let result = tool
            .execute(send_with(json!(["config.toml"])))
            .await
            .unwrap();

        assert_refused(&result, &sent, "runtime configuration file");
    }

    #[tokio::test]
    async fn attachment_limits_refuse_with_nothing_sent() {
        let fx = AttachmentFixture::new();
        fx.write("ten-a.txt", &[b'a'; 10]);
        fx.write("ten-b.txt", &[b'b'; 10]);
        fx.write("eleven.txt", &[b'c'; 11]);
        let limits = SendViaToolConfig {
            max_attachments: 2,
            max_attachment_bytes: 10,
            max_total_attachment_bytes: 15,
        };

        let cases = [
            // Per-file cap.
            (
                json!(["ten-a.txt", "eleven.txt"]),
                "'eleven.txt' is too large",
            ),
            // Combined cap: each file fits, together they do not.
            (
                json!(["ten-a.txt", "ten-b.txt"]),
                "combined limit of 15 bytes",
            ),
            // Count cap, checked before any file is read.
            (
                json!(["ten-a.txt", "ten-a.txt", "ten-a.txt"]),
                "Too many attachments: 3 (limit: 2",
            ),
        ];
        for (paths, needle) in cases {
            let ch = Arc::new(StubChannel::with_attachments("email"));
            let sent = Arc::clone(&ch.sent);
            let (tool, _routing) =
                attachment_tool(fx.security(), ch, mail_group(true), limits.clone());
            let result = tool.execute(send_with(paths)).await.unwrap();
            assert_refused(&result, &sent, needle);
        }

        // Within every limit the same tool settings send.
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) = attachment_tool(fx.security(), ch, mail_group(true), limits);
        let result = tool.execute(send_with(json!(["ten-a.txt"]))).await.unwrap();
        assert!(result.success, "{}", result.output);
        assert_eq!(sent.read()[0].attachments.len(), 1);
    }

    #[test]
    fn schema_documents_attachments_with_configured_limits() {
        let fx = AttachmentFixture::new();
        let (tool, _routing) = attachment_tool(
            fx.security(),
            Arc::new(StubChannel::with_attachments("email")),
            HashMap::new(),
            SendViaToolConfig {
                max_attachments: 3,
                max_attachment_bytes: 100,
                max_total_attachment_bytes: 200,
            },
        );
        let schema = tool.inner.parameters_schema();
        let attachments = &schema["properties"]["attachments"];
        assert_eq!(attachments["type"], "array");
        assert_eq!(attachments["items"]["type"], "string");
        let description = attachments["description"].as_str().unwrap();
        assert!(description.contains("At most 3 files, 100 bytes per file, 200 bytes"));
        assert!(description.contains("file_read"));
    }

    /// The same long-lived tool applies limits changed in the live config on
    /// its next send and advertises them in its schema, without a rebuild.
    #[tokio::test]
    async fn attachment_limits_follow_live_config_changes() {
        let fx = AttachmentFixture::new();
        fx.write("ten-a.txt", &[b'a'; 10]);
        fx.write("ten-b.txt", &[b'b'; 10]);
        let live = Arc::new(parking_lot::RwLock::new(SendViaToolConfig::default()));
        let resolver: SendViaLimitsResolver = {
            let live = Arc::clone(&live);
            Arc::new(move || live.read().clone())
        };
        let ch = Arc::new(StubChannel::with_attachments("email"));
        let sent = Arc::clone(&ch.sent);
        let (tool, _routing) =
            attachment_tool_with_limits(fx.security(), ch, mail_group(true), resolver);
        let advertised = |tool: &TestTool| {
            tool.inner.parameters_schema()["properties"]["attachments"]["description"]
                .as_str()
                .unwrap()
                .to_string()
        };

        let defaults = SendViaToolConfig::default();
        assert!(advertised(&tool).contains(&format!(
            "At most {} files, {} bytes per file, {} bytes in total",
            defaults.max_attachments,
            defaults.max_attachment_bytes,
            defaults.max_total_attachment_bytes
        )));
        let result = tool
            .execute(send_with(json!(["ten-a.txt", "ten-b.txt"])))
            .await
            .unwrap();
        assert!(result.success, "{}", result.output);
        sent.write().clear();

        // Reload: a lower count limit refuses the same call.
        live.write().max_attachments = 1;
        let result = tool
            .execute(send_with(json!(["ten-a.txt", "ten-b.txt"])))
            .await
            .unwrap();
        assert_refused(
            &result,
            &sent,
            "Too many attachments: 2 (limit: 1 per call)",
        );

        // Reload: lower byte limits refuse a single file and show in the schema.
        *live.write() = SendViaToolConfig {
            max_attachments: 1,
            max_attachment_bytes: 5,
            max_total_attachment_bytes: 5,
        };
        let result = tool.execute(send_with(json!(["ten-a.txt"]))).await.unwrap();
        assert_refused(
            &result,
            &sent,
            "attachment 'ten-a.txt' is too large: 10 bytes (limit: 5 bytes per file)",
        );
        assert!(
            advertised(&tool).contains("At most 1 files, 5 bytes per file, 5 bytes in total"),
            "the schema must advertise the reloaded limits: {}",
            advertised(&tool)
        );
    }
}
