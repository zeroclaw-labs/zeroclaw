//! Channel listing, relink and identity binding: the operations the
//! dashboard's channel routes and the core's `channels/*` methods share.
//!
//! These live in the channels crate because they reach channel internals
//! (QR-pairing sessions, persisted logins, the peer-group binder). The
//! gateway calls them directly; the core reaches them through the
//! `ChannelControl` capability registered with the daemon.

use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;
use serde_json::Value;
use zeroclaw_config::pairing::PairingGuard;
use zeroclaw_config::schema::{ChannelAliasInfo, Config};

pub fn compiled_readiness_key_for_alias<'a>(
    config: &'a Config,
    info: &'a ChannelAliasInfo,
) -> &'a str {
    if info.channel_type == "whatsapp"
        && config
            .channels
            .whatsapp
            .get(&info.alias)
            .is_some_and(|whatsapp| whatsapp.backend_type() == "web")
    {
        "whatsapp-web"
    } else {
        info.channel_type.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelReadinessState {
    Ready,
    Missing,
    Unknown,
}

pub const CHANNEL_LISTENER_HEALTH_MAX_AGE_SECS: i64 = 30;

#[derive(Debug, Clone, Serialize)]
pub struct ChannelReadiness {
    pub enabled: ChannelReadinessState,
    pub bound_to_agent: ChannelReadinessState,
    pub authenticated: ChannelReadinessState,
    pub listening: ChannelReadinessState,
    pub requirements: Vec<String>,
    pub notes: Vec<String>,
}

pub fn channel_readiness(
    config: &zeroclaw_config::schema::Config,
    info: &zeroclaw_config::schema::ChannelAliasInfo,
    health: &zeroclaw_runtime::health::HealthSnapshot,
    pairing: &PairingGuard,
) -> ChannelReadiness {
    let mut readiness = ChannelReadiness {
        enabled: if info.enabled {
            ChannelReadinessState::Ready
        } else {
            ChannelReadinessState::Missing
        },
        bound_to_agent: if info.owning_agent.is_some() {
            ChannelReadinessState::Ready
        } else {
            ChannelReadinessState::Missing
        },
        authenticated: ChannelReadinessState::Unknown,
        listening: ChannelReadinessState::Unknown,
        requirements: Vec::new(),
        notes: Vec::new(),
    };

    if readiness.enabled == ChannelReadinessState::Missing {
        readiness
            .requirements
            .push("Enable this channel alias.".to_string());
    }
    if readiness.bound_to_agent == ChannelReadinessState::Missing {
        readiness
            .requirements
            .push("Bind this channel to an enabled agent.".to_string());
    }

    if readiness.enabled == ChannelReadinessState::Ready
        && readiness.bound_to_agent == ChannelReadinessState::Ready
    {
        if info.channel_type == "webhook" {
            apply_webhook_readiness(config, &info.alias, health, pairing, &mut readiness);
        } else {
            apply_persisted_login_readiness(config, info, &mut readiness);
        }
    }

    readiness
}

/// Fill `readiness.authenticated` from the channel-owned persisted-login
/// probe (`crate::login_probe`). The probe resolves the same
/// on-disk session signal each QR-pairing channel uses at startup to decide
/// between resuming a session and minting a fresh QR code; nothing is
/// cached and nothing is written. Channel types without a typed QR-pairing
/// key (no probe, or feature not compiled) keep `authenticated: unknown`
/// and the existing "not checked yet" note.
fn apply_persisted_login_readiness(
    config: &zeroclaw_config::schema::Config,
    info: &zeroclaw_config::schema::ChannelAliasInfo,
    readiness: &mut ChannelReadiness,
) {
    use crate::login_probe::PersistedLogin;

    // Resolve the string key to the typed QR-pairing channel once; all
    // downstream dispatch is on the enum.
    let compiled_key = compiled_readiness_key_for_alias(config, info);
    let Some(channel) = crate::listing::qr_pairing_channel(compiled_key) else {
        readiness.notes.push(format!(
            "Live readiness is not checked for `{}` channels yet.",
            info.channel_type
        ));
        return;
    };

    match crate::login_probe::persisted_login(channel, config, &info.alias) {
        PersistedLogin::Present => {
            readiness.authenticated = ChannelReadinessState::Ready;
            readiness.notes.push(format!(
                "Live listener readiness is not checked for `{}` channels yet.",
                info.channel_type
            ));
        }
        PersistedLogin::Absent => {
            readiness.authenticated = ChannelReadinessState::Missing;
            readiness.requirements.push(
                "Pair this channel: no persisted login session was found on disk.".to_string(),
            );
        }
    }
}

pub fn channel_readiness_summary(readiness: &ChannelReadiness) -> (&'static str, &'static str) {
    if readiness.enabled == ChannelReadinessState::Missing
        || readiness.bound_to_agent == ChannelReadinessState::Missing
    {
        return ("inactive", "degraded");
    }

    if readiness.authenticated == ChannelReadinessState::Missing
        || readiness.listening == ChannelReadinessState::Missing
    {
        return ("error", "down");
    }

    if readiness.authenticated == ChannelReadinessState::Ready
        && readiness.listening == ChannelReadinessState::Ready
    {
        ("active", "healthy")
    } else {
        // At least one probe is Unknown and none reported Missing: not
        // enough signal to call the channel either healthy or down.
        ("unknown", "degraded")
    }
}

fn apply_webhook_readiness(
    config: &zeroclaw_config::schema::Config,
    alias: &str,
    health: &zeroclaw_runtime::health::HealthSnapshot,
    pairing: &PairingGuard,
    readiness: &mut ChannelReadiness,
) {
    let Some(webhook) = config.channels.webhook.get(alias) else {
        readiness.authenticated = ChannelReadinessState::Missing;
        readiness.listening = ChannelReadinessState::Missing;
        readiness
            .requirements
            .push("Webhook config block is missing.".to_string());
        return;
    };

    if pairing.require_pairing() && !pairing.is_paired() {
        readiness.authenticated = ChannelReadinessState::Missing;
        readiness
            .requirements
            .push("Pair the gateway before using the webhook endpoint.".to_string());
    } else {
        readiness.authenticated = ChannelReadinessState::Ready;
    }

    let component = format!("channel:webhook.{alias}");
    let component_health = health.components.get(&component);
    let component_status = component_health.map(|component| component.status.as_str());
    let supervised_listener_ok = component_health.is_some_and(component_health_ok_and_fresh);
    let listen_path = normalized_webhook_path(webhook.listen_path.as_deref());

    if supervised_listener_ok {
        readiness.listening = ChannelReadinessState::Ready;
    } else if component_status == Some("error") {
        readiness.listening = ChannelReadinessState::Missing;
        readiness.requirements.push(format!(
            "Resolve the listener error for `webhook.{alias}` before using this channel."
        ));
    } else {
        readiness.listening = ChannelReadinessState::Missing;
        readiness.requirements.push(format!(
            "Start a channel listener for `webhook.{alias}` on port {}{}.",
            webhook.port, listen_path
        ));
    }
}

fn component_health_ok_and_fresh(component: &zeroclaw_runtime::health::ComponentHealth) -> bool {
    if component.status != "ok" {
        return false;
    }

    let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(&component.updated_at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(updated_at.with_timezone(&chrono::Utc));
    age >= chrono::Duration::zero()
        && age <= chrono::Duration::seconds(CHANNEL_LISTENER_HEALTH_MAX_AGE_SECS)
}

fn normalized_webhook_path(path: Option<&str>) -> String {
    let trimmed = path.unwrap_or("/webhook").trim();
    if trimmed.is_empty() {
        "/webhook".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

/// The `channels/list` body: one entry per `[channels.<type>.<alias>]`
/// block with its owning agent, whether its type is compiled in, and its
/// readiness.
#[must_use]
pub fn channels_body(config: &Config, pairing: &PairingGuard) -> Value {
    let health = zeroclaw_runtime::health::snapshot();
    // One entry per `[channels.<type>.<alias>]` block. Owning
    // agent comes from the agents.<alias>.channels reverse lookup.
    let channels: Vec<Value> = config
        .channels_by_alias()
        .into_iter()
        .map(|info| {
            let composite = format!("{}.{}", info.channel_type, info.alias);
            let compiled_key = compiled_readiness_key_for_alias(config, &info);
            let compiled = crate::listing::is_channel_type_compiled(compiled_key);
            let readiness = channel_readiness(config, &info, &health, pairing);
            let (status, health_status) = if compiled {
                channel_readiness_summary(&readiness)
            } else {
                ("not_compiled", "unavailable")
            };
            serde_json::json!({
                "name": composite,
                "type": info.channel_type,
                "alias": info.alias,
                "owning_agent": info.owning_agent,
                "enabled": info.enabled,
                "compiled": compiled,
                "status": status,
                "message_count": 0,
                "last_message_at": null,
                "health": health_status,
                "readiness": readiness,
            })
        })
        .collect();
    serde_json::json!({ "channels": channels })
}

/// A channel operation that did not complete: the status the dashboard
/// route answers with and the JSON body both surfaces carry.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelFailure {
    pub http_status: u16,
    pub body: Value,
}

/// Clear a QR-pairing channel's persisted login so its next start begins a
/// fresh pairing. `channel` is the composite `<type>.<alias>` name
/// `channels/list` reports. Nothing is touched for an unknown channel or a
/// type with no relink operation.
pub fn relink(config: &Config, channel: &str) -> Result<Value, ChannelFailure> {
    let Some(info) = config
        .channels_by_alias()
        .into_iter()
        .find(|info| format!("{}.{}", info.channel_type, info.alias) == channel)
    else {
        return Err(ChannelFailure {
            http_status: 404,
            body: serde_json::json!({
                "error": format!("unknown channel {channel} — use the composite name from GET /api/channels"),
            }),
        });
    };

    // Resolve the string key to the typed QR-pairing channel once; probe
    // and relink dispatch on the same enum. `None` means the channel type
    // has no relink hook or its feature is not compiled — an explicit
    // no-op conflict where nothing is touched.
    let compiled_key = compiled_readiness_key_for_alias(config, &info);
    let Some(qr_channel) = crate::listing::qr_pairing_channel(compiled_key) else {
        return Err(ChannelFailure {
            http_status: 409,
            body: serde_json::json!({
                "channel": channel,
                "outcome": "unsupported",
                "error": format!(
                    "channel type {} has no relink operation (it does not use QR-pairing sessions) \
                     or the feature is not compiled into this binary; nothing was changed",
                    info.channel_type
                ),
            }),
        });
    };

    match crate::login_relink::relink(qr_channel, config, &info.alias) {
        Ok(crate::login_relink::RelinkOutcome::Cleared { removed }) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"channel": channel, "removed": removed})),
                "channel persisted login cleared for relink"
            );
            Ok(serde_json::json!({
                "channel": channel,
                "outcome": "cleared",
                "removed": removed,
                "restart_required": true,
                "note": "restart the channel (POST /admin/reload) to begin the fresh QR pairing",
            }))
        }
        Ok(crate::login_relink::RelinkOutcome::NothingToClear) => Ok(serde_json::json!({
            "channel": channel,
            "outcome": "nothing_to_clear",
            "removed": [],
            "restart_required": false,
            "note": "no persisted login was stored; the next channel start already begins a fresh QR pairing",
        })),
        Err(e) => Err(ChannelFailure {
            http_status: 500,
            body: serde_json::json!({
                "channel": channel,
                "error": format!("failed to clear persisted login: {e}"),
            }),
        }),
    }
}

/// Why an identity bind did not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindFailureKind {
    /// The channel type has no identity binding, or the identity is invalid.
    ValidationFailed,
    /// No `[channels.<type>.<alias>]` block names this channel.
    PathNotFound,
    /// The persisted peer policy could not be read, or the save failed.
    ReloadFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindFailure {
    pub kind: BindFailureKind,
    pub message: String,
}

impl BindFailure {
    fn new(kind: BindFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Authorize an operator-named `identity` on one channel alias, the
/// equivalent of `zeroclaw channel bind-<type> <identity> --alias <alias>`.
///
/// Writes only `peer_groups.<group>.external_peers`, under the config write
/// lock, onto the current on-disk document, then swaps `config`. The body
/// reports `saved: false, already_bound: true` when the identity already
/// holds the grant. Channels running on another config copy pick the peer up
/// on the next reload.
pub async fn bind(
    config: &Arc<RwLock<Config>>,
    config_write_lock: &Arc<tokio::sync::Mutex<()>>,
    channel_type: &str,
    alias: &str,
    identity: &str,
) -> Result<Value, BindFailure> {
    // Serialize the whole read-mutate-swap section: acquired before the
    // read-for-modify below and held through the swap, so a concurrent
    // config writer can't land between the read and the save/swap.
    let _cfg_guard = Arc::clone(config_write_lock).lock_owned().await;
    let channel_type = channel_type.trim();
    let alias = alias.trim();

    // Closed-set gate: only telegram/wechat/line have an operator-bind surface.
    if crate::orchestrator::channel_identity_normalizer(channel_type).is_none() {
        return Err(BindFailure::new(
            BindFailureKind::ValidationFailed,
            format!(
                "channel type `{channel_type}` does not support identity binding \
                 (supported: telegram, wechat, line)"
            ),
        ));
    }

    let mut working = config.read().clone();

    // The daemon gives the gateway, the RPC path and the channels separate
    // `Config` copies of the same file, so the write lock alone is not
    // enough: this handle's `peer_groups` can be older than what another
    // writer has already saved. Without the refresh, an `ignore` persisted
    // through another surface is both invisible to the bind check below and
    // overwritten by the save.
    match zeroclaw_config::schema::persisted_peer_groups(&working.config_path).await {
        Ok(Some(persisted)) => working.peer_groups = persisted,
        Ok(None) => {}
        Err(e) => {
            return Err(BindFailure::new(
                BindFailureKind::ReloadFailed,
                format!("could not read the persisted peer policy: {e}"),
            ));
        }
    }

    // Reject a phantom alias loudly rather than minting a peer group the
    // runtime never reads.
    if !crate::orchestrator::channel_alias_configured(&working, channel_type, alias) {
        return Err(BindFailure::new(
            BindFailureKind::PathNotFound,
            format!("channel `{channel_type}.{alias}` is not configured"),
        ));
    }

    let target = crate::orchestrator::bind_channel_identity_into(
        &mut working,
        channel_type,
        alias,
        identity,
    )
    .map_err(|e| BindFailure::new(BindFailureKind::ValidationFailed, e.to_string()))?;

    let channel = format!("{channel_type}.{alias}");

    // The writer picks its target by the group's `channel` field, so the
    // destination may be any key, not the conventional `<type>_<alias>`.
    let Some(group) = target else {
        // Name the group that actually carries the grant, including a bare
        // type-wide one.
        let source = crate::orchestrator::channel_authorizing_group_key(
            &working,
            channel_type,
            alias,
            identity,
        )
        .or_else(|| crate::orchestrator::channel_peer_group_key(&working, channel_type, alias));
        return Ok(serde_json::json!({
            "saved": false,
            "already_bound": true,
            "group": source,
            "channel": channel,
        }));
    };

    // Incremental: only `peer_groups` is applied onto the current on-disk
    // document, so the rest of this snapshot cannot drop another writer's
    // keys. A direct peer-group mutation is not dirty-tracked, so the
    // explicit `mark_dirty` is what makes `save_dirty` write it at all.
    working.mark_dirty("peer_groups");
    if let Err(e) = working.save_dirty().await {
        return Err(BindFailure::new(
            BindFailureKind::ReloadFailed,
            format!("save failed: {e}"),
        ));
    }
    *config.write() = working;

    Ok(serde_json::json!({
        "saved": true,
        "already_bound": false,
        "group": group,
        "channel": channel,
    }))
}

/// The channels crate's [`zeroclaw_runtime::rpc::channels::ChannelControl`]:
/// the operations above, with each refusal mapped to a JSON-RPC error whose
/// `data` carries the route's error body.
pub struct ChannelsControl;

#[async_trait::async_trait]
impl zeroclaw_runtime::rpc::channels::ChannelControl for ChannelsControl {
    fn list(&self, config: &Config, pairing: &PairingGuard) -> Value {
        channels_body(config, pairing)
    }

    fn relink(
        &self,
        config: &Config,
        channel: &str,
    ) -> Result<Value, zeroclaw_api::jsonrpc::JsonRpcError> {
        use zeroclaw_api::jsonrpc::error_codes;
        relink(config, channel).map_err(|failure| zeroclaw_api::jsonrpc::JsonRpcError {
            code: match failure.http_status {
                404 => error_codes::INVALID_PARAMS,
                409 => error_codes::INVALID_REQUEST,
                _ => error_codes::INTERNAL_ERROR,
            },
            message: failure.body["error"]
                .as_str()
                .unwrap_or("channel relink failed")
                .to_string(),
            data: Some(failure.body),
        })
    }

    async fn bind(
        &self,
        config: &Arc<RwLock<Config>>,
        config_write_lock: &Arc<tokio::sync::Mutex<()>>,
        channel_type: &str,
        alias: &str,
        identity: &str,
    ) -> Result<Value, zeroclaw_api::jsonrpc::JsonRpcError> {
        use zeroclaw_api::jsonrpc::error_codes;
        bind(config, config_write_lock, channel_type, alias, identity)
            .await
            .map_err(|failure| zeroclaw_api::jsonrpc::JsonRpcError {
                code: match failure.kind {
                    BindFailureKind::ValidationFailed | BindFailureKind::PathNotFound => {
                        error_codes::INVALID_PARAMS
                    }
                    BindFailureKind::ReloadFailed => error_codes::INTERNAL_ERROR,
                },
                message: failure.message,
                data: None,
            })
    }
}
