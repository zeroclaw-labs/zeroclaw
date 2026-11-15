//! Non-fatal validation warnings — config that loads and validates
//! successfully (i.e. `Config::validate()` returns `Ok(())`) but will fail
//! at agent runtime because of a logical inconsistency the schema can't
//! enforce structurally.

use serde::{Deserialize, Serialize};

/// One non-fatal validation issue surfaced after a successful save.
///
/// Stable codes (extend as new warnings are added):
/// - `memory_semantic_search_without_embedder`: `memory.search_mode` requests
///   vector search on sqlite memory, but no effective embedder is configured.
/// - `memory_config_knob_inert`: a `[memory]` knob is set to a non-default
///   value but has no runtime consumer yet, so it currently has no effect
///   (see `validate_memory_semantics` in `schema.rs` for the current list).
/// - `context_compression_unsupported`: a `runtime_profiles.<alias>.context_compression`
///   knob (`enabled = true`, or any other field set to a non-default value)
///   has no runtime consumer — the context compressor was removed —
///   so it currently has no effect. One warning per non-default field (see
///   `collect_context_compression_ignored_warnings` in `schema.rs`).
/// - `otp_action_gating_unsupported`: a `[security.otp]` action-gating knob
///   (`gated_actions`, `gated_domains`, `gated_domain_categories`, or
///   `challenge_max_attempts`) is set to a non-default value but ZeroClaw
///   has no OTP action-gating, so it is parsed for compatibility and never
///   enforced (see `collect_deprecated_otp_action_gating_warnings` in
///   `schema.rs`); live OTP authentication (`enabled`, `token_ttl_secs`,
///   `cache_valid_secs`) is unaffected.
/// - `gateway_pairing_dashboard_removed`: the `[gateway.pairing_dashboard]`
///   config section was removed from the schema; a `ZEROCLAW_gateway__
///   pairing_dashboard__*` env override (ignored by the env-override
///   tombstone before the unknown-path rejection) or a leftover
///   `[gateway.pairing_dashboard]` file section is ignored and reported via
///   this warning (see `RETIRED_CONFIG_SURFACES` below, consumed by the
///   env-override tombstone and `retired_section_tombstones`). The
///   compatibility shims will be removed in a later announced window.
/// - `inbound_webhook_channel_removed`: a `[channels.linq]`, `[channels.wati]`,
///   `[channels.nextcloud_talk]` or `[channels.gmail_push]` section is still
///   present. These channels received messages only through gateway webhook
///   routes that were removed, so the channel types were deleted; the section
///   is ignored (see `RETIRED_CONFIG_SURFACES`).
/// - `retired_channel_transport_removed`: a channel section for a transport
///   whose schema was deleted by #584 Slice 3, including its V1 spelling.
/// - `whatsapp_audio_option_removed`: the WhatsApp-only forwarded audio
///   selector has no consumer after the WhatsApp transport retired.
/// - `wss_transport_removed`: a `[wss]` section is still present. It
///   configured the daemon's JSON-RPC-over-WSS listener for remote TUI
///   clients; that listener and the local `daemon.sock` RPC socket were
///   retired (no in-repo client remained; the web dashboard and
///   `zeroclaw chat` use the gateway HTTP API and `/ws/chat`), so the
///   section is ignored (see `RETIRED_CONFIG_SURFACES`).
/// - `a2a_server_removed`: an `[a2a]` section or an
///   `[agents.<alias>.a2a]` block is still present. The fork delegates all
///   external work through the Tachi bridge (#381) and never ran the A2A
///   discovery server or agent cards; the surfaces carried no consumer and
///   were deleted, so the keys are ignored (see `RETIRED_CONFIG_SURFACES`
///   and `RETIRED_CONFIG_FIELDS`).
/// - `eval_harness_moved_to_dev_tool`: `[eval]` moved to the development
///   CLI; pass its former settings using `--suite` and `--mode`.
/// - `todo_tracker_removed`: a `[todotracker]` section is still present. It
///   configured the ZeroCode Code-pane TodoWrite tracker; ZeroCode was
///   retired (#387) and no other reader remained, so the section is ignored
///   (see `RETIRED_CONFIG_SURFACES`).
/// - `conversational_ai_removed`: a `[conversational_ai]` section is still
///   present. It carried no consumer in this fork (experimental upstream
///   feature never wired here), so the section is ignored (see
///   `RETIRED_CONFIG_SURFACES`).
/// - `wasm_plugins_removed`: a `[plugins]` section is still present. The
///   WASM plugin host, its registry CLI and the `wit/` contracts were
///   retired; extensions are served through MCP servers (`[mcp]`), so the
///   section is ignored (see `RETIRED_CONFIG_SURFACES`).
/// - `hardware_moved_out_of_core`: a `[hardware]` or `[peripherals]`
///   section is still present. In-process hardware support (GPIO, serial
///   boards, debug probes, datasheet RAG) left the body; devices join as
///   gateway Nodes instead (ADR-017), so the section is ignored (see
///   `RETIRED_CONFIG_SURFACES`).
/// - `saas_integration_removed`: a `[jira]`, `[linkedin]`, `[microsoft365]`,
///   `[google_workspace]`, `[cloud_ops]`, `[project_intel]`, `[composio]` or
///   `[notion]` section is still present. Those vendor tool families were
///   retired (external services are reached through MCP servers, and outbound
///   notifications through the gateway bridges' `notify` tool), so the section
///   is ignored (see `RETIRED_CONFIG_SURFACES`).
/// - `otel_observability_removed`: `[observability] backend` still names the
///   retired OpenTelemetry exporter (`otel`, `otlp`, `opentelemetry`) or an
///   `otel_*` field is still set. The exporter was removed; the backend loads
///   as `none` and the fields are ignored (see `RETIRED_CONFIG_VALUES` and
///   `RETIRED_CONFIG_FIELDS`).
/// - `memory_backend_removed`: `memory.backend` or `agents.<a>.memory.backend`
///   still selects the removed `postgres` or `qdrant` backend, or a
///   `[storage.postgres]` / `[storage.qdrant]` section is still present. The
///   config keeps loading, but selecting a removed backend fails memory
///   construction with an explicit error; it never falls back to another
///   store (see `RETIRED_CONFIG_VALUES`, `RETIRED_CONFIG_SURFACES` and
///   [`RETIRED_MEMORY_BACKENDS`]).
/// - `browser_native_backend_removed`: `browser.backend` still selects the
///   removed WebDriver backend (`rust_native` / `native`) or a
///   `browser.native_*` key is still set. Selecting it fails the browser tool
///   with an explicit error (see `RETIRED_CONFIG_VALUES` and
///   `RETIRED_CONFIG_FIELDS`).
/// - `ollama_native_options_removed`: an Ollama alias still sets `num_ctx`,
///   `num_predict` or `temperature_override`. These native `/api/chat` options
///   were not consumed by the current OpenAI-compatible `/v1` provider and
///   were removed from the schema (see `RETIRED_CONFIG_FIELDS`).
/// - `cost_optimized_routing_removed`: a runtime profile still sets
///   `auto_classify.cost_optimized_hint`. The unused cost selector was
///   removed; the three complexity hints and ordinary named routes remain.
/// - `knowledge_graph_removed`: a `[knowledge]` section is still present. The
///   knowledge-graph tool and its private `knowledge.db` store were retired —
///   relationship memory belongs to the Tachi memory kernel, not a second
///   local graph — so the section is ignored (see `RETIRED_CONFIG_SURFACES`).
/// - `knowledge_bundles_removed`: a `[knowledge_bundles.<alias>]` section or
///   an `agents.<alias>.knowledge_bundles` list is still present. The bundles
///   only fed the retired knowledge-graph tool, so both surfaces are ignored
///   (see `RETIRED_CONFIG_SURFACES` and `RETIRED_CONFIG_FIELDS`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ValidationWarning {
    /// Stable machine-readable identifier for the warning class.
    pub code: String,
    /// Human-readable description suitable for direct display.
    pub message: String,
    /// Dotted property path the warning concerns
    /// (e.g. `"agents.researcher.model_provider"`).
    pub path: String,
}

impl ValidationWarning {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: path.into(),
        }
    }
}

/// Retired config surfaces: dotted section path + stable warning code.
/// Single source of truth for both compatibility shims — the env-override
/// tombstone (`env_overrides.rs`) derives its env-form prefix from the
/// dotted path, and `retired_section_tombstones` matches the path against
/// config-file contents. Each hit is ignored (never applied / silently
/// dropped by serde as an unknown section) and reported as a structured
/// warning so existing deployments keep parsing instead of failing config
/// load after the backing schema was deleted.
///
/// Sunset: these tombstones are compatibility shims; they will be removed
/// in a later announced window, after which the env paths hard-error like
/// any other unknown path and the file sections parse-drop silently.
pub const RETIRED_CONFIG_SURFACES: &[(&str, &str)] = &[
    ("eval", "eval_harness_moved_to_dev_tool"),
    (
        "gateway.pairing_dashboard",
        "gateway_pairing_dashboard_removed",
    ),
    ("knowledge", "knowledge_graph_removed"),
    ("knowledge_bundles", "knowledge_bundles_removed"),
    ("delegate", "delegate_config_removed"),
    ("claude_code", "raw_launcher_config_removed"),
    ("claude_code_runner", "raw_launcher_config_removed"),
    ("codex_cli", "raw_launcher_config_removed"),
    ("gemini_cli", "raw_launcher_config_removed"),
    ("opencode_cli", "raw_launcher_config_removed"),
    ("browser_delegate", "raw_launcher_config_removed"),
    ("subagents", "subagents_config_removed"),
    ("channels.linq", "inbound_webhook_channel_removed"),
    ("channels.slack", "retired_channel_transport_removed"),
    ("channels.mattermost", "retired_channel_transport_removed"),
    ("channels.imessage", "retired_channel_transport_removed"),
    ("channels.matrix", "retired_channel_transport_removed"),
    ("channels.signal", "retired_channel_transport_removed"),
    ("channels.irc", "retired_channel_transport_removed"),
    ("channels.twitch", "retired_channel_transport_removed"),
    ("channels.lark", "retired_channel_transport_removed"),
    ("channels.line", "retired_channel_transport_removed"),
    ("channels.dingtalk", "retired_channel_transport_removed"),
    ("channels.wecom", "retired_channel_transport_removed"),
    ("channels.wecom_ws", "retired_channel_transport_removed"),
    ("channels.wechat", "retired_channel_transport_removed"),
    ("channels.qq", "retired_channel_transport_removed"),
    ("channels.twitter", "retired_channel_transport_removed"),
    ("channels.mochat", "retired_channel_transport_removed"),
    ("channels.nostr", "retired_channel_transport_removed"),
    ("channels.clawdtalk", "retired_channel_transport_removed"),
    ("channels.reddit", "retired_channel_transport_removed"),
    ("channels.bluesky", "retired_channel_transport_removed"),
    ("channels.git", "retired_channel_transport_removed"),
    ("channels.voice_call", "retired_channel_transport_removed"),
    ("channels.voice_wake", "retired_channel_transport_removed"),
    ("channels.amqp", "retired_channel_transport_removed"),
    ("channels.whatsapp", "retired_channel_transport_removed"),
    ("channels.feishu", "retired_channel_transport_removed"),
    ("channels_config.slack", "retired_channel_transport_removed"),
    (
        "channels_config.mattermost",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.imessage",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.matrix",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.signal",
        "retired_channel_transport_removed",
    ),
    ("channels_config.irc", "retired_channel_transport_removed"),
    (
        "channels_config.twitch",
        "retired_channel_transport_removed",
    ),
    ("channels_config.lark", "retired_channel_transport_removed"),
    ("channels_config.nostr", "retired_channel_transport_removed"),
    (
        "channels_config.clawdtalk",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.reddit",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.bluesky",
        "retired_channel_transport_removed",
    ),
    ("channels_config.git", "retired_channel_transport_removed"),
    (
        "channels_config.voice_call",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.voice_wake",
        "retired_channel_transport_removed",
    ),
    ("channels_config.amqp", "retired_channel_transport_removed"),
    ("channels_config.line", "retired_channel_transport_removed"),
    (
        "channels_config.dingtalk",
        "retired_channel_transport_removed",
    ),
    ("channels_config.wecom", "retired_channel_transport_removed"),
    (
        "channels_config.wecom_ws",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.wechat",
        "retired_channel_transport_removed",
    ),
    ("channels_config.qq", "retired_channel_transport_removed"),
    (
        "channels_config.twitter",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.mochat",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.whatsapp",
        "retired_channel_transport_removed",
    ),
    (
        "channels_config.feishu",
        "retired_channel_transport_removed",
    ),
    ("channels.wati", "inbound_webhook_channel_removed"),
    ("channels.nextcloud_talk", "inbound_webhook_channel_removed"),
    ("channels.gmail_push", "inbound_webhook_channel_removed"),
    ("wss", "wss_transport_removed"),
    ("a2a", "a2a_server_removed"),
    ("todotracker", "todo_tracker_removed"),
    ("conversational_ai", "conversational_ai_removed"),
    ("plugins", "wasm_plugins_removed"),
    ("hardware", "hardware_moved_out_of_core"),
    ("peripherals", "hardware_moved_out_of_core"),
    ("jira", "saas_integration_removed"),
    ("linkedin", "saas_integration_removed"),
    ("microsoft365", "saas_integration_removed"),
    ("google_workspace", "saas_integration_removed"),
    ("cloud_ops", "saas_integration_removed"),
    ("project_intel", "saas_integration_removed"),
    ("composio", "saas_integration_removed"),
    ("notion", "saas_integration_removed"),
    ("storage.postgres", "memory_backend_removed"),
    ("storage.qdrant", "memory_backend_removed"),
];

/// Memory backend names that were removed. The memory factory classifies
/// them as retired and fails construction explicitly; the config value
/// tombstones below report them at load.
pub const RETIRED_MEMORY_BACKENDS: &[&str] = &["postgres", "qdrant"];

/// True when `channel_type` names a channel whose `[channels.<type>]`
/// section is registered in [`RETIRED_CONFIG_SURFACES`]. Load-time
/// validation uses this to tolerate leftover `agents.<a>.channels` entries
/// and `peer_groups.<g>.channel` references to a retired channel type: the
/// section tombstone already reports the retirement, and the reference has
/// nothing left to resolve against, so it must not fail config load.
/// Legacy Feishu sections migrated into the `lark` type in V3.
#[must_use]
pub fn is_retired_channel_type(channel_type: &str) -> bool {
    channel_type != "feishu"
        && RETIRED_CONFIG_SURFACES
            .iter()
            .any(|(path, _)| path.strip_prefix("channels.") == Some(channel_type))
}

/// Retired config FIELDS: dotted path (one `*` wildcard segment allowed for
/// map-keyed aliases, e.g. `[agents.<alias>]`) + stable warning code. Same
/// contract as `RETIRED_CONFIG_SURFACES`, one level deeper: the field was
/// deleted from its struct, serde silently drops the unknown key, and this
/// checker makes the retirement visible instead of a silent no-op.
pub const RETIRED_CONFIG_FIELDS: &[(&str, &str)] = &[
    (
        "runtime_profiles.*.auto_classify.cost_optimized_hint",
        "cost_optimized_routing_removed",
    ),
    ("agents.*.knowledge_bundles", "knowledge_bundles_removed"),
    (
        "providers.models.ollama.*.num_ctx",
        "ollama_native_options_removed",
    ),
    (
        "providers.models.ollama.*.num_predict",
        "ollama_native_options_removed",
    ),
    (
        "providers.models.ollama.*.temperature_override",
        "ollama_native_options_removed",
    ),
    (
        "transcription.transcribe_non_ptt_audio",
        "whatsapp_audio_option_removed",
    ),
    ("agents.*.delegates", "delegate_config_removed"),
    ("agents.*.a2a", "a2a_server_removed"),
    (
        "agents.*.delegate_same_risk_profile",
        "delegate_config_removed",
    ),
    (
        "risk_profiles.*.delegation_policy",
        "delegate_config_removed",
    ),
    (
        "runtime_profiles.*.delegation_timeout_secs",
        "delegate_config_removed",
    ),
    (
        "runtime_profiles.*.max_delegation_depth",
        "max_delegation_depth_removed",
    ),
    (
        "runtime_profiles.*.agentic_timeout_secs",
        "delegate_config_removed",
    ),
    ("observability.otel_endpoint", "otel_observability_removed"),
    (
        "observability.otel_service_name",
        "otel_observability_removed",
    ),
    ("observability.otel_headers", "otel_observability_removed"),
    (
        "observability.otel_genai_content",
        "otel_observability_removed",
    ),
    (
        "observability.otel_genai_content_max_chars",
        "otel_observability_removed",
    ),
    ("observability.otel_tool_io", "otel_observability_removed"),
    (
        "observability.otel_tool_io_max_chars",
        "otel_observability_removed",
    ),
    ("browser.native_headless", "browser_native_backend_removed"),
    (
        "browser.native_webdriver_url",
        "browser_native_backend_removed",
    ),
    (
        "browser.native_chrome_path",
        "browser_native_backend_removed",
    ),
];

/// Retired config VALUES: dotted path (one `*` wildcard segment allowed, as
/// in [`RETIRED_CONFIG_FIELDS`]) + the retired spellings (matched
/// case-insensitively) + stable warning code. Same contract one level
/// deeper again: the key is still valid, but a value that selected a removed
/// backend no longer resolves. Each entry says in its warning what the key
/// does instead; [`retired_field_tombstones`] reports these hits alongside
/// the retired fields, so every load path that already surfaces field
/// tombstones surfaces value tombstones too.
pub const RETIRED_CONFIG_VALUES: &[(&str, &[&str], &str)] = &[
    (
        "observability.backend",
        &["otel", "otlp", "opentelemetry"],
        "otel_observability_removed",
    ),
    (
        "memory.backend",
        RETIRED_MEMORY_BACKENDS,
        "memory_backend_removed",
    ),
    (
        "agents.*.memory.backend",
        RETIRED_MEMORY_BACKENDS,
        "memory_backend_removed",
    ),
    (
        "browser.backend",
        &["rust_native", "rust-native", "native"],
        "browser_native_backend_removed",
    ),
];

/// Why a retired field was removed, keyed by its stable warning code, for
/// the human-readable half of the field tombstone warning.
fn retired_field_reason(code: &str) -> &'static str {
    match code {
        "cost_optimized_routing_removed" => {
            "the unused cost-optimized selector was removed; this field never \
             selected a route. The simple, standard and complex hints and \
             ordinary named routes remain; prices do not select routes"
        }
        "ollama_native_options_removed" => {
            "native Ollama /api/chat options were removed; the current \
             OpenAI-compatible /v1 provider did not consume them. No automatic \
             mapping is applied; configure supported client or Ollama server \
             options explicitly"
        }
        "memory_backend_removed" => {
            "the postgres and qdrant memory backends were removed; selecting one \
             fails memory construction instead of falling back to another store. \
             Choose sqlite, lucid, markdown or none"
        }
        "browser_native_backend_removed" => {
            "the rust_native (WebDriver) browser backend was removed; selecting it \
             fails the browser tool. Use agent_browser, computer_use or auto"
        }
        "otel_observability_removed" => {
            "the OpenTelemetry exporter was removed; the `log`, `verbose` and \
             `prometheus` sinks remain, and a retired backend value loads as `none`"
        }
        "whatsapp_audio_option_removed" => {
            "the WhatsApp-only forwarded audio selector has no runtime consumer after the transport retired"
        }
        "a2a_server_removed" => {
            "the A2A discovery server and agent cards were never consumers in \
             this fork (external work goes through the Tachi bridge); the key is \
             ignored. Remove it"
        }
        _ => {
            "the legacy delegation key was removed from the schema with the retired \
             delegate tool and has no runtime consumer"
        }
    }
}

/// Structured tombstone warnings for retired fields still present in a
/// config file. Companion to [`retired_section_tombstones`]: same load-path
/// wiring, same warning shape, same sunset. A hit means the key was dropped
/// by serde as unknown while every other key in its section kept working —
/// the warning says so explicitly.
pub fn retired_field_tombstones(contents: &str) -> Vec<ValidationWarning> {
    let Ok(root) = toml::from_str::<toml::Value>(contents) else {
        return Vec::new();
    };
    let Some(root) = root.as_table() else {
        return Vec::new();
    };
    let mut warnings = Vec::new();
    for (path, code) in RETIRED_CONFIG_FIELDS {
        if !field_path_exists(root, path) {
            continue;
        }
        let reason = retired_field_reason(code);
        let warning = ValidationWarning::new(
            *code,
            format!("[{path}] in config.toml is ignored: {reason}. Remove the key."),
            (*path).to_string(),
        );
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "path": path, "code": code })),
            &warning.message
        );
        warnings.push(warning);
    }
    for (path, retired, code) in RETIRED_CONFIG_VALUES {
        let Some(value) = field_values(root, path)
            .into_iter()
            .find_map(|value| value.as_str().filter(|v| is_retired_value(v, retired)))
        else {
            continue;
        };
        let reason = retired_field_reason(code);
        let warning = ValidationWarning::new(
            *code,
            format!(
                "[{path}] = \"{value}\" in config.toml is no longer supported: {reason}. \
                 Change or remove the value."
            ),
            (*path).to_string(),
        );
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "path": path, "value": value, "code": code })),
            &warning.message
        );
        warnings.push(warning);
    }
    warnings
}

/// True when `value` names one of the `retired` spellings, case-insensitively.
/// A retired spelling also matches its aliased form (`<retired>.<alias>`), so
/// `postgres` covers `memory.backend = "postgres.work"`.
#[must_use]
pub fn is_retired_value(value: &str, retired: &[&str]) -> bool {
    let value = value.trim();
    let kind = value.split_once('.').map_or(value, |(kind, _)| kind);
    retired
        .iter()
        .any(|r| r.eq_ignore_ascii_case(value) || r.eq_ignore_ascii_case(kind))
}

/// Every value a dotted path with at most one `*` wildcard segment resolves
/// to in the parsed root (one per matching map key for a wildcard path).
fn field_values<'a>(root: &'a toml::value::Table, path: &str) -> Vec<&'a toml::Value> {
    let segments: Vec<&str> = path.split('.').collect();
    let lookup = |table: &'a toml::value::Table, segments: &[&str]| -> Option<&'a toml::Value> {
        let (&last, intermediates) = segments.split_last()?;
        value_at_table(table, intermediates)?.get(last)
    };
    match segments.iter().position(|segment| *segment == "*") {
        None => lookup(root, &segments).into_iter().collect(),
        Some(star) => {
            let Some(parent) = value_at_table(root, &segments[..star]) else {
                return Vec::new();
            };
            let suffix = &segments[star + 1..];
            parent
                .values()
                .filter_map(|value| lookup(value.as_table()?, suffix))
                .collect()
        }
    }
}

/// True when a dotted path with at most one `*` wildcard segment (matching a
/// map key) resolves to any value in the parsed root.
fn field_path_exists(root: &toml::value::Table, path: &str) -> bool {
    let segments: Vec<&str> = path.split('.').collect();
    match segments.iter().position(|segment| *segment == "*") {
        None => value_at_exists(root, &segments),
        Some(star) => {
            let Some(parent) = value_at_table(root, &segments[..star]) else {
                return false;
            };
            let suffix = &segments[star + 1..];
            parent.values().any(|value| match value.as_table() {
                Some(table) => value_at_exists(table, suffix),
                None => suffix.is_empty(),
            })
        }
    }
}

/// Resolve a wildcard-free segment list to a table, if present.
fn value_at_table<'a>(
    table: &'a toml::value::Table,
    segments: &[&str],
) -> Option<&'a toml::value::Table> {
    let mut current = table;
    for segment in segments {
        current = current.get(*segment)?.as_table()?;
    }
    Some(current)
}

/// True when a wildcard-free segment list resolves to a value. Only the
/// intermediate segments must be tables; the terminal segment may be any
/// value kind (the retired delegate keys are bools, arrays, and tables).
fn value_at_exists(mut current: &toml::value::Table, segments: &[&str]) -> bool {
    let Some((&last, intermediates)) = segments.split_last() else {
        return false;
    };
    for segment in intermediates {
        let Some(value) = current.get(*segment) else {
            return false;
        };
        match value.as_table() {
            Some(table) => current = table,
            // A non-table mid-path value cannot contain the terminal.
            None => return false,
        }
    }
    current.contains_key(last)
}

/// Structured tombstone warnings for retired sections still present in a
/// config file. `GatewayConfig`-style structs do not use
/// `deny_unknown_fields`, so serde silently drops an unknown nested
/// section and configs carrying a retired section keep parsing; this makes
/// the retirement visible instead of a silent no-op. Called by the
/// config-file LOAD paths (`Config::load_or_init`, the channels standalone
/// loader, and the gateway migrate handler); incidental single-key file
/// readers (e.g. the web-search Brave-key reload) never participated in
/// any config warning machinery and are out of this boundary.
///
/// Each hit also logs a WARN at detection time, symmetric with the env
/// tombstone's apply-time log: loaders that never call
/// `validate()`/`collect_warnings()` (the channels per-message reload)
/// still surface the retirement, and the warning is not gated behind
/// `validate()`'s replay loop (which an earlier validation error skips).
/// Ordering caveat, stated honestly: in `Config::load_or_init`, detection
/// runs before the `composition` hard-error gate, so a config with both
/// problems still warns before the load fails; on the channels loader,
/// strict migration runs first, so a file that fails strict migration
/// (e.g. invalid `composition`) errors before detection. A failed load
/// has its own error to fix; after the fix, the tombstone surfaces.
/// Successful loads additionally replay the structured warning through
/// `collect_warnings()` — the same documented dual emission as the env
/// tombstone.
pub fn retired_section_tombstones(contents: &str) -> Vec<ValidationWarning> {
    let Ok(root) = toml::from_str::<toml::Value>(contents) else {
        return Vec::new();
    };
    RETIRED_CONFIG_SURFACES
        .iter()
        .filter_map(|(path, code)| {
            let message = if *path == "eval" && table_path_exists(root.as_table(), path) {
                "[eval] in config.toml is no longer used: the evaluation harness moved to the development CLI. Pass suite_dir as --suite and mode as --mode; [runtime_profiles.<alias>.eval] is unchanged.".to_string()
            } else if table_path_exists(root.as_table(), path) {
                format!(
                    "[{path}] in config.toml is ignored: the section was removed from the \
                     schema and has no runtime consumer. This compatibility shim will be \
                     removed in a later announced window. Remove the section."
                )
            } else if matches!(*path, "channels.feishu" | "channels_config.feishu")
                && root.as_table().is_some_and(|table| {
                    value_at_exists(table, &path.split('.').collect::<Vec<_>>())
                })
            {
                // The old Feishu migration also reported malformed values.
                format!("{path} in config.toml is ignored: Feishu was removed from the schema. Remove the key.")
            } else {
                return None;
            };
            let warning = ValidationWarning::new(*code, message, *path);
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"path": path, "code": code})),
                &warning.message
            );
            Some(warning)
        })
        .collect()
}

/// True when a dotted path resolves to a table (section) in the parsed
/// root — a scalar planted where the section used to be is not a section
/// tombstone hit.
fn table_path_exists(root: Option<&toml::value::Table>, path: &str) -> bool {
    let mut current = root;
    for segment in path.split('.') {
        current = match current.and_then(|table| table.get(segment)) {
            Some(value) => value.as_table(),
            None => return false,
        };
    }
    current.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_field_tombstones_flags_field_under_wildcard_alias() {
        let contents = r#"
[agents.lead]
model_provider = "openai.default"
delegates = ["peer"]
delegate_same_risk_profile = true

[agents.lead.a2a]
published = true

[risk_profiles.shared]
delegation_policy = { mode = "allow" }
"#;
        let warnings = retired_field_tombstones(contents);
        let paths: Vec<_> = warnings.iter().map(|w| w.path.as_str()).collect();
        assert!(paths.contains(&"agents.*.delegates"), "{paths:?}");
        assert!(
            paths.contains(&"agents.*.delegate_same_risk_profile"),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"risk_profiles.*.delegation_policy"),
            "{paths:?}"
        );
        assert!(paths.contains(&"agents.*.a2a"), "{paths:?}");
        let a2a = warnings
            .iter()
            .find(|w| w.path == "agents.*.a2a")
            .expect("a2a tombstone");
        assert_eq!(a2a.code, "a2a_server_removed");
        assert!(
            a2a.message.contains("Tachi bridge"),
            "the a2a tombstone must state its own reason, not the delegation text: {}",
            a2a.message
        );
        // Untouched fields never warn.
        assert_eq!(warnings.len(), 4, "{paths:?}");
    }

    #[test]
    fn retired_value_tombstones_flag_retired_backend_only() {
        for value in ["otel", "OTLP", "opentelemetry"] {
            let warnings =
                retired_field_tombstones(&format!("[observability]\nbackend = \"{value}\"\n"));
            assert_eq!(warnings.len(), 1, "{value}: {warnings:?}");
            assert_eq!(warnings[0].code, "otel_observability_removed");
            assert_eq!(warnings[0].path, "observability.backend");
            assert!(warnings[0].message.contains(value), "{warnings:?}");
        }
        assert!(retired_field_tombstones("[observability]\nbackend = \"prometheus\"\n").is_empty());
        assert!(retired_field_tombstones("[observability]\nbackend = 3\n").is_empty());
    }

    #[test]
    fn retired_field_tombstones_silent_when_absent() {
        let contents = "[agents.lead]\nmodel_provider = \"openai.default\"\n";
        assert!(retired_field_tombstones(contents).is_empty());
    }

    #[test]
    fn retired_section_tombstones_flags_section_but_not_scalar_or_absent() {
        let with_section = "[gateway]\npairing_dashboard = { code_length = 8 }\n";
        let warnings = retired_section_tombstones(with_section);
        assert_eq!(warnings.len(), 1, "section hit must warn: {warnings:?}");
        assert_eq!(warnings[0].code, "gateway_pairing_dashboard_removed");
        assert_eq!(warnings[0].path, "gateway.pairing_dashboard");

        // A scalar planted where the section lived is not a section
        // tombstone hit — nothing to keep parsing, nothing to warn about.
        assert!(retired_section_tombstones("[gateway]\npairing_dashboard = 3\n").is_empty());
        assert!(retired_section_tombstones("default_temperature = 0.7\n").is_empty());
        assert!(retired_section_tombstones("not toml {{{").is_empty());

        let with_composio_and_notion =
            "[composio]\nenabled = true\n\n[notion]\nenabled = true\ndatabase_id = \"db\"\n";
        let warnings = retired_section_tombstones(with_composio_and_notion);
        assert_eq!(
            warnings.len(),
            2,
            "composio and notion sections must warn: {warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.path == "composio" && w.code == "saas_integration_removed"),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.path == "notion" && w.code == "saas_integration_removed"),
            "{warnings:?}"
        );
    }

    #[test]
    fn retired_section_tombstones_warn_for_every_table_entry() {
        // Discriminator for the wall 2 entries: the shared table is only as
        // good as the detector actually firing for each of its paths. Build
        // a minimal doc carrying every retired section and require exactly
        // one warning with that entry's code and path.
        for (path, code) in RETIRED_CONFIG_SURFACES {
            let segments: Vec<&str> = path.split('.').collect();
            let mut doc = String::new();
            for depth in 1..=segments.len() {
                doc.push_str(&format!("[{}]\n", segments[..depth].join(".")));
            }
            doc.push_str("legacy_key = true\n");
            let warnings = retired_section_tombstones(&doc);
            assert_eq!(warnings.len(), 1, "{path}: {warnings:?}");
            assert_eq!(warnings[0].code, *code);
            assert_eq!(warnings[0].path, *path);
        }
    }

    #[test]
    fn env_tombstone_prefix_is_derived_from_the_retired_surface_table() {
        // SSOT guard: the env-form prefixes used by the walker must be the
        // retired table's dotted paths translated to env form — if a future
        // surface joins the table, both shims pick it up without edits.
        for (path, _) in RETIRED_CONFIG_SURFACES {
            let env_prefix = format!("{}__", path.replace('.', "__"));
            assert!(
                crate::env_overrides::retired_env_tombstone_prefixes()
                    .iter()
                    .any(|(prefix, _)| *prefix == env_prefix),
                "env walker must carry the derived prefix for {env_prefix}"
            );
        }
    }
}
