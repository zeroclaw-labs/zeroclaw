//! Read-only catalogs the dashboard shows: `integrations/list`,
//! `tools/cli-discover`, `plugins/list` and `a2a/identity`.
//!
//! Each body here is also what the matching HTTP route serializes, so the
//! route and the RPC method cannot drift.

use serde_json::Value;
use zeroclaw_api::jsonrpc::{JsonRpcError, error_codes};
use zeroclaw_config::schema::Config;

use crate::integrations::IntegrationEntry;

/// One integration as it goes over the wire.
#[must_use]
pub fn integration_entry_json(entry: &IntegrationEntry) -> Value {
    serde_json::json!({
        "name": &entry.name,
        "description": &entry.description,
        "category": entry.category,
        "category_label": entry.category.label(),
        "status": entry.status,
        // Canonical config map key (provider family key / ChannelsConfig map
        // key) for deep links; null when the entry has no config section.
        "key": &entry.key,
    })
}

/// The `integrations/list` body: every known integration and its status
/// under `config`.
#[must_use]
pub fn integrations_body(config: &Config) -> Value {
    let integrations: Vec<Value> = crate::integrations::registry::all_integrations(config)
        .iter()
        .map(integration_entry_json)
        .collect();
    serde_json::json!({ "integrations": integrations })
}

/// One tool spec as the dashboard lists it: name, description and parameter
/// schema, plus its output schema and parameter domains when it has them.
#[must_use]
pub fn tool_spec_json(spec: &zeroclaw_api::tool::ToolSpec) -> Value {
    let mut tool = serde_json::json!({
        "name": spec.name,
        "description": spec.description,
        "parameters": spec.parameters,
    });
    if let Some(output) = &spec.output {
        tool["output"] = output.clone();
    }
    if !spec.param_domains.is_empty() {
        tool["param_domains"] = serde_json::json!(spec.param_domains);
    }
    tool
}

/// The `tools/list` body for a set of specs.
#[must_use]
pub fn tools_body(specs: &[zeroclaw_api::tool::ToolSpec]) -> Value {
    let tools: Vec<Value> = specs.iter().map(tool_spec_json).collect();
    serde_json::json!({ "tools": tools })
}

/// The `tools/cli-discover` body: CLI tools found on the daemon's `PATH`.
///
/// Discovery spawns child processes and blocks, so it runs on a blocking
/// worker. If that worker panics the list is empty and the reason is logged,
/// rather than failing the request.
pub async fn cli_tools_body() -> Value {
    let tools = match tokio::task::spawn_blocking(|| {
        zeroclaw_tools::cli_discovery::discover_cli_tools(&[], &[])
    })
    .await
    {
        Ok(tools) => tools,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "cli-tools discovery task failed; returning empty list"
            );
            Vec::new()
        }
    };
    serde_json::json!({ "cli_tools": tools })
}

/// The `plugins/list` body: the configured plugin directory and what loads
/// from it under the configured signature policy.
///
/// The policy is resolved the way the runtime resolves it when loading tools,
/// so a plugin the agent refuses in strict mode never appears as loaded here.
/// A build without plugin support reports `plugins_enabled: false`, because
/// no plugin can load in it whatever the config says.
#[must_use]
pub fn plugins_body(config: &Config) -> Value {
    let plugins_enabled = cfg!(feature = "plugins-wasm") && config.plugins.enabled;
    let plugins: Vec<Value> = if plugins_enabled {
        loaded_plugins(config)
    } else {
        Vec::new()
    };
    serde_json::json!({
        "plugins_enabled": plugins_enabled,
        "plugins_dir": config.plugins.plugins_dir,
        "plugins": plugins,
    })
}

#[cfg(feature = "plugins-wasm")]
fn loaded_plugins(config: &Config) -> Vec<Value> {
    let plugin_path = config.plugins.resolved_plugins_dir();
    if !plugin_path.exists() {
        return Vec::new();
    }
    let signature_mode = zeroclaw_plugins::host::PluginHost::resolve_signature_mode(
        &config.plugins.security.signature_mode,
    );
    match zeroclaw_plugins::host::PluginHost::from_plugins_dir_with_security(
        &plugin_path,
        signature_mode,
        config.plugins.security.trusted_publisher_keys.clone(),
    ) {
        Ok(host) => host
            .list_plugins()
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "version": p.version,
                    "description": p.description,
                    "capabilities": p.capabilities,
                    "loaded": p.loaded,
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(not(feature = "plugins-wasm"))]
fn loaded_plugins(_config: &Config) -> Vec<Value> {
    Vec::new()
}

/// The `a2a/identity` body: the per-alias agent card for `agent`, or the
/// discovery catalog card without one.
///
/// The card is built from config. A gateway started with a host or port
/// override advertises that override on its own well-known routes; the core
/// does not learn the gateway's listener address, so it uses the configured
/// one.
pub fn a2a_identity(config: &Config, agent: Option<&str>) -> Result<Value, JsonRpcError> {
    if !config.a2a.server.enabled {
        return Err(JsonRpcError {
            code: error_codes::INVALID_REQUEST,
            message: "the A2A server is not enabled ([a2a.server] enabled = false)".into(),
            data: None,
        });
    }
    let card = match agent {
        Some(alias) => {
            crate::a2a_card::build_agent_card(config, alias).ok_or_else(|| JsonRpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("agent {alias:?} is not published over A2A"),
                data: None,
            })?
        }
        None => crate::a2a_card::build_catalog_card(config),
    };
    serde_json::to_value(card).map_err(|e| JsonRpcError {
        code: error_codes::INTERNAL_ERROR,
        message: format!("failed to serialize the agent card: {e}"),
        data: None,
    })
}
