//! REST API handlers for the dedicated MCP dashboard tab.
//!
//! These are the *live/runtime* MCP endpoints — they complement the static
//! config CRUD (`/api/config/*`, which already manages `mcp.servers[]` and
//! `mcp_bundles`). Server add/edit/delete still flows through the config API;
//! these endpoints only report connection state and probe connectivity:
//!
//! - `GET  /api/mcp/status` — per-server connection state + discovered tools,
//!   merged from the live [`McpRegistry`] captured at gateway startup. Never
//!   echoes secret values (`env`/`headers`); the form gets those, masked, from
//!   the config API.
//! - `POST /api/mcp/test` — connect to a single server on demand and return its
//!   tool list (or the error). Lets the user validate a server before saving /
//!   reloading. Masked secrets in the body are merged back from the in-memory
//!   config so "test what you'd save" works without re-typing tokens.
//!
//! All routes require bearer-token auth, like the rest of `/api/*`.

use std::collections::HashMap;
use std::time::Duration;

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Json},
};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_config::schema::{McpServerConfig, McpTransport};
use zeroclaw_config::traits::MASKED_SECRET;
use zeroclaw_tools::mcp_client::{McpServer, McpServerStatus};

use crate::AppState;
use crate::api::require_auth;
use crate::api_config::{error_response, persist_and_swap};

/// Hard cap on per-tool timeout, mirroring `validate_mcp_config` in the schema
/// crate so the dashboard rejects the same out-of-range values the daemon would.
const MAX_TOOL_TIMEOUT_SECS: u64 = 600;

/// Hard ceiling for an on-demand `POST /api/mcp/test` probe. The client's
/// internal init/list timeout is 30s per round-trip; this outer guard bounds
/// the whole handshake so a wedged transport can't hang the request.
const TEST_CONNECT_TIMEOUT_SECS: u64 = 40;

/// Lowercase transport label matching the TOML `serde(rename_all = "lowercase")`.
fn transport_label(t: &zeroclaw_config::schema::McpTransport) -> &'static str {
    use zeroclaw_config::schema::McpTransport;
    match t {
        McpTransport::Stdio => "stdio",
        McpTransport::Http => "http",
        McpTransport::Sse => "sse",
    }
}

/// Mask a secret string map for transport to the dashboard: keys are preserved
/// (so the form can render the rows), every non-empty value becomes the masked
/// sentinel. The real value is only re-sent if the operator edits it — the PUT
/// handler restores untouched (`***MASKED***`) values from the in-memory config.
fn mask_map(map: &HashMap<String, String>) -> serde_json::Value {
    let masked: HashMap<&str, &str> = map.keys().map(|k| (k.as_str(), MASKED_SECRET)).collect();
    serde_json::json!(masked)
}

/// Discovered tools serialized for the dashboard (name + description only —
/// the full input schema is available via `/api/tools`).
fn tools_json(status: Option<&McpServerStatus>) -> Vec<serde_json::Value> {
    status
        .map(|s| {
            s.tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// GET /api/mcp/status — live MCP connection state for every configured server.
///
/// Merges `config.mcp.servers` (the desired set) with the live registry
/// statuses (the actual connect outcome from startup). A server present in
/// config but absent from the registry — e.g. added since the last reload —
/// reports `connected: false` with no error.
pub async fn handle_mcp_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let cfg = state.config.read();
    let registry = state.mcp_registry.as_ref();

    let servers: Vec<serde_json::Value> = cfg
        .mcp
        .servers
        .iter()
        .map(|server| {
            let live = registry.and_then(|r| r.statuses().iter().find(|s| s.name == server.name));
            serde_json::json!({
                "name": server.name,
                "transport": transport_label(&server.transport),
                "url": server.url,
                "command": server.command,
                "args": server.args,
                "tool_timeout_secs": server.tool_timeout_secs,
                // Secret maps: keys preserved, values masked (never echoed).
                "env": mask_map(&server.env),
                "headers": mask_map(&server.headers),
                "connected": live.map(|s| s.connected).unwrap_or(false),
                "tool_count": live.map(|s| s.tool_count).unwrap_or(0),
                "error": live.and_then(|s| s.error.clone()),
                "tools": tools_json(live),
            })
        })
        .collect();

    Json(serde_json::json!({
        "enabled": cfg.mcp.enabled,
        "deferred_loading": cfg.mcp.deferred_loading,
        // True once the registry was built at startup (i.e. config matched what
        // is running). The UI uses this to decide whether to show a "reload to
        // apply" hint for servers that aren't yet connected.
        "registry_initialized": registry.is_some(),
        "servers": servers,
    }))
    .into_response()
}

/// Validate a server list the way `validate_mcp_config` does, returning a
/// dashboard-friendly error. Kept in sync with the schema crate's rules:
/// non-empty unique names, stdio needs a command, http/sse need a valid
/// http(s) URL, and the per-tool timeout is bounded.
fn validate_servers(servers: &[McpServerConfig]) -> Result<(), ConfigApiError> {
    let mut seen = std::collections::HashSet::new();
    for (i, s) in servers.iter().enumerate() {
        let name = s.name.trim();
        let at = format!("mcp.servers[{i}]");
        if name.is_empty() {
            return Err(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("{at}.name must not be empty"),
            )
            .with_path(&at));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("duplicate MCP server name: {name}"),
            )
            .with_path(&at));
        }
        if let Some(t) = s.tool_timeout_secs
            && (t == 0 || t > MAX_TOOL_TIMEOUT_SECS)
        {
            return Err(ConfigApiError::new(
                ConfigApiCode::ValidationFailed,
                format!("{at}.tool_timeout_secs must be between 1 and {MAX_TOOL_TIMEOUT_SECS}"),
            )
            .with_path(&at));
        }
        match s.transport {
            McpTransport::Stdio => {
                if s.command.trim().is_empty() {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("{at}: stdio transport requires a command"),
                    )
                    .with_path(&at));
                }
            }
            McpTransport::Http | McpTransport::Sse => {
                let url = s.url.as_deref().map(str::trim).unwrap_or("");
                // Lightweight client-side scheme check; the daemon runs the
                // full URL parse in `validate_mcp_config` on load.
                let ok = url.starts_with("http://") || url.starts_with("https://");
                if !ok {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!(
                            "{at}: {} transport requires a valid http(s) URL",
                            transport_label(&s.transport)
                        ),
                    )
                    .with_path(&at));
                }
            }
        }
    }
    Ok(())
}

/// PUT /api/mcp/servers — replace the whole `mcp.servers` list.
///
/// This is the only way to configure server *fields* (transport, command,
/// url, args, env, headers, timeout): `mcp.servers` is a `#[nested] Vec<T>`
/// List section, which the generic config prop API can only add/remove by
/// name — it exposes no per-field paths. Masked secrets (`***MASKED***`) are
/// restored per-server (matched by name) from the in-memory config so the
/// dashboard never has to re-send tokens the operator didn't change.
///
/// New/edited servers connect on the next daemon reload (the registry is built
/// at startup); `POST /api/mcp/test` validates connectivity in the meantime.
pub async fn handle_mcp_servers_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut servers): Json<Vec<McpServerConfig>>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut working = state.config.read().clone();

    // Restore masked secrets from the current config (match by name).
    for server in &mut servers {
        if let Some(stored) = working.mcp.servers.iter().find(|s| s.name == server.name) {
            unmask_from_stored(server, stored);
        }
    }

    if let Err(e) = validate_servers(&servers) {
        return error_response(e);
    }

    working.mcp.servers = servers;
    working.mark_dirty("mcp.servers");
    if let Err(e) = persist_and_swap(&state, working).await {
        return error_response(e);
    }

    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Replace masked secret sentinels in `submitted` with the real values from the
/// matching stored server (by name), so a "test" probe uses live credentials
/// the operator didn't re-type. Values the operator actually changed (anything
/// other than the `***MASKED***` sentinel) are left untouched.
fn unmask_from_stored(submitted: &mut McpServerConfig, stored: &McpServerConfig) {
    for (key, value) in submitted.env.iter_mut() {
        if value == MASKED_SECRET {
            if let Some(real) = stored.env.get(key) {
                *value = real.clone();
            }
        }
    }
    for (key, value) in submitted.headers.iter_mut() {
        if value == MASKED_SECRET {
            if let Some(real) = stored.headers.get(key) {
                *value = real.clone();
            }
        }
    }
}

/// POST /api/mcp/test — connect to a single MCP server on demand and report the
/// outcome. Body is a full [`McpServerConfig`]. Returns `{ ok, tool_count,
/// tools }` on success or `{ ok: false, error }` on failure. Non-mutating: the
/// probe connection is dropped immediately after the tool list is fetched.
pub async fn handle_mcp_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut server): Json<McpServerConfig>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Merge masked secrets back from the in-memory config (real values live in
    // memory in plaintext; only the wire/disk forms are masked/encrypted).
    {
        let cfg = state.config.read();
        if let Some(stored) = cfg.mcp.servers.iter().find(|s| s.name == server.name) {
            unmask_from_stored(&mut server, stored);
        }
    }

    let connect = McpServer::connect(server);
    let result =
        match tokio::time::timeout(Duration::from_secs(TEST_CONNECT_TIMEOUT_SECS), connect).await {
            Err(_) => {
                return Json(serde_json::json!({
                    "ok": false,
                    "error": format!("timed out after {TEST_CONNECT_TIMEOUT_SECS}s"),
                }))
                .into_response();
            }
            Ok(r) => r,
        };

    match result {
        Ok(srv) => {
            let tools = srv.tools().await;
            let tools_json: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| serde_json::json!({"name": t.name, "description": t.description}))
                .collect();
            Json(serde_json::json!({
                "ok": true,
                "tool_count": tools.len(),
                "tools": tools_json,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "error": format!("{e:#}"),
        }))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::McpTransport;

    fn server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn transport_labels_match_wire_form() {
        assert_eq!(transport_label(&McpTransport::Stdio), "stdio");
        assert_eq!(transport_label(&McpTransport::Http), "http");
        assert_eq!(transport_label(&McpTransport::Sse), "sse");
    }

    #[test]
    fn unmask_restores_only_masked_values() {
        let mut stored = server("fs");
        stored.env.insert("TOKEN".into(), "real-secret".into());
        stored.env.insert("OTHER".into(), "real-other".into());
        stored
            .headers
            .insert("Authorization".into(), "Bearer real".into());

        let mut submitted = server("fs");
        // Untouched secret → comes back masked, must be restored.
        submitted.env.insert("TOKEN".into(), MASKED_SECRET.into());
        // Explicitly changed secret → must be kept as typed.
        submitted.env.insert("OTHER".into(), "changed".into());
        submitted
            .headers
            .insert("Authorization".into(), MASKED_SECRET.into());

        unmask_from_stored(&mut submitted, &stored);

        assert_eq!(submitted.env.get("TOKEN").unwrap(), "real-secret");
        assert_eq!(submitted.env.get("OTHER").unwrap(), "changed");
        assert_eq!(
            submitted.headers.get("Authorization").unwrap(),
            "Bearer real"
        );
    }

    #[test]
    fn unmask_leaves_unknown_masked_keys_as_is() {
        // A masked key with no stored counterpart can't be restored; it stays
        // masked rather than panicking or inventing a value.
        let stored = server("fs");
        let mut submitted = server("fs");
        submitted.env.insert("NEW".into(), MASKED_SECRET.into());
        unmask_from_stored(&mut submitted, &stored);
        assert_eq!(submitted.env.get("NEW").unwrap(), MASKED_SECRET);
    }
}
