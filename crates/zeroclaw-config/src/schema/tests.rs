/// The whole point of splitting the accessor: an operator-facing caller
/// must be able to tell "unconfigured" from a real 32,000, which a bare
/// `usize` cannot express.
#[::core::prelude::v1::test]
fn configured_context_window_reports_unset_distinctly_from_the_fallback() {
    let mut cfg = super::Config::default();
    cfg.providers
        .models
        .ensure("ollama", "local")
        .expect("known model provider type")
        .model = Some("qwen3".to_string());
    cfg.agents.insert(
        "coder".to_string(),
        super::AliasedAgentConfig {
            model_provider: "ollama.local".into(),
            ..Default::default()
        },
    );

    // A real referenced profile without a declaration is honestly
    // unknown, while budget arithmetic retains its historical operand.
    assert_eq!(cfg.configured_model_context_window("coder"), None);
    // Budget arithmetic still gets an operand, unchanged from before.
    assert_eq!(
        cfg.effective_model_context_window("coder"),
        super::UNCONFIGURED_CONTEXT_WINDOW_FALLBACK
    );

    // Explicitly configuring the same numeric value remains distinguishable
    // from the fallback.
    cfg.providers
        .models
        .ensure("ollama", "local")
        .expect("known model provider type")
        .context_window = Some(super::UNCONFIGURED_CONTEXT_WINDOW_FALLBACK);
    assert_eq!(
        cfg.configured_model_context_window("coder"),
        Some(super::UNCONFIGURED_CONTEXT_WINDOW_FALLBACK)
    );
}

#[::core::prelude::v1::test]
fn unconfigured_context_window_fallback_is_documented_stub_value() {
    // Pinned so a change to the stub is a deliberate, reviewed edit — the
    // value is load-bearing for trim budgets on unconfigured profiles.
    assert_eq!(super::UNCONFIGURED_CONTEXT_WINDOW_FALLBACK, 32_000);
}

#[::core::prelude::v1::test]
fn mcp_server_config_pinned_resources_defaults_empty_and_round_trips() {
    // Absent field defaults to empty.
    let cfg: McpServerConfig = serde_json::from_str(r#"{"name":"s","command":"x"}"#).unwrap();
    assert!(cfg.pinned_resources.is_empty());

    // Present field round-trips.
    let cfg: McpServerConfig = serde_json::from_str(
        r#"{"name":"s","command":"x","pinned_resources":["file:///a","file:///b"]}"#,
    )
    .unwrap();
    assert_eq!(cfg.pinned_resources, vec!["file:///a", "file:///b"]);
}

#[::core::prelude::v1::test]
fn tool_filter_group_legacy_filter_builtins_key_still_parses() {
    // `filter_builtins` was declared-but-never-read and is removed.
    // `ToolFilterGroup` has no `deny_unknown_fields`, so configs
    // still carrying the key must keep deserializing (silently ignored).
    let group: super::ToolFilterGroup = toml::from_str(
        r#"
        mode = "always"
        tools = ["filesystem__*"]
        filter_builtins = true
        "#,
    )
    .expect("legacy filter_builtins key must not break deserialization");
    assert!(matches!(group.mode, super::ToolFilterGroupMode::Always));
    assert_eq!(group.tools, vec!["filesystem__*".to_string()]);
}

#[::core::prelude::v1::test]
fn memory_config_rerank_stage_defaults() {
    // An empty [memory] block resolves the rerank-stage keys to their
    // inert defaults (stage off, "none" strategy).
    let cfg: super::MemoryConfig = serde_json::from_str("{}").unwrap();
    assert!(!cfg.rerank_enabled);
    assert_eq!(cfg.candidate_multiplier, 4);
    assert_eq!(cfg.rerank_threshold, 5);
    assert_eq!(cfg.rerank_strategy, "none");
    assert!((cfg.mmr_lambda - 0.7).abs() < f64::EPSILON);
    assert!((cfg.importance_weight - 0.2).abs() < f64::EPSILON);
    assert!((cfg.recency_weight - 0.1).abs() < f64::EPSILON);

    // The Default impl agrees with the serde defaults.
    let def = super::MemoryConfig::default();
    assert_eq!(def.candidate_multiplier, cfg.candidate_multiplier);
    assert_eq!(def.rerank_strategy, cfg.rerank_strategy);
    assert!((def.mmr_lambda - cfg.mmr_lambda).abs() < f64::EPSILON);
    assert!((def.importance_weight - cfg.importance_weight).abs() < f64::EPSILON);
    assert!((def.recency_weight - cfg.recency_weight).abs() < f64::EPSILON);
}

#[::core::prelude::v1::test]
fn config_validate_rejects_invalid_memory_rerank_values() {
    // A NaN in any of the blend/floor floats must be rejected outright: it
    // survives `clamp` and would silently drop valid memories downstream.
    let reject_nan = |field: &str| {
        let mut config = super::Config::default();
        match field {
            "memory.min_relevance_score" => config.memory.min_relevance_score = f64::NAN,
            "memory.mmr_lambda" => config.memory.mmr_lambda = f64::NAN,
            "memory.importance_weight" => config.memory.importance_weight = f64::NAN,
            "memory.recency_weight" => config.memory.recency_weight = f64::NAN,
            other => panic!("unhandled field {other}"),
        }
        let err = config
            .validate()
            .expect_err("non-finite value must fail validation");
        assert!(
            err.to_string().contains(field),
            "expected {field}, got {err}"
        );
    };
    reject_nan("memory.min_relevance_score");
    reject_nan("memory.mmr_lambda");
    reject_nan("memory.importance_weight");
    reject_nan("memory.recency_weight");

    for multiplier in [0, super::MAX_MEMORY_RERANK_CANDIDATE_MULTIPLIER + 1] {
        let mut config = super::Config::default();
        config.memory.candidate_multiplier = multiplier;
        let err = config
            .validate()
            .expect_err("out-of-range multiplier must fail validation");
        assert!(
            err.to_string().contains("memory.candidate_multiplier"),
            "unexpected error: {err}"
        );
    }

    let mut config = super::Config::default();
    config.memory.mmr_lambda = 1.1;
    let err = config
        .validate()
        .expect_err("out-of-range MMR lambda must fail validation");
    assert!(err.to_string().contains("memory.mmr_lambda"));
}

#[::core::prelude::v1::test]
fn skill_bundle_admits_skill_honors_include_and_exclude() {
    let mut bundle = super::SkillBundleConfig::default();
    assert!(bundle.admits_skill("anything"));

    bundle.include = vec!["widget".into()];
    assert!(bundle.admits_skill("widget"));
    assert!(!bundle.admits_skill("gadget"));

    bundle.exclude = vec!["widget".into()];
    assert!(!bundle.admits_skill("widget"));
}

#[::core::prelude::v1::test]
fn provider_cost_categories_match_rate_struct_fields() {
    let json = serde_json::to_value(super::ProviderCostRates::default()).unwrap();
    let mut fields: Vec<String> = json
        .as_object()
        .expect("ProviderCostRates serializes to a map")
        .keys()
        .cloned()
        .collect();
    fields.sort();
    let mut registry: Vec<String> = super::PROVIDER_COST_CATEGORIES
        .iter()
        .map(|s| s.to_string())
        .collect();
    registry.sort();
    assert_eq!(
        fields, registry,
        "PROVIDER_COST_CATEGORIES must list exactly the ProviderCostRates rate-sheet fields"
    );
}

#[::core::prelude::v1::test]
fn cost_category_resolves_only_rate_bearing_sections() {
    assert_eq!(
        super::cost_category_for_provider_section("providers.models"),
        Some("models")
    );
    assert_eq!(
        super::cost_category_for_provider_section("providers.tts"),
        Some("tts")
    );
    assert_eq!(
        super::cost_category_for_provider_section("providers.transcription"),
        Some("transcription")
    );
    assert_eq!(super::cost_category_for_provider_section("channels"), None);
    assert_eq!(
        super::cost_category_for_provider_section("providers.unknown"),
        None
    );
    assert_eq!(super::cost_category_for_provider_section("models"), None);
}

/// Retired MQTT/filesystem listener sections must fail config parsing with
/// an actionable message, including an explicitly empty section header.
#[test]
async fn retired_sop_run_config_keys_fail_parse_loudly() {
    let fs: Result<ChannelsConfig, _> = ::toml::from_str(
        r#"[filesystem.watch]
        enabled = true
        paths = ["/tmp/inbox"]
        "#,
    );
    let err = fs.unwrap_err().to_string();
    assert!(err.contains("retired SOP run-side channel"), "{err}");

    let mqtt: Result<ChannelsConfig, _> = ::toml::from_str(
        r#"[mqtt.sensors]
        enabled = true
        broker_url = "mqtt://localhost:1883"
        "#,
    );
    let err = mqtt.unwrap_err().to_string();
    assert!(err.contains("retired SOP run-side channel"), "{err}");

    // Even an explicitly EMPTY retired section header fails: the section
    // is retired, presence itself is the misconfiguration.
    for header in ["[filesystem]", "[mqtt]"] {
        let empty: Result<ChannelsConfig, _> = ::toml::from_str(header);
        let err = empty.unwrap_err().to_string();
        assert!(err.contains("retired SOP run-side channel"), "{err}");
    }
}

/// The retired run-side config keys must FAIL config parse
/// with an actionable message, never silently no-op.
use super::*;
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;
use tokio::sync::MutexGuard;
use tokio::test;

struct EnvValueGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvValueGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: tests that mutate env vars serialize on env_override_lock().
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: tests that mutate env vars serialize on env_override_lock().
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvValueGuard {
    fn drop(&mut self) {
        // SAFETY: tests that mutate env vars serialize on env_override_lock().
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[cfg(unix)]
fn write_fake_op(bin_dir: &Path, script: &str) -> PathBuf {
    let op_path = bin_dir.join("op");
    std::fs::write(&op_path, script).expect("write fake op");
    let mut perms = std::fs::metadata(&op_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&op_path, perms).unwrap();
    op_path
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "test.object_array.entries"]
struct ObjectArraySecretEntry {
    pub name: String,
    #[secret]
    pub token: Option<String>,
    #[secret]
    pub headers: HashMap<String, String>,
}

impl crate::config::HasPropKind for Vec<ObjectArraySecretEntry> {
    const PROP_KIND: crate::config::PropKind = crate::config::PropKind::ObjectArray;

    fn display_secret_terminals() -> Vec<&'static str> {
        ObjectArraySecretEntry::secret_field_terminals()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "test.object_array"]
struct ObjectArraySecretFixture {
    pub entries: Vec<ObjectArraySecretEntry>,
}

// ── Tilde expansion ───────────────────────────────────────

#[test]
async fn expand_tilde_path_handles_absolute_path() {
    let path = expand_tilde_path("/absolute/path");
    assert_eq!(path, PathBuf::from("/absolute/path"));
}

#[test]
async fn expand_tilde_path_handles_relative_path() {
    let path = expand_tilde_path("relative/path");
    assert_eq!(path, PathBuf::from("relative/path"));
}

#[test]
async fn expand_tilde_path_expands_tilde_when_home_set() {
    // This test verifies that tilde expansion works when HOME is set.
    // In normal environments, HOME is set, so ~ should expand.
    let path = expand_tilde_path("~/.zeroclaw");
    // The path should not literally start with '~' if HOME is set
    // (it should be expanded to the actual home directory)
    if std::env::var("HOME").is_ok() {
        assert!(
            !path.to_string_lossy().starts_with('~'),
            "Tilde should be expanded when HOME is set"
        );
    }
}

// ── Defaults ─────────────────────────────────────────────

fn has_test_table(raw: &str, table: &str) -> bool {
    let exact = format!("[{table}]");
    let nested = format!("[{table}.");
    raw.lines()
        .map(str::trim)
        .any(|line| line == exact || line.starts_with(&nested))
}

fn mcp_server(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        ..McpServerConfig::default()
    }
}

/// A config with three configured servers (`a`, `b`, `c`) plus the given
/// `[mcp_bundles.<alias>]` entries. Uses `push`/`insert` (not field
/// assignment) to avoid `clippy::field_reassign_with_default`.
fn config_with_mcp_bundles(bundles: Vec<(&str, McpBundleConfig)>) -> Config {
    let mut config = Config::default();
    config.mcp.servers.push(mcp_server("a"));
    config.mcp.servers.push(mcp_server("b"));
    config.mcp.servers.push(mcp_server("c"));
    for (alias, bundle) in bundles {
        config.mcp_bundles.insert(alias.to_string(), bundle);
    }
    config
}

#[test]
async fn sop_untrusted_payload_defaults_are_back_compat() {
    let config: SopConfig = toml::from_str("").expect("empty SOP config should deserialize");

    assert_eq!(config.untrusted_payload_max_bytes, 8192);
    assert_eq!(config.untrusted_input_guard, "warn");
    assert_eq!(config.untrusted_guard_sensitivity, 0.7);
    assert!(config.untrusted_frame_warning);
    assert!(config.untrusted_outbound_redact);
}

#[test]
async fn sop_untrusted_payload_config_overrides_deserialize() {
    let config: SopConfig = toml::from_str(
        r#"
untrusted_payload_max_bytes = 4096
untrusted_input_guard = "block"
untrusted_guard_sensitivity = 0.9
untrusted_frame_warning = false
untrusted_outbound_redact = false
"#,
    )
    .expect("SOP untrusted payload overrides should deserialize");

    assert_eq!(config.untrusted_payload_max_bytes, 4096);
    assert_eq!(config.untrusted_input_guard, "block");
    assert_eq!(config.untrusted_guard_sensitivity, 0.9);
    assert!(!config.untrusted_frame_warning);
    assert!(!config.untrusted_outbound_redact);
}

#[test]
async fn mcp_bundles_empty_grants_no_servers() {
    // Secure by default: omission is not a grant.
    let config = config_with_mcp_bundles(vec![]);
    assert!(config.mcp_servers_for_bundles(&[]).is_empty());
}

#[test]
async fn mcp_bundles_union_resolves_and_dedups() {
    let config = config_with_mcp_bundles(vec![
        (
            "x",
            McpBundleConfig {
                servers: vec!["a".into(), "b".into()],
                exclude: vec![],
            },
        ),
        (
            "y",
            McpBundleConfig {
                servers: vec!["b".into(), "c".into()],
                exclude: vec![],
            },
        ),
    ]);
    let names: Vec<String> = config
        .mcp_servers_for_bundles(&["x".to_string(), "y".to_string()])
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        vec!["a", "b", "c"],
        "union across bundles, first-seen order, deduplicated"
    );
}

#[test]
async fn mcp_bundles_exclude_wins_across_bundles() {
    // `b` is included by bundle `x` but excluded by bundle `y`; deny wins.
    let config = config_with_mcp_bundles(vec![
        (
            "x",
            McpBundleConfig {
                servers: vec!["a".into(), "b".into()],
                exclude: vec![],
            },
        ),
        (
            "y",
            McpBundleConfig {
                servers: vec!["c".into()],
                exclude: vec!["b".into()],
            },
        ),
    ]);
    let names: Vec<String> = config
        .mcp_servers_for_bundles(&["x".to_string(), "y".to_string()])
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        vec!["a", "c"],
        "an excluded server is denied even when another referenced bundle includes it"
    );
}

#[test]
async fn mcp_bundles_unknown_bundle_and_dangling_name_grant_nothing() {
    // An unknown bundle alias and a server name with no `[mcp.servers]`
    // entry both fail closed (grant nothing).
    let config = config_with_mcp_bundles(vec![(
        "x",
        McpBundleConfig {
            servers: vec!["a".into(), "ghost".into()],
            exclude: vec![],
        },
    )]);
    let names: Vec<String> = config
        .mcp_servers_for_bundles(&["x".to_string(), "missing".to_string()])
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        vec!["a"],
        "a dangling server name and an unknown bundle alias grant nothing"
    );
}

#[test]
async fn mcp_servers_for_agent_grants_only_via_agent_bundles() {
    let mut config = config_with_mcp_bundles(vec![(
        "aa",
        McpBundleConfig {
            servers: vec!["a".into()],
            exclude: vec![],
        },
    )]);
    config.agents.insert(
        "aaatools".to_string(),
        AliasedAgentConfig {
            mcp_bundles: vec!["aa".to_string()],
            ..AliasedAgentConfig::default()
        },
    );
    config
        .agents
        .insert("defzc".to_string(), AliasedAgentConfig::default());

    let granted: Vec<String> = config
        .mcp_servers_for_agent("aaatools")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(granted, vec!["a"], "agent is granted its bundle's servers");
    assert!(
        config.mcp_servers_for_agent("defzc").is_empty(),
        "an agent with no mcp_bundles is granted no MCP servers (omission is not a grant)"
    );
    assert!(
        config.mcp_servers_for_agent("ghost").is_empty(),
        "an unknown agent is granted no MCP servers"
    );
}

/// Regression test for the operator-UX warning added alongside:
/// when MCP is enabled and `[[mcp.servers]]` is non-empty but no
/// `[mcp_bundles.*]` exists, validate() must still succeed (warnings
/// are non-fatal) AND every agent must resolve to zero servers
/// (proving the secure-by-default semantics that motivate the warning
/// are still in force).
#[test]
async fn validate_warns_when_servers_configured_but_no_bundles() {
    use crate::schema::{McpServerConfig, McpTransport};
    let mut config = Config::default();
    config.mcp.enabled = true;
    config.mcp.servers = vec![McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    }];
    assert!(
        config.mcp_bundles.is_empty(),
        "test precondition: no bundles configured"
    );

    // validate() must succeed (warnings are non-fatal).
    assert!(config.validate().is_ok());

    // Behavioral assertion that motivates the warning: every agent
    // resolves to zero servers under these conditions.
    for alias in config.agents.keys() {
        assert!(
            config.mcp_servers_for_agent(alias).is_empty(),
            "every agent must get zero servers when no bundles exist"
        );
    }
}

/// Counterpart to `validate_warns_when_servers_configured_but_no_bundles`:
/// once at least one `[mcp_bundles.*]` exists, the warning's
/// precondition no longer holds. validate() still succeeds and the
/// granted agent resolves to its bundled server.
#[test]
async fn validate_does_not_warn_when_a_bundle_exists() {
    use crate::schema::{McpBundleConfig, McpServerConfig, McpTransport};
    let mut config = Config::default();
    config.mcp.enabled = true;
    config.mcp.servers = vec![McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    }];
    config.mcp_bundles.insert(
        "default".into(),
        McpBundleConfig {
            servers: vec!["fs".into()],
            exclude: vec![],
        },
    );

    assert!(config.validate().is_ok());
    // Precondition check: the warning's trigger condition is now false.
    assert!(!config.mcp_bundles.is_empty());
}

fn parse_test_config(raw: &str) -> Config {
    let mut merged = raw.trim().to_string();
    for table in ["data_retention", "security", "security_ops"] {
        if has_test_table(&merged, table) {
            continue;
        }
        if !merged.is_empty() {
            merged.push_str("\n\n");
        }
        merged.push('[');
        merged.push_str(table);
        merged.push(']');
    }
    merged.push('\n');
    // Schema-deserialization helper: parses TOML directly into Config
    // WITHOUT running migration transforms. Tests that need migration
    // behavior should use `migrate_to_current` directly. This helper
    // exists so V2-shaped inputs (e.g. flat `[autonomy]` blocks) can
    // be exercised against the typed deserializer without losing
    // sections that V2→V3 strips.
    let mut config: Config = toml::from_str(&merged).unwrap();
    config
        .risk_profiles
        .entry("default".to_string())
        .or_default()
        .ensure_default_auto_approve();
    config
}

#[test]
async fn http_request_config_default_has_correct_values() {
    let cfg = HttpRequestConfig::default();
    assert_eq!(cfg.timeout_secs, 30);
    assert_eq!(cfg.max_response_size, 1_000_000);
    assert!(cfg.enabled);
    assert_eq!(cfg.allowed_domains, vec!["*".to_string()]);
    assert!(!cfg.allow_private_hosts);
    assert!(cfg.allowed_private_hosts.is_empty());
    assert!(cfg.secrets.is_empty());
}

#[test]
async fn http_request_config_deserializes_allowed_private_hosts() {
    let c = parse_test_config(
        r#"
[http_request]
allowed_domains = ["example.com"]
allowed_private_hosts = ["localhost", "10.0.0.1"]
"#,
    );

    assert_eq!(
        c.http_request.allowed_private_hosts,
        vec!["localhost".to_string(), "10.0.0.1".to_string()]
    );
}

#[test]
async fn http_request_config_deserializes_auth_secrets() {
    let c = parse_test_config(
        r#"
[http_request.secrets]
api_token = "Bearer test-token"
"#,
    );

    assert_eq!(
        c.http_request.secrets.get("api_token").map(String::as_str),
        Some("Bearer test-token")
    );
}

#[test]
async fn http_request_auth_secret_names_are_validated() {
    let mut config = Config::default();
    config
        .http_request
        .secrets
        .insert("bad.name".to_string(), "Bearer test-token".to_string());

    let err = config.validate().expect_err("invalid secret name");
    assert!(
        err.to_string().contains("http_request.secrets.bad.name"),
        "validation error must name the bad auth secret path: {err}"
    );
}

#[test]
async fn config_default_has_sane_values() {
    let c = Config::default();
    // No model_provider configured by default — set during Quickstart.
    assert!(c.providers.models.is_empty());
    assert!(c.providers.models.iter_entries().next().is_none());
    assert!(!c.skills.open_skills_enabled);
    assert!(!c.skills.allow_scripts);
    assert!(!c.skills.install_suggestions.enabled);
    assert_eq!(
        c.skills.prompt_injection_mode,
        SkillsPromptInjectionMode::Full
    );
    assert!(c.data_dir.to_string_lossy().contains("data"));
    assert!(c.config_path.to_string_lossy().contains("config.toml"));
}

#[test]
async fn runtime_profile_prompt_injection_mode_overrides_global() {
    let mut config = Config::default();
    config.skills.prompt_injection_mode = SkillsPromptInjectionMode::Full;
    // A runtime profile that pins compact, and an agent pointing at it.
    config.runtime_profiles.insert(
        "compact_profile".to_string(),
        RuntimeProfileConfig {
            prompt_injection_mode: Some(SkillsPromptInjectionMode::Compact),
            ..RuntimeProfileConfig::default()
        },
    );
    // A runtime profile that leaves the mode unset (inherits global).
    config
        .runtime_profiles
        .insert("unset_profile".to_string(), RuntimeProfileConfig::default());
    config.agents.insert(
        "override".to_string(),
        AliasedAgentConfig {
            runtime_profile: "compact_profile".into(),
            ..AliasedAgentConfig::default()
        },
    );
    config.agents.insert(
        "unset".to_string(),
        AliasedAgentConfig {
            runtime_profile: "unset_profile".into(),
            ..AliasedAgentConfig::default()
        },
    );
    // An agent with no runtime profile inherits the global.
    config
        .agents
        .insert("inherit".to_string(), AliasedAgentConfig::default());

    // Profile override beats the global value.
    assert_eq!(
        config.effective_skills_prompt_mode("override"),
        SkillsPromptInjectionMode::Compact
    );
    // Profile present but mode unset → inherit the global value.
    assert_eq!(
        config.effective_skills_prompt_mode("unset"),
        SkillsPromptInjectionMode::Full
    );
    // No runtime profile → inherit the global value.
    assert_eq!(
        config.effective_skills_prompt_mode("inherit"),
        SkillsPromptInjectionMode::Full
    );
    // Unknown alias also falls back to the global value.
    assert_eq!(
        config.effective_skills_prompt_mode("missing"),
        SkillsPromptInjectionMode::Full
    );

    // Flipping the global moves only the inheriting/unset/unknown agents;
    // the profile override is unaffected.
    config.skills.prompt_injection_mode = SkillsPromptInjectionMode::Compact;
    assert_eq!(
        config.effective_skills_prompt_mode("unset"),
        SkillsPromptInjectionMode::Compact
    );
    assert_eq!(
        config.effective_skills_prompt_mode("inherit"),
        SkillsPromptInjectionMode::Compact
    );
    assert_eq!(
        config.effective_skills_prompt_mode("missing"),
        SkillsPromptInjectionMode::Compact
    );
    assert_eq!(
        config.effective_skills_prompt_mode("override"),
        SkillsPromptInjectionMode::Compact
    );
}

#[test]
async fn runtime_profile_prompt_injection_mode_deserializes() {
    // Absent → None, so a profile that omits the key inherits the global
    // mode (no migration for existing global-only configs).
    let inherited: RuntimeProfileConfig = toml::from_str("").unwrap();
    assert_eq!(inherited.prompt_injection_mode, None);

    // Explicit wire spellings parse to their variants.
    let compact: RuntimeProfileConfig =
        toml::from_str("prompt_injection_mode = \"compact\"").unwrap();
    assert_eq!(
        compact.prompt_injection_mode,
        Some(SkillsPromptInjectionMode::Compact)
    );
    let full: RuntimeProfileConfig = toml::from_str("prompt_injection_mode = \"full\"").unwrap();
    assert_eq!(
        full.prompt_injection_mode,
        Some(SkillsPromptInjectionMode::Full)
    );
}

#[test]
async fn resolved_agent_config_bakes_prompt_injection_mode_from_profile() {
    // The documented invariant: a runtime_profile knob must be threaded
    // through `resolved_agent_config` into `ResolvedRuntime`, consistent
    // with the `effective_*` helper.
    let raw = r#"
[skills]
prompt_injection_mode = "full"

[runtime_profiles.fast]
prompt_injection_mode = "compact"

[agents.default]
runtime_profile = "fast"

[agents.plain]
"#;
    let parsed = parse_test_config(raw);

    // Profiled agent: resolved value + effective helper both see compact.
    let resolved = parsed
        .resolved_agent_config("default")
        .expect("agent default resolves");
    assert_eq!(
        resolved.resolved.prompt_injection_mode,
        SkillsPromptInjectionMode::Compact
    );
    assert_eq!(
        parsed.effective_skills_prompt_mode("default"),
        SkillsPromptInjectionMode::Compact
    );

    // Profile-less agent: resolved value falls back to the global default.
    let plain = parsed
        .resolved_agent_config("plain")
        .expect("agent plain resolves");
    assert_eq!(
        plain.resolved.prompt_injection_mode,
        SkillsPromptInjectionMode::Full
    );
}

#[test]
async fn skills_install_suggestions_config_deserializes_enabled() {
    let c = parse_test_config(
        r#"
[skills.install_suggestions]
enabled = true
"#,
    );

    assert!(c.skills.install_suggestions.enabled);
}

#[test]
async fn skills_install_suggestions_config_accepts_hyphen_alias() {
    let c = parse_test_config(
        r#"
[skills.install-suggestions]
enabled = true
"#,
    );

    assert!(c.skills.install_suggestions.enabled);
}

fn capture_log_events() -> tokio::sync::broadcast::Receiver<serde_json::Value> {
    ::zeroclaw_log::try_install_capture_subscriber();
    ::zeroclaw_log::subscribe_or_install()
}

fn drain_captured(rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>) -> String {
    let mut buf = String::new();
    while let Ok(value) = rx.try_recv() {
        buf.push_str(&serde_json::to_string(&value).unwrap_or_default());
        buf.push('\n');
    }
    buf
}

#[test]
async fn config_dir_creation_error_mentions_openrc_and_path() {
    let msg = config_dir_creation_error(Path::new("/etc/zeroclaw"));
    assert!(msg.contains("/etc/zeroclaw"));
    assert!(msg.contains("OpenRC"));
    assert!(msg.contains("zeroclaw"));
}

#[test]
async fn config_schema_export_contains_expected_contract_shape() {
    #[cfg(feature = "schema-export")]
    let schema = schemars::schema_for!(Config);
    let schema_json = serde_json::to_value(&schema).expect("schema should serialize to json");

    assert_eq!(
        schema_json
            .get("$schema")
            .and_then(serde_json::Value::as_str),
        Some("https://json-schema.org/draft/2020-12/schema")
    );

    let properties = schema_json
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .expect("schema should expose top-level properties");

    assert!(properties.contains_key("providers"));
    assert!(properties.contains_key("skills"));
    assert!(properties.contains_key("gateway"));
    assert!(properties.contains_key("channels"));
    assert!(!properties.contains_key("workspace_dir"));
    assert!(!properties.contains_key("config_path"));
    assert!(!properties.contains_key("model_providers"));
    assert!(!properties.contains_key("tts_providers"));
    assert!(!properties.contains_key("transcription_providers"));
    // These fields are now #[serde(skip)] cache fields, not in schema.
    assert!(!properties.contains_key("default_model_provider"));
    assert!(!properties.contains_key("api_key"));
    assert!(!properties.contains_key("default_model"));

    assert!(
        schema_json
            .get("$defs")
            .and_then(serde_json::Value::as_object)
            .is_some(),
        "schema should include reusable type definitions"
    );
}

#[cfg(unix)]
#[test]
async fn save_sets_config_permissions_on_new_file() {
    let temp = TempDir::new().expect("temp dir");
    let config_path = temp.path().join("config.toml");
    let workspace_dir = temp.path().join("workspace");

    let config = Config {
        config_path: config_path.clone(),
        data_dir: workspace_dir,
        ..Default::default()
    };

    config.save().await.expect("save config");

    let mode = std::fs::metadata(&config_path)
        .expect("config metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
async fn validate_rejects_reply_min_interval_above_upper_bound() {
    let mut config = Config::default();
    let mut tg = TelegramConfig {
        bot_token: "tok".into(),
        ..Default::default()
    };
    tg.reply_min_interval_secs = REPLY_MIN_INTERVAL_MAX_SECS + 1;
    config.channels.telegram.insert("default".to_string(), tg);
    let err = config.validate().expect_err("over-bound must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("channels.telegram.default.reply_min_interval_secs"),
        "error must name the offending path; got: {msg}"
    );
}

fn ext_reg(name: &str, url: &str, kind: &str) -> ExternalRegistry {
    ExternalRegistry {
        name: name.to_string(),
        url: url.to_string(),
        kind: kind.to_string().into(),
        enabled: true,
    }
}

#[test]
async fn validate_accepts_git_extra_registry() {
    let mut config = Config::default();
    config.skills.extra_registries = vec![ext_reg("team", "https://github.com/acme/skills", "git")];
    assert!(config.validate().is_ok(), "valid git registry must pass");
}

#[test]
async fn validate_rejects_extra_registry_non_git_kind() {
    let mut config = Config::default();
    config.skills.extra_registries =
        vec![ext_reg("team", "https://github.com/acme/skills", "zip-api")];
    let err = config
        .validate()
        .expect_err("non-git kind must be rejected");
    assert!(err.to_string().contains("kind must be 'git'"), "got: {err}");
}

#[test]
async fn validate_rejects_extra_registry_duplicate_names() {
    let mut config = Config::default();
    config.skills.extra_registries = vec![
        ext_reg("team", "https://github.com/acme/a", "git"),
        ext_reg("team", "https://github.com/acme/b", "git"),
    ];
    let err = config
        .validate()
        .expect_err("duplicate names must be rejected");
    assert!(
        err.to_string().contains("duplicate name 'team'"),
        "got: {err}"
    );
}

#[test]
async fn validate_rejects_extra_registry_unaddressable_names() {
    for name in [
        "team.prod",
        "team prod",
        "team/prod",
        "..",
        " team",
        "team ",
        "Team",
        "teamProd",
    ] {
        let mut config = Config::default();
        config.skills.extra_registries =
            vec![ext_reg(name, "https://github.com/acme/skills", "git")];
        let err = config
            .validate()
            .expect_err("unaddressable extra-registry name must be rejected");
        assert!(
            err.to_string().contains("registry:<name>/<skill>"),
            "name {name:?} produced unexpected error: {err}"
        );
    }
}

#[test]
async fn validate_rejects_extra_registry_empty_name_or_url() {
    let mut config = Config::default();
    config.skills.extra_registries = vec![ext_reg("", "https://github.com/acme/a", "git")];
    assert!(config.validate().is_err(), "empty name must be rejected");

    let mut config = Config::default();
    config.skills.extra_registries = vec![ext_reg("team", "   ", "git")];
    assert!(config.validate().is_err(), "empty url must be rejected");
}

#[test]
async fn validate_rejects_extra_registry_bad_url_scheme() {
    let mut config = Config::default();
    config.skills.extra_registries = vec![ext_reg("team", "ftp://example.com/x", "git")];
    let err = config
        .validate()
        .expect_err("non-http(s)/file scheme must be rejected");
    assert!(err.to_string().contains("scheme"), "got: {err}");
}

#[test]
async fn validate_accepts_reply_min_interval_at_upper_bound() {
    let mut config = Config::default();
    let mut tg = TelegramConfig {
        bot_token: "tok".into(),
        ..Default::default()
    };
    tg.reply_min_interval_secs = REPLY_MIN_INTERVAL_MAX_SECS;
    config.channels.telegram.insert("default".to_string(), tg);
    config.validate().expect("documented upper bound must pass");
}

#[test]
async fn validate_rejects_reply_queue_depth_above_ceiling() {
    let mut config = Config::default();
    let mut tg = TelegramConfig {
        bot_token: "tok".into(),
        ..Default::default()
    };
    tg.reply_min_interval_secs = 1;
    tg.reply_queue_depth_max = REPLY_QUEUE_DEPTH_CEILING + 1;
    config.channels.telegram.insert("default".to_string(), tg);
    let err = config
        .validate()
        .expect_err("over-ceiling depth must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("channels.telegram.default.reply_queue_depth_max"),
        "error must name the offending path; got: {msg}"
    );
}

#[test]
async fn validate_accepts_reply_queue_depth_at_ceiling() {
    let mut config = Config::default();
    let mut tg = TelegramConfig {
        bot_token: "tok".into(),
        ..Default::default()
    };
    tg.reply_min_interval_secs = 1;
    tg.reply_queue_depth_max = REPLY_QUEUE_DEPTH_CEILING;
    config.channels.telegram.insert("default".to_string(), tg);
    config.validate().expect("documented ceiling must pass");
}

#[test]
async fn validate_accepts_reply_queue_depth_zero_meaning_default() {
    // depth=0 means "fall back to DEFAULT_REPLY_QUEUE_DEPTH at the
    // pacing-wrapper construction site." Validator must accept it.
    let mut config = Config::default();
    let mut tg = TelegramConfig {
        bot_token: "tok".into(),
        ..Default::default()
    };
    tg.reply_min_interval_secs = 1;
    tg.reply_queue_depth_max = 0;
    config.channels.telegram.insert("default".to_string(), tg);
    config
        .validate()
        .expect("zero depth means default; must pass");
}

#[test]
async fn telegram_api_base_url_default_uses_official_endpoint() {
    assert_eq!(
        TelegramConfig::default().api_base_url,
        "https://api.telegram.org"
    );
}

#[test]
async fn telegram_api_base_url_missing_toml_defaults_to_official_endpoint() {
    let parsed: TelegramConfig = toml::from_str(r#"bot_token = "tok""#).unwrap();

    assert_eq!(parsed.api_base_url, "https://api.telegram.org");
}

#[test]
async fn telegram_api_base_url_parses_custom_endpoint() {
    let parsed: TelegramConfig = toml::from_str(
        r#"
bot_token = "tok"
api_base_url = "http://127.0.0.1:8081"
"#,
    )
    .unwrap();

    assert_eq!(parsed.api_base_url, "http://127.0.0.1:8081");
}

#[test]
async fn validate_rejects_empty_telegram_api_base_url() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "default".to_string(),
        TelegramConfig {
            bot_token: "tok".into(),
            api_base_url: "   ".into(),
            ..Default::default()
        },
    );

    let err = config
        .validate()
        .expect_err("empty Telegram API base URL must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("channels.telegram.default.api_base_url"));
}

#[test]
async fn validate_rejects_malformed_telegram_api_base_url() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "default".to_string(),
        TelegramConfig {
            bot_token: "tok".into(),
            api_base_url: "not a url".into(),
            ..Default::default()
        },
    );

    let err = config
        .validate()
        .expect_err("malformed Telegram API base URL must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("channels.telegram.default.api_base_url"));
}

#[test]
async fn validate_rejects_enabled_telegram_without_bot_token() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "telegram".to_string(),
        TelegramConfig {
            enabled: true,
            bot_token: "   ".into(),
            ..Default::default()
        },
    );

    let err = config
        .validate()
        .expect_err("enabled Telegram channel must require a bot token");
    assert!(
        err.to_string()
            .contains("channels.telegram.telegram.bot_token")
    );
}

#[test]
async fn validate_allows_disabled_telegram_without_bot_token() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "telegram".to_string(),
        TelegramConfig {
            enabled: false,
            bot_token: "   ".into(),
            ..Default::default()
        },
    );

    config
        .validate()
        .expect("disabled Telegram channel may be staged without a bot token");
}

#[test]
async fn validate_rejects_enabled_telegram_with_unset_display_token() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "telegram".to_string(),
        TelegramConfig {
            enabled: true,
            bot_token: crate::traits::UNSET_DISPLAY.into(),
            ..Default::default()
        },
    );

    config
        .validate()
        .expect_err("enabled Telegram channel must reject the display sentinel");
}

#[test]
async fn validate_rejects_disabled_telegram_with_unset_display_token() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "telegram".to_string(),
        TelegramConfig {
            enabled: false,
            bot_token: crate::traits::UNSET_DISPLAY.into(),
            ..Default::default()
        },
    );

    config
        .validate()
        .expect_err("the unset display sentinel must never become persisted config");
}

#[test]
async fn validate_rejects_enabled_discord_without_bot_token() {
    let mut config = Config::default();
    config.channels.discord.insert(
        "discord".to_string(),
        DiscordConfig {
            enabled: true,
            bot_token: "   ".into(),
            ..Default::default()
        },
    );

    let err = config
        .validate()
        .expect_err("enabled Discord channel must require a bot token");
    assert!(
        err.to_string()
            .contains("channels.discord.discord.bot_token")
    );
}

#[test]
async fn validate_allows_disabled_discord_without_bot_token() {
    let mut config = Config::default();
    config.channels.discord.insert(
        "discord".to_string(),
        DiscordConfig {
            enabled: false,
            bot_token: "   ".into(),
            ..Default::default()
        },
    );

    config
        .validate()
        .expect("disabled Discord channel may be staged without a bot token");
}

#[test]
async fn validate_rejects_enabled_discord_with_unset_display_token() {
    let mut config = Config::default();
    config.channels.discord.insert(
        "discord".to_string(),
        DiscordConfig {
            enabled: true,
            bot_token: crate::traits::UNSET_DISPLAY.into(),
            ..Default::default()
        },
    );

    config
        .validate()
        .expect_err("enabled Discord channel must reject the display sentinel");
}

#[test]
async fn validate_rejects_disabled_discord_with_unset_display_token() {
    let mut config = Config::default();
    config.channels.discord.insert(
        "discord".to_string(),
        DiscordConfig {
            enabled: false,
            bot_token: crate::traits::UNSET_DISPLAY.into(),
            ..Default::default()
        },
    );

    config
        .validate()
        .expect_err("the unset display sentinel must never become persisted config");
}

// Regression (fail closed, both PAT-backed forge providers): a Gitea or
// Forgejo alias with an access token but no api_base_url must be rejected
// at config-validation time. The old behavior silently defaulted to
// https://gitea.com/api/v1 and sent the configured token there.
// The requirement is scoped to enabled PAT-backed providers: the GitHub
// provider has no api_base_url, and a disabled half-filled Gitea block
// must not fail the whole config.
#[test]
async fn observability_enums_deserialize_legacy_string_values() {
    // Backward compat: TOML configs written before the enum conversion
    // stored these as bare strings. They must still parse.
    let toml = r#"
backend = "prometheus"
log_persistence = "full"
log_tool_io = "off"
"#;
    let o: ObservabilityConfig = toml::from_str(toml).unwrap();
    assert_eq!(o.backend, ObservabilityBackend::Prometheus);
    assert_eq!(o.log_persistence, LogPersistence::Full);
    assert_eq!(o.log_tool_io, LogToolIo::Off);

    // Legacy alias key `runtime_trace_mode` still maps to log_persistence.
    let aliased: ObservabilityConfig = toml::from_str("runtime_trace_mode = \"none\"").unwrap();
    assert_eq!(aliased.log_persistence, LogPersistence::None);

    // Round-trip: serialize back to the same wire strings the runtime
    // boundary (`to_log_config`) and downstream `from_raw` expect.
    assert_eq!(ObservabilityBackend::Prometheus.as_wire(), "prometheus");
    assert_eq!(LogPersistence::Full.as_wire(), "full");
    assert_eq!(LogPersistence::Rotating.as_wire(), "rotating");
    assert_eq!(LogToolIo::Off.as_wire(), "off");

    // The `rotating` mode and its knobs parse from a TOML config.
    let rot: ObservabilityConfig = toml::from_str(
        "log_persistence = \"rotating\"\nlog_persistence_max_bytes = 1048576\nlog_persistence_rotate_daily = false\nlog_persistence_retention_max_files = 3\nlog_persistence_retention_max_age_days = 14\n",
    )
    .unwrap();
    assert_eq!(rot.log_persistence, LogPersistence::Rotating);
    assert_eq!(rot.log_persistence_max_bytes, 1_048_576);
    assert!(!rot.log_persistence_rotate_daily);
    assert_eq!(rot.log_persistence_retention_max_files, 3);
    assert_eq!(rot.log_persistence_retention_max_age_days, 14);

    // Rotation knobs are serde-defaulted, so existing configs that omit them
    // still parse and pick up the documented defaults.
    let defaults: ObservabilityConfig = toml::from_str("backend = \"none\"").unwrap();
    assert_eq!(defaults.log_persistence_max_bytes, 0);
    assert!(defaults.log_persistence_rotate_daily);
    assert_eq!(defaults.log_persistence_retention_max_files, 7);
    assert_eq!(defaults.log_persistence_retention_max_age_days, 0);

    // log_llm_request_payload parses leniently and round-trips its wire form.
    let payload: ObservabilityConfig =
        toml::from_str("log_llm_request_payload = \"full\"").unwrap();
    assert_eq!(payload.log_llm_request_payload, LogLlmRequestPayload::Full);
    assert_eq!(LogLlmRequestPayload::Off.as_wire(), "off");
}

#[test]
async fn observability_backend_unknown_falls_back_to_default() {
    let parsed: ObservabilityConfig = toml::from_str("backend = \"bogus\"").unwrap();
    assert_eq!(parsed.backend, ObservabilityBackend::None);
    let noop: ObservabilityConfig = toml::from_str("backend = \"noop\"").unwrap();
    assert_eq!(noop.backend, ObservabilityBackend::None);
    // The retired OTel backend values load as the default sink; the
    // retired-value tombstone reports them.
    for retired in ["otel", "otlp", "opentelemetry"] {
        let parsed: ObservabilityConfig =
            toml::from_str(&format!("backend = \"{retired}\"")).unwrap();
        assert_eq!(parsed.backend, ObservabilityBackend::None);
    }
}

#[test]
async fn runtime_kind_unknown_falls_back_to_default() {
    let docker: RuntimeConfig = toml::from_str("kind = \"docker\"").unwrap();
    assert_eq!(docker.kind, RuntimeKind::Docker);
    let cf: RuntimeConfig = toml::from_str("kind = \"cloudflare\"").unwrap();
    assert_eq!(cf.kind, RuntimeKind::Cloudflare);
    let bogus: RuntimeConfig = toml::from_str("kind = \"bogus\"").unwrap();
    assert_eq!(bogus.kind, RuntimeKind::Native);
    let empty: RuntimeConfig = toml::from_str("kind = \"\"").unwrap();
    assert_eq!(empty.kind, RuntimeKind::Native);
    assert_eq!(RuntimeKind::default(), RuntimeKind::Native);
}

#[test]
async fn observability_config_default() {
    let o = ObservabilityConfig::default();
    assert_eq!(o.backend, ObservabilityBackend::None);
    assert_eq!(o.log_persistence, LogPersistence::Rolling);
    assert_eq!(o.log_persistence_path, "state/runtime-trace.jsonl");
    assert_eq!(o.log_persistence_max_entries, 200);
    assert_eq!(o.log_tool_io, LogToolIo::Redacted);
    assert_eq!(o.log_tool_io_truncate_bytes, 40960);
    assert!(o.log_tool_io_denylist.is_empty());
    assert_eq!(o.log_llm_request_payload, LogLlmRequestPayload::Off);
}

#[test]
async fn risk_profile_default_mirrors_v2_autonomy_safety_defaults() {
    let a = RiskProfileConfig::default();
    assert_eq!(a.level, AutonomyLevel::Supervised);
    assert!(a.workspace_only);
    assert!(a.allowed_commands.contains(&"git".to_string()));
    assert!(a.allowed_commands.contains(&"cargo".to_string()));
    assert!(
        !a.forbidden_paths.is_empty(),
        "default forbidden_paths must not be empty"
    );
    #[cfg(not(target_os = "windows"))]
    assert!(
        a.forbidden_paths.iter().any(|p| p == "/etc"),
        "Default forbidden_paths must include /etc on Unix"
    );
    #[cfg(target_os = "windows")]
    assert!(
        a.forbidden_paths.iter().any(|p| p == "C:\\Windows"),
        "Default forbidden_paths must include C:\\Windows on Windows"
    );
    assert!(
        a.forbidden_paths.contains(&"~/.ssh".to_string()),
        "Default forbidden_paths must include ~/.ssh"
    );
    assert!(a.require_approval_for_medium_risk);
    assert!(a.block_high_risk_commands);
    assert!(a.shell_env_passthrough.is_empty());
    assert!(a.allowed_tools.is_none());
}

#[test]
async fn runtime_config_default() {
    let r = RuntimeConfig::default();
    assert_eq!(r.kind, RuntimeKind::Native);
    assert_eq!(r.docker.image, "alpine:3.20");
    assert_eq!(r.docker.network, "none");
    assert_eq!(r.docker.memory_limit_mb, Some(512));
    assert_eq!(r.docker.cpu_limit, Some(1.0));
    assert!(r.docker.read_only_rootfs);
    assert!(r.docker.mount_workspace);
}

#[test]
async fn heartbeat_config_parses_delivery_aliases() {
    let raw = r#"
enabled = true
interval_minutes = 10
message = "Ping"
channel = "telegram"
recipient = "42"
"#;
    let parsed: HeartbeatConfig = toml::from_str(raw).unwrap();
    assert!(parsed.enabled);
    assert_eq!(parsed.interval_minutes, 10);
    assert_eq!(parsed.message.as_deref(), Some("Ping"));
    assert_eq!(parsed.target.as_deref(), Some("telegram"));
    assert_eq!(parsed.to.as_deref(), Some("42"));
}

#[test]
async fn config_defaults_scheduler_when_section_missing() {
    let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;

    let parsed = parse_test_config(toml_str);
    assert!(parsed.scheduler.enabled);
    assert!(parsed.scheduler.catch_up_on_startup);
    assert_eq!(parsed.scheduler.max_run_history, 50);
    assert!(parsed.cron.is_empty());
}

#[test]
async fn memory_config_default_hygiene_settings() {
    let m = MemoryConfig::default();
    assert_eq!(m.backend, "sqlite");
    assert!(m.auto_save);
    assert!(m.hygiene_enabled);
    assert_eq!(m.archive_after_days, 7);
    assert_eq!(m.purge_after_days, 30);
    assert_eq!(m.conversation_retention_days, 30);
    assert_eq!(m.search_mode, SearchMode::Hybrid);
}

#[test]
async fn memory_config_without_types_keys_deserializes_off() {
    // Back-compat: configs written before [memory.types] and
    // consolidation_extract_facts existed must still parse, with both off.
    let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
"#;
    let parsed = parse_test_config(toml_str);
    assert!(!parsed.memory.consolidation_extract_facts);
    assert!(!parsed.memory.types.enabled);
}

#[test]
async fn memory_types_keys_parse_when_set() {
    let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
consolidation_extract_facts = true

[memory.types]
enabled = true
"#;
    let parsed = parse_test_config(toml_str);
    assert!(parsed.memory.consolidation_extract_facts);
    assert!(parsed.memory.types.enabled);
}

#[test]
async fn search_mode_config_deserialization() {
    let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
search_mode = "bm25"
"#;
    let parsed = parse_test_config(toml_str);
    assert_eq!(parsed.memory.search_mode, SearchMode::Bm25);

    let toml_str_embedding = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
search_mode = "embedding"
"#;
    let parsed = parse_test_config(toml_str_embedding);
    assert_eq!(parsed.memory.search_mode, SearchMode::Embedding);

    let toml_str_hybrid = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
search_mode = "hybrid"
"#;
    let parsed = parse_test_config(toml_str_hybrid);
    assert_eq!(parsed.memory.search_mode, SearchMode::Hybrid);
}

#[test]
async fn search_mode_serde_roundtrip() {
    let json_bm25 = serde_json::to_string(&SearchMode::Bm25).unwrap();
    assert_eq!(json_bm25, "\"bm25\"");
    let parsed: SearchMode = serde_json::from_str(&json_bm25).unwrap();
    assert_eq!(parsed, SearchMode::Bm25);

    let json_embedding = serde_json::to_string(&SearchMode::Embedding).unwrap();
    assert_eq!(json_embedding, "\"embedding\"");
    let parsed: SearchMode = serde_json::from_str(&json_embedding).unwrap();
    assert_eq!(parsed, SearchMode::Embedding);

    let json_hybrid = serde_json::to_string(&SearchMode::Hybrid).unwrap();
    assert_eq!(json_hybrid, "\"hybrid\"");
    let parsed: SearchMode = serde_json::from_str(&json_hybrid).unwrap();
    assert_eq!(parsed, SearchMode::Hybrid);
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_warns_on_retired_ollama_options_and_keeps_live_fields() {
    let _env_guard = env_override_lock().await;
    let home = TempDir::new().unwrap();
    let workspace = home.path().join("profile-a");
    fs::create_dir_all(&workspace).await.unwrap();
    let config_path = workspace.join("config.toml");
    let contents = r#"schema_version = 3

[providers.models.ollama.local]
model = "qwen3"
uri = "http://localhost:11434"
api_key = "ollama-fixture-key"
max_tokens = 2048
temperature = 0.7
num_ctx = 16384
num_predict = 4096
temperature_override = 0.5

# A live alias may have the same spelling as a retired leaf.
[providers.models.ollama.num_ctx]
model = "llama3.1"

[risk_profiles.shared]
[runtime_profiles.default]
[agents.assistant]
model_provider = "ollama.local"
risk_profile = "shared"
runtime_profile = "default"
"#;
    fs::write(&config_path, contents).await.unwrap();
    let _home = EnvValueGuard::set("HOME", home.path());
    let _workspace = EnvValueGuard::set("ZEROCLAW_WORKSPACE", &workspace);
    let _config_dir = EnvValueGuard::remove("ZEROCLAW_CONFIG_DIR");

    let mut config = Box::pin(Config::load_or_init()).await.unwrap();
    assert!(config.degraded_sections.is_empty());
    assert!(config.degraded_security.is_empty());
    let local = config.providers.models.find("ollama", "local").unwrap();
    assert_eq!(local.model.as_deref(), Some("qwen3"));
    assert_eq!(local.uri.as_deref(), Some("http://localhost:11434"));
    assert_eq!(local.api_key.as_deref(), Some("ollama-fixture-key"));
    assert_eq!(local.max_tokens, Some(2048));
    assert_eq!(local.temperature, Some(0.7));
    assert!(config.providers.models.find("ollama", "num_ctx").is_some());

    let warnings = config.collect_warnings();
    let retired: Vec<_> = warnings
        .iter()
        .filter(|warning| warning.code == "ollama_native_options_removed")
        .collect();
    assert_eq!(retired.len(), 3);
    let fields = config.prop_fields();
    let serialized = toml::to_string(&config.providers.models.ollama["local"]).unwrap();
    let alias: toml::Value = toml::from_str(&serialized).unwrap();
    for key in ["num_ctx", "num_predict", "temperature_override"] {
        let path = format!("providers.models.ollama.local.{key}");
        assert!(retired.iter().any(|warning| {
            warning.path == format!("providers.models.ollama.*.{key}")
                && warning.message.contains("No automatic mapping")
        }));
        assert!(!fields.iter().any(|field| field.name == path));
        assert!(config.set_prop(&path, "1").is_err());
        assert!(alias.get(key).is_none());
    }
    assert_eq!(fs::read_to_string(config_path).await.unwrap(), contents);
}

#[cfg(feature = "schema-export")]
#[test]
async fn ollama_schema_exposes_live_options_without_native_tuning() {
    let schema = serde_json::to_value(schemars::schema_for!(OllamaModelProviderConfig)).unwrap();
    let properties = schema["properties"].as_object().unwrap();
    for key in ["model", "uri", "api_key", "max_tokens", "temperature"] {
        assert!(properties.contains_key(key), "missing live field {key}");
    }
    for key in ["num_ctx", "num_predict", "temperature_override"] {
        assert!(
            !properties.contains_key(key),
            "retired field {key} advertised"
        );
    }
}

#[test]
async fn channels_default() {
    let c = ChannelsConfig::default();
    assert!(c.cli);
    assert!(c.telegram.is_empty());
    assert!(c.discord.is_empty());
    assert!(!c.show_tool_calls);
    assert_eq!(
        c.max_concurrent_per_channel,
        default_channel_max_concurrent_per_channel()
    );
}

#[test]
async fn channels_max_concurrent_per_channel_defaults_and_round_trips() {
    let parsed: ChannelsConfig = toml::from_str("cli = true").unwrap();
    assert_eq!(
        parsed.max_concurrent_per_channel,
        default_channel_max_concurrent_per_channel()
    );

    let parsed: ChannelsConfig =
        toml::from_str("cli = true\nmax_concurrent_per_channel = 2").unwrap();
    assert_eq!(parsed.max_concurrent_per_channel, 2);

    let toml_str = toml::to_string_pretty(&parsed).unwrap();
    let reparsed: ChannelsConfig = toml::from_str(&toml_str).unwrap();
    assert_eq!(reparsed.max_concurrent_per_channel, 2);
}

#[test]
async fn validate_rejects_zero_channel_max_concurrent_per_channel() {
    let mut config = Config::default();
    config.channels.max_concurrent_per_channel = 0;

    let err = config
        .validate()
        .expect_err("zero channel concurrency budget must fail validate");
    assert!(
        err.to_string()
            .contains("channels.max_concurrent_per_channel must be greater than 0"),
        "got: {err}"
    );
}

// ── Serde round-trip ─────────────────────────────────────

#[test]
async fn config_toml_roundtrip() {
    let config = Config {
        composition: None,
        tasks: crate::tasks::TasksConfig::default(),
        degraded_security: Vec::new(),
        degraded_sections: Vec::new(),
        retired_surface_warnings: Vec::new(),
        loaded_from: None,
        schema_version: crate::migration::CURRENT_SCHEMA_VERSION,
        providers: {
            let mut p = crate::providers::Providers::default();
            p.models.openrouter.insert(
                "default".to_string(),
                OpenRouterModelProviderConfig {
                    base: ModelProviderConfig {
                        api_key: Some("sk-test-key".into()),
                        model: Some("gpt-4o".into()),
                        temperature: Some(0.5),
                        timeout_secs: Some(120),
                        ..Default::default()
                    },
                },
            );
            p
        },
        model_routes: Vec::new(),
        embedding_routes: Vec::new(),
        data_dir: PathBuf::from("/tmp/test/workspace"),
        config_path: PathBuf::from("/tmp/test/config.toml"),
        observability: ObservabilityConfig {
            backend: ObservabilityBackend::Log,
            ..ObservabilityConfig::default()
        },
        risk_profiles: {
            let mut m = HashMap::new();
            m.insert(
                "default".into(),
                RiskProfileConfig {
                    level: AutonomyLevel::Full,
                    workspace_only: false,
                    allowed_commands: vec!["docker".into()],
                    forbidden_paths: vec!["/secret".into()],
                    require_approval_for_medium_risk: false,
                    block_high_risk_commands: true,
                    shell_env_passthrough: vec!["DATABASE_URL".into()],
                    auto_approve: vec!["file_read".into()],
                    always_ask: vec![],
                    allowed_roots: vec![],
                    allowed_tools: None,
                    excluded_tools: vec![],
                    ..RiskProfileConfig::default()
                },
            );
            m
        },
        trust: crate::scattered_types::TrustConfig::default(),
        backup: BackupConfig::default(),
        data_retention: DataRetentionConfig::default(),
        security: SecurityConfig::default(),
        security_ops: SecurityOpsConfig::default(),
        runtime: RuntimeConfig {
            kind: RuntimeKind::Docker,
            ..RuntimeConfig::default()
        },
        reliability: ReliabilityConfig::default(),
        scheduler: SchedulerConfig::default(),
        skills: SkillsConfig::default(),
        pipeline: PipelineConfig::default(),
        query_classification: QueryClassificationConfig::default(),
        heartbeat: HeartbeatConfig {
            enabled: true,
            interval_minutes: 15,
            two_phase: true,
            message: Some("Check London time".into()),
            target: Some("telegram".into()),
            to: Some("123456".into()),
            ..HeartbeatConfig::default()
        },
        cron: HashMap::new(),
        acp: AcpConfig::default(),
        channels: ChannelsConfig {
            cli: true,
            telegram: HashMap::from([(
                "default".to_string(),
                TelegramConfig {
                    enabled: true,
                    bot_token: "123:ABC".into(),
                    api_base_url: default_telegram_api_base_url(),
                    stream_mode: StreamMode::default(),
                    draft_update_interval_ms: default_draft_update_interval_ms(),
                    debounce_ms: None,
                    interrupt_on_new_message: false,
                    mention_only: false,
                    ack_reactions: None,
                    proxy_url: None,
                    approval_timeout_secs: default_telegram_approval_timeout_secs(),
                    excluded_tools: vec![],
                    reply_min_interval_secs: 0,
                    reply_queue_depth_max: 0,
                },
            )]),
            discord: HashMap::new(),
            webhook: HashMap::new(),
            email: HashMap::new(),
            voice_duplex: HashMap::new(),
            mqtt: HashMap::new(),
            filesystem: HashMap::new(),
            message_timeout_secs: 300,
            max_concurrent_per_channel: default_channel_max_concurrent_per_channel(),
            ack_reactions: true,
            show_tool_calls: true,
            session_persistence: true,
            session_backend: default_session_backend(),
            session_ttl_hours: 0,
            debounce_ms: 0,
        },
        memory: MemoryConfig::default(),
        storage: StorageConfig::default(),
        tunnel: TunnelConfig::default(),
        gateway: GatewayConfig::default(),
        secrets: SecretsConfig::default(),
        browser: BrowserConfig::default(),
        http_request: HttpRequestConfig::default(),
        multimodal: MultimodalConfig::default(),
        media_pipeline: MediaPipelineConfig::default(),
        web_fetch: WebFetchConfig::default(),
        link_enricher: LinkEnricherConfig::default(),
        text_browser: TextBrowserConfig::default(),
        web_search: WebSearchConfig::default(),
        proxy: ProxyConfig::default(),
        pacing: PacingConfig::default(),
        cost: CostConfig::default(),
        agents: HashMap::new(),
        runtime_profiles: HashMap::new(),
        personas: HashMap::new(),
        cards: HashMap::new(),
        companion_memory: crate::companion::CompanionMemoryConfig::default(),
        skill_bundles: HashMap::new(),
        knowledge_bundles: HashMap::new(),
        mcp_bundles: HashMap::new(),
        peer_groups: HashMap::new(),
        hooks: HooksConfig::default(),
        transcription: TranscriptionConfig::default(),
        tts: TtsConfig::default(),
        mcp: McpConfig::default(),
        tachi: crate::tachi::TachiConfig::default(),
        nodes: NodesConfig::default(),
        onboard_state: OnboardStateConfig::default(),
        node_transport: NodeTransportConfig::default(),
        knowledge: KnowledgeConfig::default(),
        image_gen: ImageGenConfig::default(),
        file_upload: FileUploadConfig::default(),
        file_upload_bundle: FileUploadBundleConfig::default(),
        file_download: FileDownloadConfig::default(),
        locale: None,
        verifiable_intent: VerifiableIntentConfig::default(),
        sop: SopConfig::default(),
        shell_tool: ShellToolConfig::default(),
        escalation: EscalationConfig::default(),
        env_overridden_paths: EnvOverriddenPaths::new(),
        pre_override_snapshots: PreOverrideSnapshots::new(),
        onepassword_reference_snapshots: ExternalRefSnapshots::new(),
        dirty_paths: std::collections::HashSet::new(),
        dirty_map_entries: std::collections::HashSet::new(),
        explicit_prop_dirty_paths: std::collections::HashSet::new(),
        tracked_prop_identities: std::collections::HashMap::new(),
    };
    // ModelProvider fields are now resolved directly — no cache needed.

    let toml_str = toml::to_string_pretty(&config).unwrap();
    let parsed = parse_test_config(&toml_str);

    assert_eq!(parsed.providers.models.len(), config.providers.models.len());
    assert_eq!(parsed.observability.backend, ObservabilityBackend::Log);
    assert_eq!(
        parsed.observability.log_persistence,
        LogPersistence::Rolling
    );
    let default_profile = parsed.risk_profiles.get("default").unwrap();
    assert_eq!(default_profile.level, AutonomyLevel::Full);
    assert!(!default_profile.workspace_only);
    assert_eq!(parsed.runtime.kind, RuntimeKind::Docker);
    assert!(parsed.heartbeat.enabled);
    assert_eq!(parsed.heartbeat.interval_minutes, 15);
    assert_eq!(
        parsed.heartbeat.message.as_deref(),
        Some("Check London time")
    );
    assert_eq!(parsed.heartbeat.target.as_deref(), Some("telegram"));
    assert_eq!(parsed.heartbeat.to.as_deref(), Some("123456"));
    assert!(!parsed.channels.telegram.is_empty());
    assert_eq!(
        parsed.channels.telegram.get("default").unwrap().bot_token,
        "123:ABC"
    );
}

#[test]
async fn config_minimal_toml_uses_defaults() {
    let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
    let parsed = parse_test_config(minimal);
    assert!(
        parsed
            .providers
            .models
            .iter_entries()
            .next()
            .map(|(_, _, e)| e)
            .and_then(|e| e.api_key.as_deref())
            .is_none()
    );
    assert_eq!(parsed.observability.backend, ObservabilityBackend::None);
    assert_eq!(
        parsed.observability.log_persistence,
        LogPersistence::Rolling
    );
    // Migration synthesizes risk_profiles.default from the legacy
    // [autonomy] block; assert against the named entry rather than a
    // global "active" profile (no such concept exists).
    assert_eq!(
        parsed
            .risk_profiles
            .get("default")
            .expect("migration synthesized risk_profiles.default")
            .level,
        AutonomyLevel::Supervised
    );
    assert_eq!(parsed.runtime.kind, RuntimeKind::Native);
    // Heartbeat defaults to disabled.
    assert!(!parsed.heartbeat.enabled);
    assert!(parsed.channels.cli);
    assert!(parsed.memory.hygiene_enabled);
    assert_eq!(parsed.memory.archive_after_days, 7);
    assert_eq!(parsed.memory.purge_after_days, 30);
    assert_eq!(parsed.memory.conversation_retention_days, 30);
    // Temperature migrated onto the primary model_provider entry
    assert!(
        (parsed
            .providers
            .models
            .iter_entries()
            .next()
            .map(|(_, _, e)| e)
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.7)
            .abs()
            < f64::EPSILON
    );
    assert_eq!(
        parsed
            .providers
            .models
            .iter_entries()
            .next()
            .map(|(_, _, e)| e)
            .and_then(|e| e.timeout_secs)
            .unwrap_or(120),
        120
    );
}

/// `[autonomy]` migrates onto `[risk_profiles.default]` via the V2→V3
/// migration. The fields must round-trip without being silently dropped.
#[test]
async fn v2_autonomy_section_migrates_onto_risk_profiles_default() {
    let raw = r#"
schema_version = 2
default_temperature = 0.7

[autonomy]
level = "full"
max_actions_per_hour = 99
auto_approve = ["file_read", "memory_recall", "http_request"]
"#;
    let parsed = crate::migration::migrate_to_current(raw).unwrap();
    let profile = parsed
        .risk_profiles
        .get("default")
        .expect("default profile");
    assert_eq!(profile.level, AutonomyLevel::Full);
    assert!(profile.auto_approve.contains(&"http_request".to_string()));
    let runtime = parsed
        .runtime_profiles
        .get("default")
        .expect("default runtime profile");
    assert_eq!(runtime.max_actions_per_hour, 99);
}

/// Regression test for: when a user provides a custom auto_approve
/// list, the built-in defaults must still be present.
#[test]
async fn auto_approve_merges_user_entries_with_defaults() {
    let raw = r#"
default_temperature = 0.7

[risk_profiles.default]
auto_approve = ["my_custom_tool", "another_tool"]
"#;
    let parsed = parse_test_config(raw);
    let profile = parsed.risk_profiles.get("default").unwrap();
    assert!(profile.auto_approve.contains(&"my_custom_tool".to_string()));
    assert!(profile.auto_approve.contains(&"another_tool".to_string()));
    for default_tool in &[
        "file_read",
        "memory_recall",
        "glob_search",
        "content_search",
        "web_fetch",
    ] {
        assert!(
            profile.auto_approve.contains(&String::from(*default_tool)),
            "default tool '{default_tool}' must be present"
        );
    }
}

#[test]
async fn default_auto_approve_includes_tool_search() {
    let defaults = default_auto_approve();
    assert!(defaults.contains(&"tool_search".to_string()));
}

/// Regression test: empty auto_approve still gets defaults merged.
#[test]
async fn auto_approve_empty_list_gets_defaults() {
    let raw = r#"
default_temperature = 0.7

[risk_profiles.default]
auto_approve = []
"#;
    let parsed = parse_test_config(raw);
    let profile = parsed.risk_profiles.get("default").unwrap();
    for tool in &default_auto_approve() {
        assert!(
            profile.auto_approve.contains(tool),
            "default tool '{tool}' must be present"
        );
    }
}

/// When no risk_profiles section is provided, defaults are applied to the
/// synthesized "default" profile.
#[test]
async fn auto_approve_defaults_when_no_risk_profile_section() {
    let raw = r#"
default_temperature = 0.7
"#;
    let parsed = parse_test_config(raw);
    let profile = parsed.risk_profiles.get("default").unwrap();
    for tool in &default_auto_approve() {
        assert!(
            profile.auto_approve.contains(tool),
            "default tool '{tool}' must be present"
        );
    }
}

/// Duplicates are not introduced when ensure_default_auto_approve runs
/// on a list that already contains the defaults.
#[test]
async fn auto_approve_no_duplicates() {
    let raw = r#"
default_temperature = 0.7

[risk_profiles.default]
auto_approve = ["glob_search", "file_read"]
"#;
    let parsed = parse_test_config(raw);
    let profile = parsed.risk_profiles.get("default").unwrap();
    assert_eq!(
        profile
            .auto_approve
            .iter()
            .filter(|t| *t == "glob_search")
            .count(),
        1
    );
    assert_eq!(
        profile
            .auto_approve
            .iter()
            .filter(|t| *t == "file_read")
            .count(),
        1
    );
}

#[test]
async fn provider_timeout_secs_parses_from_toml() {
    // V1 top-level `provider_timeout_secs` is folded into the
    // synthesized model_provider entry's `timeout_secs`.
    let raw = r#"
default_temperature = 0.7
provider_timeout_secs = 300
"#;
    let parsed = crate::migration::migrate_to_current(raw).expect("migration succeeds");
    assert_eq!(
        parsed
            .providers
            .models
            .find("openrouter", "default")
            .and_then(|e| e.timeout_secs)
            .unwrap_or(120),
        300
    );
}

#[test]
async fn extra_headers_parses_from_toml() {
    // V1 top-level `[extra_headers]` is folded into the synthesized
    // default model_provider entry's `extra_headers` map.
    let raw = r#"
default_temperature = 0.7

[extra_headers]
User-Agent = "MyApp/1.0"
X-Title = "zeroclaw"
"#;
    let parsed = crate::migration::migrate_to_current(raw).expect("migration succeeds");
    let headers = &parsed
        .providers
        .models
        .find("openrouter", "default")
        .expect("synthesized openrouter.default model_provider")
        .extra_headers;
    assert_eq!(headers.len(), 2);
    assert_eq!(headers.get("User-Agent").unwrap(), "MyApp/1.0");
    assert_eq!(headers.get("X-Title").unwrap(), "zeroclaw");
}

#[test]
async fn extra_headers_defaults_to_empty() {
    let raw = r#"
default_temperature = 0.7
"#;
    let parsed = parse_test_config(raw);
    assert!(
        parsed
            .providers
            .models
            .iter_entries()
            .next()
            .map(|(_, _, e)| e.extra_headers.is_empty())
            .unwrap_or(true)
    );
}

#[test]
async fn storage_lucid_timeout_overrides_deserialize() {
    let raw = r#"
default_temperature = 0.7

[storage.lucid.default]
binary_path = "/opt/lucid/bin/lucid"
recall_timeout_ms = 5000
store_timeout_ms = 4000
"#;

    let parsed = parse_test_config(raw);
    let lucid = parsed
        .storage
        .lucid
        .get("default")
        .expect("lucid.default present");
    assert_eq!(lucid.binary_path.as_deref(), Some("/opt/lucid/bin/lucid"));
    assert_eq!(lucid.recall_timeout_ms, Some(5000));
    assert_eq!(lucid.store_timeout_ms, Some(4000));
}

#[test]
async fn validate_rejects_zero_lucid_timeouts_with_alias_qualified_paths() {
    for field in ["recall_timeout_ms", "store_timeout_ms"] {
        let raw = format!(
            r#"
default_temperature = 0.7

[storage.lucid.edge_arm]
{field} = 0
"#
        );
        let parsed = parse_test_config(&raw);
        let error = parsed
            .validate()
            .expect_err("zero Lucid timeout must fail validation");
        let expected_path = format!("storage.lucid.edge_arm.{field}");
        assert!(
            error.to_string().contains(&expected_path),
            "validation error must name {expected_path}: {error:#}"
        );
    }
}

#[test]
async fn validate_rejects_blank_lucid_binary_with_alias_qualified_path() {
    let raw = r#"
default_temperature = 0.7

[storage.lucid.edge_arm]
binary_path = "   "
"#;
    let parsed = parse_test_config(raw);
    let error = parsed
        .validate()
        .expect_err("blank Lucid binary path must fail validation");
    assert!(
        error
            .to_string()
            .contains("storage.lucid.edge_arm.binary_path"),
        "validation error must name the alias-qualified binary path: {error:#}"
    );
}

#[test]
async fn runtime_reasoning_enabled_deserializes() {
    let raw = r#"
default_temperature = 0.7

[runtime]
reasoning_enabled = false
"#;

    let parsed = parse_test_config(raw);
    assert_eq!(parsed.runtime.reasoning_enabled, Some(false));
}

#[test]
async fn runtime_reasoning_effort_deserializes() {
    let raw = r#"
default_temperature = 0.7

[runtime]
reasoning_effort = "HIGH"
"#;

    let parsed: Config = toml::from_str(raw).unwrap();
    assert_eq!(parsed.runtime.reasoning_effort.as_deref(), Some("high"));
}

#[test]
async fn runtime_reasoning_effort_rejects_invalid_values() {
    let raw = r#"
default_temperature = 0.7

[runtime]
reasoning_effort = "turbo"
"#;

    let error = toml::from_str::<Config>(raw).expect_err("invalid value should fail");
    assert!(error.to_string().contains("reasoning_effort"));
}

#[test]
async fn agent_config_defaults() {
    let cfg = AliasedAgentConfig::default();
    assert!(cfg.resolved.compact_context);
    assert_eq!(cfg.resolved.max_tool_iterations, 10);
    assert_eq!(cfg.resolved.max_history_messages, 50);
    assert!(!cfg.resolved.parallel_tools);
    assert_eq!(cfg.resolved.tool_dispatcher, "auto");
    assert!(!cfg.resolved.strict_tool_parsing);
    assert!(cfg.precheck.enabled);
    assert_eq!(cfg.precheck.timeout_secs, 5);
}

#[test]
async fn agent_precheck_config_parses_from_agent_block() {
    let raw = r#"
[agents.default]
model_provider = "custom.default"
risk_profile = "default"
runtime_profile = "default"

[agents.default.precheck]
enabled = false
timeout_secs = 12
"#;
    let parsed = parse_test_config(raw);
    let agent = parsed
        .agents
        .get("default")
        .expect("[agents.default] parses into agents map");
    assert!(!agent.precheck.enabled);
    assert_eq!(agent.precheck.timeout_secs, 12);
}

#[test]
async fn validate_rejects_zero_agent_precheck_timeout() {
    let raw = r#"
[agents.default]
model_provider = "custom.default"
risk_profile = "default"
runtime_profile = "default"

[agents.default.precheck]
timeout_secs = 0
"#;
    let parsed = parse_test_config(raw);
    let error = parsed
        .validate()
        .expect_err("zero precheck timeout must be rejected");
    assert!(
        error
            .to_string()
            .contains("agents.default.precheck.timeout_secs")
    );
}

#[test]
async fn agent_level_tunable_keys_are_inert() {
    let raw = r#"
default_temperature = 0.7
[agents.default]
compact_context = true
max_tool_iterations = 20
max_history_messages = 80
parallel_tools = true
tool_dispatcher = "xml"
strict_tool_parsing = true
"#;
    let parsed = parse_test_config(raw);
    let agent = parsed
        .agents
        .get("default")
        .expect("[agents.default] parses into agents map");
    assert_eq!(agent.resolved.max_tool_iterations, 10);
    assert_eq!(agent.resolved.tool_dispatcher, "auto");
    assert!(!agent.resolved.strict_tool_parsing);
}

#[test]
async fn runtime_profile_max_tool_iterations_is_honored() {
    // `[runtime_profiles.*].max_tool_iterations` must actually take
    // effect. It previously had no effect (the value had to be set on
    // `[agents.*]`); now agent-inline is inert and the profile is the
    // authoritative surface, so this guards the resolved value.
    let raw = r#"
[runtime_profiles.fast]
max_tool_iterations = 25

[agents.default]
runtime_profile = "fast"
"#;
    let parsed = parse_test_config(raw);
    assert_eq!(parsed.effective_max_tool_iterations("default"), 25);
}

#[test]
async fn runtime_profile_unset_max_tool_iterations_uses_default() {
    // A profile that does not set max_tool_iterations (sentinel 0) falls
    // back to the global default rather than 0.
    let raw = r#"
[runtime_profiles.fast]
max_history_messages = 80

[agents.default]
runtime_profile = "fast"
"#;
    let parsed = parse_test_config(raw);
    assert_eq!(parsed.effective_max_tool_iterations("default"), 10);
}

#[test]
async fn runtime_profile_structured_history_cap_scales_when_omitted() {
    let raw = r#"
[runtime_profiles.long_turn]
max_tool_iterations = 100

[agents.default]
runtime_profile = "long_turn"
"#;
    let parsed = parse_test_config(raw);
    assert_eq!(parsed.effective_max_history_messages("default"), 50);
    assert_eq!(
        parsed.effective_structured_max_history_messages("default"),
        202
    );
    let agent = parsed.resolved_agent_config("default").unwrap();
    assert_eq!(agent.resolved.max_history_messages, 50);
}

#[test]
async fn runtime_profile_history_cap_explicit_value_remains_authoritative() {
    let raw = r#"
[runtime_profiles.long_turn]
max_tool_iterations = 100
max_history_messages = 80

[agents.default]
runtime_profile = "long_turn"
"#;
    let parsed = parse_test_config(raw);
    assert_eq!(parsed.effective_max_history_messages("default"), 80);
    assert_eq!(
        parsed.effective_structured_max_history_messages("default"),
        80
    );
    let agent = parsed.resolved_agent_config("default").unwrap();
    assert_eq!(agent.resolved.max_history_messages, 80);
}

#[test]
async fn runtime_profile_history_cap_explicit_zero_remains_authoritative() {
    let raw = r#"
[runtime_profiles.long_turn]
max_tool_iterations = 100
max_history_messages = 0

[agents.default]
runtime_profile = "long_turn"
"#;
    let parsed = parse_test_config(raw);
    assert_eq!(parsed.effective_max_history_messages("default"), 0);
    assert_eq!(
        parsed.effective_structured_max_history_messages("default"),
        0
    );
    let agent = parsed.resolved_agent_config("default").unwrap();
    assert_eq!(agent.resolved.max_history_messages, 0);
}

#[test]
async fn runtime_profile_history_cap_saturates_at_usize_max() {
    let mut config = Config::default();
    config.runtime_profiles.insert(
        "long_turn".to_string(),
        RuntimeProfileConfig {
            max_tool_iterations: usize::MAX,
            ..RuntimeProfileConfig::default()
        },
    );
    config.agents.insert(
        "default".to_string(),
        AliasedAgentConfig {
            runtime_profile: "long_turn".into(),
            ..AliasedAgentConfig::default()
        },
    );

    assert_eq!(
        config.effective_structured_max_history_messages("default"),
        usize::MAX
    );
    assert_eq!(config.effective_max_history_messages("default"), 50);
}

#[test]
async fn default_runtime_profile_history_cap_remains_50() {
    let parsed = parse_test_config("");
    assert_eq!(parsed.effective_max_tool_iterations("default"), 10);
    assert_eq!(parsed.effective_max_history_messages("default"), 50);
    assert_eq!(
        parsed.effective_structured_max_history_messages("default"),
        50
    );
}

#[test]
async fn pacing_config_deserializes_from_toml() {
    let raw = r#"
default_temperature = 0.7
[pacing]
step_timeout_secs = 120
loop_detection_min_elapsed_secs = 60
loop_ignore_tools = ["browser_screenshot", "browser_navigate"]
message_timeout_scale_max = 8
"#;
    let parsed: Config = toml::from_str(raw).unwrap();
    assert_eq!(parsed.pacing.step_timeout_secs, Some(120));
    assert_eq!(parsed.pacing.loop_detection_min_elapsed_secs, Some(60));
    assert_eq!(
        parsed.pacing.loop_ignore_tools,
        vec!["browser_screenshot", "browser_navigate"]
    );
    assert_eq!(parsed.pacing.message_timeout_scale_max, Some(8));
}

#[test]
async fn pacing_config_absent_preserves_defaults() {
    let raw = r#"
default_temperature = 0.7
"#;
    let parsed: Config = toml::from_str(raw).unwrap();
    assert!(parsed.pacing.step_timeout_secs.is_none());
    assert!(parsed.pacing.loop_detection_min_elapsed_secs.is_none());
    assert!(parsed.pacing.loop_ignore_tools.is_empty());
    assert!(parsed.pacing.message_timeout_scale_max.is_none());
}

#[tokio::test]
async fn sync_directory_handles_existing_directory() {
    let dir = std::env::temp_dir().join(format!(
        "zeroclaw_test_sync_directory_{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).await.unwrap();

    sync_directory(&dir).await.unwrap();

    let _ = fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn config_save_prunes_unchanged_default_blocks() {
    // Fresh-init config without any operator edits should write a
    // tiny config.toml — only `schema_version` and any operator-
    // touched fields. The hundreds of all-default blocks
    // (knowledge, memory, observability, etc.) must not appear.
    let dir =
        std::env::temp_dir().join(format!("zeroclaw_save_prune_test_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).await.unwrap();
    let config = Config {
        config_path: dir.join("config.toml"),
        data_dir: dir.join("data"),
        ..Default::default()
    };
    config.save().await.unwrap();
    let raw = fs::read_to_string(&config.config_path).await.unwrap();

    // schema_version must always survive (migration detector
    // anchor); without it a re-load would mis-detect as V1.
    assert!(
        raw.contains("schema_version"),
        "schema_version must survive pruning"
    );

    // Defaulted nested struct blocks must NOT appear in a fresh
    // save. Pick representative samples from across the schema:
    for block in [
        "[memory]",
        "[knowledge]",
        "[observability]",
        "[gateway]",
        "[cost]",
    ] {
        assert!(
            !raw.contains(block),
            "pruned config.toml must not emit defaulted block {block}; got:\n{raw}",
        );
    }

    // Round-trip: load the pruned config and verify it still
    // deserializes to a `Config` (schema defaults fill the gaps).
    let _reloaded: Config = toml::from_str(&raw).expect("pruned config round-trips");

    let _ = fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn config_save_keeps_operator_set_non_default_fields() {
    let dir =
        std::env::temp_dir().join(format!("zeroclaw_save_keep_test_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).await.unwrap();
    let mut config = Config {
        config_path: dir.join("config.toml"),
        data_dir: dir.join("data"),
        ..Default::default()
    };
    // Operator picked a non-default locale + provider entry.
    config.locale = Some("ja-JP".into());
    config.providers.models.anthropic.insert(
        "claude_default".into(),
        AnthropicModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("claude-sonnet-4".into()),
                ..Default::default()
            },
        },
    );
    config.save().await.unwrap();
    let raw = fs::read_to_string(&config.config_path).await.unwrap();

    assert!(
        raw.contains("ja-JP"),
        "operator-set locale must survive pruning; got:\n{raw}",
    );
    assert!(
        raw.contains("claude_default"),
        "operator-added provider alias must survive pruning; got:\n{raw}",
    );
    assert!(
        raw.contains("claude-sonnet-4"),
        "operator-set model must survive pruning; got:\n{raw}",
    );

    let _ = fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn config_save_and_load_tmpdir() {
    let dir = std::env::temp_dir().join("zeroclaw_test_config");
    let _ = fs::remove_dir_all(&dir).await;
    fs::create_dir_all(&dir).await.unwrap();

    let config_path = dir.join("config.toml");
    let mut providers = crate::providers::Providers::default();
    providers.models.openrouter.insert(
        "default".to_string(),
        OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("sk-roundtrip".into()),
                model: Some("test-model".into()),
                temperature: Some(0.9),
                timeout_secs: Some(120),
                ..Default::default()
            },
        },
    );
    let config = Config {
        composition: None,
        tasks: crate::tasks::TasksConfig::default(),
        degraded_security: Vec::new(),
        degraded_sections: Vec::new(),
        retired_surface_warnings: Vec::new(),
        loaded_from: None,
        schema_version: crate::migration::CURRENT_SCHEMA_VERSION,
        providers,
        model_routes: Vec::new(),
        embedding_routes: Vec::new(),
        data_dir: dir.join("workspace"),
        config_path: config_path.clone(),
        observability: ObservabilityConfig::default(),
        trust: crate::scattered_types::TrustConfig::default(),
        backup: BackupConfig::default(),
        data_retention: DataRetentionConfig::default(),
        security: SecurityConfig::default(),
        security_ops: SecurityOpsConfig::default(),
        runtime: RuntimeConfig::default(),
        reliability: ReliabilityConfig::default(),
        scheduler: SchedulerConfig::default(),
        skills: SkillsConfig::default(),
        pipeline: PipelineConfig::default(),
        query_classification: QueryClassificationConfig::default(),
        heartbeat: HeartbeatConfig::default(),
        cron: HashMap::new(),
        acp: AcpConfig::default(),
        channels: ChannelsConfig::default(),
        memory: MemoryConfig::default(),
        storage: StorageConfig::default(),
        tunnel: TunnelConfig::default(),
        gateway: GatewayConfig::default(),
        secrets: SecretsConfig::default(),
        browser: BrowserConfig::default(),
        http_request: HttpRequestConfig::default(),
        multimodal: MultimodalConfig::default(),
        media_pipeline: MediaPipelineConfig::default(),
        web_fetch: WebFetchConfig::default(),
        link_enricher: LinkEnricherConfig::default(),
        text_browser: TextBrowserConfig::default(),
        web_search: WebSearchConfig::default(),
        proxy: ProxyConfig::default(),
        pacing: PacingConfig::default(),
        cost: CostConfig::default(),
        agents: HashMap::new(),
        risk_profiles: HashMap::new(),
        runtime_profiles: HashMap::new(),
        personas: HashMap::new(),
        cards: HashMap::new(),
        companion_memory: crate::companion::CompanionMemoryConfig::default(),
        skill_bundles: HashMap::new(),
        knowledge_bundles: HashMap::new(),
        mcp_bundles: HashMap::new(),
        peer_groups: HashMap::new(),
        hooks: HooksConfig::default(),
        transcription: TranscriptionConfig::default(),
        tts: TtsConfig::default(),
        mcp: McpConfig::default(),
        tachi: crate::tachi::TachiConfig::default(),
        nodes: NodesConfig::default(),
        onboard_state: OnboardStateConfig::default(),
        node_transport: NodeTransportConfig::default(),
        knowledge: KnowledgeConfig::default(),
        image_gen: ImageGenConfig::default(),
        file_upload: FileUploadConfig::default(),
        file_upload_bundle: FileUploadBundleConfig::default(),
        file_download: FileDownloadConfig::default(),
        locale: None,
        verifiable_intent: VerifiableIntentConfig::default(),
        sop: SopConfig::default(),
        shell_tool: ShellToolConfig::default(),
        escalation: EscalationConfig::default(),
        env_overridden_paths: EnvOverriddenPaths::new(),
        pre_override_snapshots: PreOverrideSnapshots::new(),
        onepassword_reference_snapshots: ExternalRefSnapshots::new(),
        dirty_paths: std::collections::HashSet::new(),
        dirty_map_entries: std::collections::HashSet::new(),
        explicit_prop_dirty_paths: std::collections::HashSet::new(),
        tracked_prop_identities: std::collections::HashMap::new(),
    };

    // ModelProvider fields are now resolved directly — no cache needed.
    config.save().await.unwrap();
    assert!(config_path.exists());

    let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
    let loaded = crate::migration::migrate_to_current(&contents).unwrap();
    let entry = &loaded
        .providers
        .models
        .find("openrouter", "default")
        .expect("entry exists");
    assert!(
        entry
            .api_key
            .as_deref()
            .is_some_and(crate::secrets::SecretStore::is_encrypted)
    );
    let store = crate::secrets::SecretStore::new(&dir, true);
    let decrypted = store.decrypt(entry.api_key.as_deref().unwrap()).unwrap();
    assert_eq!(decrypted, "sk-roundtrip");
    assert_eq!(entry.model.as_deref(), Some("test-model"));
    assert!(
        entry
            .temperature
            .is_some_and(|t| (t - 0.9).abs() < f64::EPSILON)
    );

    let _ = fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn config_save_encrypts_nested_credentials() {
    let dir = std::env::temp_dir().join(format!(
        "zeroclaw_test_nested_credentials_{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).await.unwrap();

    let mut config = Config {
        data_dir: dir.join("workspace"),
        config_path: dir.join("config.toml"),
        ..Default::default()
    };
    config.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("root-credential".into()),
                extra_headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer provider-header-credential".to_string(),
                )]),
                ..Default::default()
            },
        },
    );
    // ModelProvider fields are now resolved directly — no cache needed.
    config.browser.computer_use.api_key = Some("browser-credential".into());
    config.web_search.brave_api_key = Some("brave-credential".into());
    config.web_search.tavily_api_key = Some("tavily-credential".into());
    config.reliability.api_keys = vec![
        "rotation-credential-a".into(),
        "rotation-credential-b".into(),
    ];
    config.node_transport.shared_secret = "node-shared-credential".into();
    config.nodes.auth_token = Some("nodes-auth-credential".into());
    config.file_upload.headers = HashMap::from([(
        "Authorization".to_string(),
        "Bearer upload-credential".to_string(),
    )]);
    config.http_request.secrets = HashMap::from([(
        "api_token".to_string(),
        "Bearer http-request-credential".to_string(),
    )]);
    config.channels.telegram.insert(
        "primary".to_string(),
        TelegramConfig {
            enabled: true,
            bot_token: "telegram-bot-fixture".into(),
            ..TelegramConfig::default()
        },
    );

    config.providers.models.openrouter.insert(
        "worker".into(),
        crate::schema::OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("agent-credential".into()),
                model: Some("model-test".into()),
                ..Default::default()
            },
        },
    );
    config.agents.insert(
        "worker".into(),
        AliasedAgentConfig {
            model_provider: "openrouter.worker".into(),
            ..Default::default()
        },
    );

    // Webhook channel: auth_header carries a Bearer token; must be
    // encrypted alongside the existing webhook `secret` field.
    config.channels.webhook.insert(
        "primary".into(),
        WebhookConfig {
            enabled: true,
            port: 8080,
            auth_header: Some("Bearer webhook-cred".into()),
            secret: Some("webhook-shared-secret".into()),
            ..Default::default()
        },
    );

    // MCP server: HTTP headers map carries an Authorization Bearer
    // token; the new `#[secret]` on `HashMap<String, String>` must
    // encrypt every value (and only every value — keys stay plain).
    config.mcp.servers.push(McpServerConfig {
        name: "primary".into(),
        transport: McpTransport::Sse,
        url: Some("https://mcp.example.invalid/sse".into()),
        env: HashMap::from([("MCP_API_KEY".to_string(), "mcp-env-credential".to_string())]),
        headers: HashMap::from([
            ("Authorization".to_string(), "Bearer mcp-cred".to_string()),
            ("X-Tenant".to_string(), "tenant-42".to_string()),
        ]),
        ..Default::default()
    });

    config.save().await.unwrap();

    let contents = tokio::fs::read_to_string(config.config_path.clone())
        .await
        .unwrap();
    for plaintext in [
        "root-credential",
        "Bearer provider-header-credential",
        "browser-credential",
        "brave-credential",
        "tavily-credential",
        "rotation-credential-a",
        "rotation-credential-b",
        "node-shared-credential",
        "nodes-auth-credential",
        "Bearer upload-credential",
        "Bearer http-request-credential",
        "mcp-env-credential",
        "Bearer mcp-cred",
        "tenant-42",
    ] {
        assert!(
            !contents.contains(plaintext),
            "saved TOML must not contain plaintext credential `{plaintext}`"
        );
    }
    let stored: Config = crate::migration::migrate_to_current(&contents).unwrap();
    let store = crate::secrets::SecretStore::new(&dir, true);

    let root_encrypted = stored
        .providers
        .models
        .find("anthropic", "default")
        .and_then(|e| e.api_key.as_deref())
        .unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(root_encrypted));
    assert_eq!(store.decrypt(root_encrypted).unwrap(), "root-credential");

    let provider_header = stored
        .providers
        .models
        .find("anthropic", "default")
        .and_then(|e| e.extra_headers.get("Authorization"))
        .unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(provider_header));
    assert_eq!(
        store.decrypt(provider_header).unwrap(),
        "Bearer provider-header-credential"
    );

    let browser_encrypted = stored.browser.computer_use.api_key.as_deref().unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(browser_encrypted));
    assert_eq!(
        store.decrypt(browser_encrypted).unwrap(),
        "browser-credential"
    );

    let web_search_encrypted = stored.web_search.brave_api_key.as_deref().unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(
        web_search_encrypted
    ));
    assert_eq!(
        store.decrypt(web_search_encrypted).unwrap(),
        "brave-credential"
    );

    let tavily_encrypted = stored.web_search.tavily_api_key.as_deref().unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(tavily_encrypted));
    assert_eq!(
        store.decrypt(tavily_encrypted).unwrap(),
        "tavily-credential"
    );

    let worker_provider = stored
        .providers
        .models
        .find("openrouter", "worker")
        .unwrap();
    let worker_encrypted = worker_provider.api_key.as_deref().unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(worker_encrypted));
    assert_eq!(store.decrypt(worker_encrypted).unwrap(), "agent-credential");

    for key in &stored.reliability.api_keys {
        assert!(crate::secrets::SecretStore::is_encrypted(key));
    }
    assert_eq!(
        store.decrypt(&stored.reliability.api_keys[0]).unwrap(),
        "rotation-credential-a"
    );
    assert_eq!(
        store.decrypt(&stored.reliability.api_keys[1]).unwrap(),
        "rotation-credential-b"
    );

    assert!(crate::secrets::SecretStore::is_encrypted(
        &stored.node_transport.shared_secret
    ));
    assert_eq!(
        store.decrypt(&stored.node_transport.shared_secret).unwrap(),
        "node-shared-credential"
    );

    let nodes_auth = stored.nodes.auth_token.as_deref().unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(nodes_auth));
    assert_eq!(store.decrypt(nodes_auth).unwrap(), "nodes-auth-credential");

    let upload_auth = stored.file_upload.headers.get("Authorization").unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(upload_auth));
    assert_eq!(
        store.decrypt(upload_auth).unwrap(),
        "Bearer upload-credential"
    );

    let http_request_auth = stored.http_request.secrets.get("api_token").unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(http_request_auth));
    assert_eq!(
        store.decrypt(http_request_auth).unwrap(),
        "Bearer http-request-credential"
    );

    let tg = stored.channels.telegram.get("primary").unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(&tg.bot_token));
    assert_eq!(
        store.decrypt(&tg.bot_token).unwrap(),
        "telegram-bot-fixture"
    );

    // Webhook auth_header — newly tagged `#[secret]`.
    let webhook = stored.channels.webhook.get("primary").unwrap();
    let webhook_auth = webhook.auth_header.as_deref().unwrap();
    assert!(
        crate::secrets::SecretStore::is_encrypted(webhook_auth),
        "webhook auth_header must be encrypted on save"
    );
    assert_eq!(store.decrypt(webhook_auth).unwrap(), "Bearer webhook-cred");
    // The pre-existing webhook `secret` field stays encrypted too —
    // sanity check that the refactor didn't regress it.
    let webhook_secret = webhook.secret.as_deref().unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(webhook_secret));
    assert_eq!(
        store.decrypt(webhook_secret).unwrap(),
        "webhook-shared-secret"
    );

    // MCP server headers — every value must be encrypted; the keys
    // stay plaintext (TOML table headers are not secret).
    let mcp_server = stored
        .mcp
        .servers
        .iter()
        .find(|s| s.name == "primary")
        .expect("mcp server `primary` round-trips through save");
    for (key, value) in &mcp_server.headers {
        assert!(
            crate::secrets::SecretStore::is_encrypted(value),
            "mcp.servers.primary.headers.{key} must be encrypted on save"
        );
    }
    let mcp_env = mcp_server.env.get("MCP_API_KEY").unwrap();
    assert!(
        crate::secrets::SecretStore::is_encrypted(mcp_env),
        "mcp.servers.primary.env.MCP_API_KEY must be encrypted on save"
    );
    let auth = mcp_server.headers.get("Authorization").unwrap();
    let tenant = mcp_server.headers.get("X-Tenant").unwrap();
    assert_eq!(store.decrypt(mcp_env).unwrap(), "mcp-env-credential");
    assert_eq!(store.decrypt(auth).unwrap(), "Bearer mcp-cred");
    assert_eq!(store.decrypt(tenant).unwrap(), "tenant-42");

    let _ = fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn config_save_atomic_cleanup() {
    let dir = std::env::temp_dir().join(format!("zeroclaw_test_config_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).await.unwrap();

    let config_path = dir.join("config.toml");
    let mut config = Config {
        data_dir: dir.join("workspace"),
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.providers.models.openrouter.insert(
        "default".to_string(),
        OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("model-a".into()),
                ..Default::default()
            },
        },
    );
    config.save().await.unwrap();
    assert!(config_path.exists());
    // This value just wrote the file; mirror load_or_init's fresh-init
    // provenance so the second save exercises the atomic-write path,
    // not the unproven-overwrite guard.
    config.loaded_from = Some(config_path.clone());

    config
        .providers
        .models
        .ensure("openrouter", "default")
        .unwrap()
        .model = Some("model-b".into());
    config.save().await.unwrap();

    let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
    assert!(contents.contains("model-b"));

    let mut names: Vec<String> = Vec::new();
    let mut read_dir = fs::read_dir(&dir).await.unwrap();
    while let Some(entry) = read_dir.next_entry().await.unwrap() {
        names.push(entry.file_name().to_string_lossy().to_string());
    }
    assert!(!names.iter().any(|name| name.contains(".tmp-")));
    assert!(!names.iter().any(|name| name.ends_with(".bak")));

    let _ = fs::remove_dir_all(&dir).await;
}

// ── Telegram / Discord config ────────────────────────────

#[test]
async fn telegram_config_defaults_stream_off() {
    let json = r#"{"bot_token":"tok","allowed_users":[]}"#;
    let parsed: TelegramConfig = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.stream_mode, StreamMode::Off);
    assert_eq!(parsed.draft_update_interval_ms, 1000);
    assert!(!parsed.interrupt_on_new_message);
    assert_eq!(parsed.api_base_url, "https://api.telegram.org");
}

// ── iMessage / Matrix config ────────────────────────────

// iMessage `allowed_contacts` was lifted out of `IMessageConfig` in V3;
// inbound peer authorization lives in `Config::peer_groups`. The
// round-trip of contact-list values from a V2 TOML is exercised by
// `imessage_v2_allowed_contacts_fold_into_peer_groups` below; per-field
// struct serde for `allowed_contacts` no longer applies.

// ── Edge cases: serde(default) for non-secret optional fields ─────
// The legacy `allowed_users` field is no longer carried on channel
// configs (V3 moved inbound peer authorization into
// `Config::peer_groups`); V2 TOMLs with `allowed_users` are folded
// by `migrate_to_current` into `[peer_groups.<type>_<alias>]`. See
// `discord_v2_allowed_users_fold_into_peer_groups` below.

#[test]
async fn discord_v2_allowed_users_fold_into_peer_groups() {
    let raw = r#"
schema_version = 2

[channels.discord]
enabled = true
bot_token = "tok"
guild_id = "123"
allowed_users = ["111", "222"]
"#;
    let parsed = crate::migration::migrate_to_current(raw).expect("migration succeeds");
    let group = parsed
        .peer_groups
        .get("discord_default")
        .expect("V2 discord.allowed_users must fold into peer_groups.discord_default");
    assert_eq!(group.channel, "discord");
    let usernames: Vec<&str> = group.external_peers.iter().map(|p| p.as_str()).collect();
    assert_eq!(usernames, vec!["111", "222"]);
}

#[test]
async fn interrupt_on_new_message_defaults_off_and_parses_true() {
    let discord = |json: &str| {
        serde_json::from_str::<DiscordConfig>(json)
            .unwrap()
            .interrupt_on_new_message
    };
    assert!(!discord(r#"{"bot_token":"tok"}"#));
    assert!(discord(
        r#"{"bot_token":"tok","interrupt_on_new_message":true}"#
    ));

    let mattermost = |json: &str| {
        serde_json::from_str::<TelegramConfig>(json)
            .unwrap()
            .interrupt_on_new_message
    };
    assert!(!mattermost(
        r#"{"url":"https://mm.example.com","bot_token":"tok"}"#
    ));
    assert!(mattermost(
        r#"{"url":"https://mm.example.com","bot_token":"tok","interrupt_on_new_message":true}"#
    ));
}

#[test]
async fn discord_config_toml_backward_compat() {
    let toml_str = r#"
bot_token = "tok"
guild_id = "123"
"#;
    let parsed: DiscordConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(parsed.bot_token, "tok");
}

#[test]
async fn webhook_config_with_secret() {
    let json = r#"{"port":8080,"secret":"my-secret-key"}"#;
    let parsed: WebhookConfig = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.secret.as_deref(), Some("my-secret-key"));
}

#[test]
async fn webhook_config_without_secret() {
    let json = r#"{"port":8080}"#;
    let parsed: WebhookConfig = serde_json::from_str(json).unwrap();
    assert!(parsed.secret.is_none());
    assert_eq!(parsed.port, 8080);
}

#[test]
async fn webhook_config_port_defaults_when_omitted() {
    let p: WebhookConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(p.port, 8090);
}

#[test]
async fn webhook_config_retry_fields_default_to_none() {
    let json = r#"{"port":8080}"#;
    let parsed: WebhookConfig = serde_json::from_str(json).unwrap();
    assert!(parsed.max_retries.is_none());
    assert!(parsed.retry_base_delay_ms.is_none());
    assert!(parsed.retry_max_delay_ms.is_none());
}

#[test]
async fn webhook_replay_config_defaults_and_roundtrip() {
    for config in [
        WebhookConfig::default(),
        serde_json::from_str::<WebhookConfig>("{}").unwrap(),
    ] {
        assert!(!config.dangerously_disable_replay_protection);
        assert_eq!(config.replay_window_secs, None);
        assert_eq!(config.replay_cache_capacity, None);
    }
    let config: WebhookConfig = toml::from_str(
        r#"
        dangerously_disable_replay_protection = true
        replay_window_secs = 60
        replay_cache_capacity = 128
    "#,
    )
    .unwrap();
    let restored: WebhookConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert!(restored.dangerously_disable_replay_protection);
    assert_eq!(restored.replay_window_secs, Some(60));
    assert_eq!(restored.replay_cache_capacity, Some(128));
}

// ══════════════════════════════════════════════════════════
// SECURITY CHECKLIST TESTS — Gateway config
// ══════════════════════════════════════════════════════════

#[test]
async fn checklist_gateway_default_requires_pairing() {
    let g = GatewayConfig::default();
    assert!(g.require_pairing, "Pairing must be required by default");
}

#[test]
async fn checklist_gateway_default_blocks_public_bind() {
    let g = GatewayConfig::default();
    assert!(
        !g.allow_public_bind,
        "Public bind must be blocked by default"
    );
}

#[test]
async fn checklist_gateway_default_no_tokens() {
    let g = GatewayConfig::default();
    assert!(
        g.paired_tokens.is_empty(),
        "No pre-paired tokens by default"
    );
    assert_eq!(g.pair_rate_limit_per_minute, 10);
    assert_eq!(g.webhook_rate_limit_per_minute, 60);
    assert!(!g.trust_forwarded_headers);
    assert_eq!(g.rate_limit_max_keys, 10_000);
    assert_eq!(g.idempotency_ttl_secs, 300);
    assert_eq!(g.idempotency_max_keys, 10_000);
}

#[test]
async fn checklist_gateway_cli_default_host_is_localhost() {
    // The CLI default for --host is 127.0.0.1 (checked in main.rs)
    // Here we verify the config default matches
    let c = Config::default();
    assert!(
        c.gateway.require_pairing,
        "Config default must require pairing"
    );
    assert!(
        !c.gateway.allow_public_bind,
        "Config default must block public bind"
    );
}

#[test]
async fn checklist_gateway_serde_roundtrip() {
    let g = GatewayConfig {
        operator: GatewayOperatorConfig::default(),
        attention: None,
        port: 42617,
        host: "127.0.0.1".into(),
        require_pairing: true,
        allow_public_bind: false,
        dangerously_allow_unauthenticated_public_bind: false,
        allow_remote_admin: false,
        paired_tokens: vec!["zc_test_token".into()],
        pair_rate_limit_per_minute: 12,
        webhook_rate_limit_per_minute: 80,
        trust_forwarded_headers: true,
        path_prefix: Some("/zeroclaw".into()),
        rate_limit_max_keys: 2048,
        idempotency_ttl_secs: 600,
        idempotency_max_keys: 4096,
        session_persistence: true,
        session_ttl_hours: 0,
        web_dist_dir: None,
        tls: None,
        request_timeout_secs: 30,
        long_running_request_timeout_secs: 600,
        check_updates: true,
        allow_self_upgrade: false,
        bridges: HashMap::from([(
            "telegram".to_string(),
            GatewayBridgeConfig {
                token_hash: "ab".repeat(32),
                session_prefix: Some("tg:".into()),
                sessions: vec!["main".into()],
                owner: None,
                device: None,
            },
        )]),
    };
    let toml_str = toml::to_string(&g).unwrap();
    let parsed: GatewayConfig = toml::from_str(&toml_str).unwrap();
    assert!(parsed.require_pairing);
    assert!(parsed.session_persistence);
    assert_eq!(parsed.session_ttl_hours, 0);
    assert!(!parsed.allow_public_bind);
    assert_eq!(parsed.paired_tokens, vec!["zc_test_token"]);
    assert_eq!(parsed.pair_rate_limit_per_minute, 12);
    assert_eq!(parsed.webhook_rate_limit_per_minute, 80);
    assert!(parsed.trust_forwarded_headers);
    assert_eq!(parsed.path_prefix.as_deref(), Some("/zeroclaw"));
    assert_eq!(parsed.rate_limit_max_keys, 2048);
    assert_eq!(parsed.idempotency_ttl_secs, 600);
    assert_eq!(parsed.idempotency_max_keys, 4096);
    assert!(parsed.check_updates);
    assert!(!parsed.allow_self_upgrade);
    let bridge = &parsed.bridges["telegram"];
    assert_eq!(bridge.token_hash, "ab".repeat(32));
    assert_eq!(bridge.session_prefix.as_deref(), Some("tg:"));
    assert_eq!(bridge.sessions, vec!["main"]);
}

#[test]
async fn bridge_tokens_resolve_by_hash_and_scope_sessions() {
    let token = "zcb_secret";
    let mut gateway = GatewayConfig::default();
    gateway.bridges.insert(
        "telegram".into(),
        GatewayBridgeConfig {
            token_hash: crate::tokens::hash_token(token),
            session_prefix: Some("tg:".into()),
            sessions: vec!["main".into()],
            owner: None,
            device: None,
        },
    );
    gateway.bridges.insert(
        "closed".into(),
        GatewayBridgeConfig {
            token_hash: crate::tokens::hash_token("other"),
            ..GatewayBridgeConfig::default()
        },
    );

    let (name, bridge) = gateway.bridge_for_token(token).expect("token resolves");
    assert_eq!(name, "telegram");
    assert!(gateway.bridge_for_token("zcb_wrong").is_none());
    assert!(gateway.bridge_for_token("").is_none());
    // The stored hash is compared, never the plaintext.
    assert!(gateway.bridge_for_token(&bridge.token_hash).is_none());

    assert!(bridge.allows_session("main"));
    assert!(bridge.allows_session("tg:-100123"));
    assert!(!bridge.allows_session("mainframe"));
    assert!(!bridge.allows_session("other"));
    // No scope opens no chat session; an empty prefix is not a wildcard.
    let (_, closed) = gateway.bridge_for_token("other").unwrap();
    assert!(!closed.allows_session("main"));
    let empty_prefix = GatewayBridgeConfig {
        session_prefix: Some(String::new()),
        ..GatewayBridgeConfig::default()
    };
    assert!(!empty_prefix.allows_session("anything"));
}

#[test]
async fn bridge_token_hash_must_be_a_sha256_hex_digest() {
    let mut config = Config::default();
    config.gateway.bridges.insert(
        "telegram".into(),
        GatewayBridgeConfig {
            token_hash: "zcb_plaintext".into(),
            ..GatewayBridgeConfig::default()
        },
    );
    let err = config.validate().unwrap_err().to_string();
    assert!(err.contains("gateway.bridges.telegram.token_hash"), "{err}");
    config
        .gateway
        .bridges
        .get_mut("telegram")
        .unwrap()
        .token_hash = "0f".repeat(32);
    config.validate().unwrap();
}

#[test]
async fn checklist_gateway_backward_compat_no_gateway_section() {
    // Old configs without [gateway] should get secure defaults
    let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
    let parsed = parse_test_config(minimal);
    assert!(
        parsed.gateway.require_pairing,
        "Missing [gateway] must default to require_pairing=true"
    );
    assert!(
        !parsed.gateway.allow_public_bind,
        "Missing [gateway] must default to allow_public_bind=false"
    );
}

#[test]
async fn checklist_risk_profile_default_is_workspace_scoped() {
    let a = RiskProfileConfig::default();
    assert!(a.workspace_only, "Default profile must be workspace_only");
    assert!(
        !a.forbidden_paths.is_empty(),
        "Default forbidden_paths must not be empty"
    );
    #[cfg(not(target_os = "windows"))]
    {
        assert!(
            a.forbidden_paths.iter().any(|p| p == "/etc"),
            "Must block /etc on Unix"
        );
        assert!(
            a.forbidden_paths.iter().any(|p| p == "/proc"),
            "Must block /proc on Unix"
        );
    }
    #[cfg(target_os = "windows")]
    {
        assert!(
            a.forbidden_paths.iter().any(|p| p == "C:\\Windows"),
            "Must block C:\\Windows on Windows"
        );
        assert!(
            a.forbidden_paths.iter().any(|p| p == "C:\\Program Files"),
            "Must block C:\\Program Files on Windows"
        );
    }
    assert!(
        a.forbidden_paths.contains(&"~/.ssh".to_string()),
        "Must block ~/.ssh"
    );
}

// ══════════════════════════════════════════════════════════
// SECRETS CONFIG TESTS
// ══════════════════════════════════════════════════════════

#[test]
async fn secrets_config_default_encrypts() {
    let s = SecretsConfig::default();
    assert!(s.encrypt, "Encryption must be enabled by default");
}

#[test]
async fn secrets_config_serde_roundtrip() {
    let s = SecretsConfig { encrypt: false };
    let toml_str = toml::to_string(&s).unwrap();
    let parsed: SecretsConfig = toml::from_str(&toml_str).unwrap();
    assert!(!parsed.encrypt);
}

#[test]
async fn secrets_config_backward_compat_missing_section() {
    let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
    let parsed = parse_test_config(minimal);
    assert!(
        parsed.secrets.encrypt,
        "Missing [secrets] must default to encrypt=true"
    );
}

#[test]
async fn browser_config_default_enabled() {
    let b = BrowserConfig::default();
    assert!(b.enabled);
    assert_eq!(b.allowed_domains, vec!["*".to_string()]);
    assert_eq!(b.backend, "agent_browser");
    assert_eq!(b.headed, None);
    assert_eq!(b.computer_use.endpoint, "http://127.0.0.1:8787/v1/actions");
    assert_eq!(b.computer_use.timeout_ms, 15_000);
    assert!(!b.computer_use.allow_remote_endpoint);
    assert!(b.computer_use.window_allowlist.is_empty());
    assert!(b.computer_use.max_coordinate_x.is_none());
    assert!(b.computer_use.max_coordinate_y.is_none());
}

#[test]
async fn browser_config_parses_headed_true() {
    let parsed: BrowserConfig = toml::from_str(
        r#"
backend = "agent_browser"
headed = true
"#,
    )
    .unwrap();

    assert_eq!(parsed.backend, "agent_browser");
    assert_eq!(parsed.headed, Some(true));
}

#[test]
async fn browser_config_backward_compat_missing_section() {
    let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
    let parsed = parse_test_config(minimal);
    assert!(parsed.browser.enabled);
    assert_eq!(parsed.browser.allowed_domains, vec!["*".to_string()]);
}

async fn env_override_lock() -> MutexGuard<'static, ()> {
    // Delegate to the crate-shared lock so env-mutating tests in this
    // module serialize against `env_overrides::tests` too. Without
    // this, tests across the two modules race on `ZEROCLAW_*` vars.
    crate::env_overrides::env_test_lock().await
}

#[test]
async fn v1_known_provider_migrates_with_globals_folded_onto_typed_slot() {
    // Top-level `model_provider` + `model` + `default_temperature` flow
    // onto the migrated typed-slot entry. Vendor-canonical names like
    // `openai` map straight to their typed slot; `wire_api` and
    // `requires_openai_auth` survive the move.
    //
    // (Unknown V1 names like `sub2api` are intentionally silent-dropped
    // by the V2→V3 migration — see the `Unknown/passthrough` arm of
    // `normalize_provider_type` in schema/v2.rs.)
    let raw = r#"
default_temperature = 0.7
model_provider = "openai"
model = "gpt-5.3-codex"

[model_providers.openai]
api_key = "sk-test"
uri = "https://api.openai.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#;

    let parsed = crate::migration::migrate_to_current(raw).expect("migration succeeds");
    assert!(
        parsed
            .providers
            .models
            .contains_model_provider_type("openai"),
        "vendor-canonical V1 provider should land in its typed slot",
    );
    let profile = parsed
        .providers
        .models
        .find("openai", "default")
        .expect("openai.default entry");
    assert_eq!(profile.api_key.as_deref(), Some("sk-test"));
    assert_eq!(profile.uri.as_deref(), Some("https://api.openai.com/v1"));
    assert_eq!(profile.model.as_deref(), Some("gpt-5.3-codex"));
    assert_eq!(profile.wire_api, Some(WireApi::Responses));
    assert!(profile.requires_openai_auth);
}

#[test]
async fn typed_custom_slot_routes_uri_through_find() {
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    config.providers.models.custom.insert(
        "default".to_string(),
        CustomModelProviderConfig {
            base: ModelProviderConfig {
                uri: Some("https://api.tonsof.blue/v1".to_string()),
                ..Default::default()
            },
        },
    );

    assert_eq!(
        config
            .providers
            .models
            .find("custom", "default")
            .and_then(|e| e.uri.as_deref()),
        Some("https://api.tonsof.blue/v1")
    );
    assert!(config.providers.models.find("custom", "default").is_some());
}

#[test]
async fn openai_codex_alias_carries_responses_wire_api_and_requires_openai_auth() {
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    config.providers.models.openai.insert(
        "codex".to_string(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                uri: Some("https://api.tonsof.blue".to_string()),
                wire_api: Some(WireApi::Responses),
                requires_openai_auth: true,
                ..Default::default()
            },
        },
    );

    let entry = config
        .providers
        .models
        .find("openai", "codex")
        .expect("openai.codex entry");
    assert_eq!(entry.uri.as_deref(), Some("https://api.tonsof.blue"));
    assert_eq!(entry.wire_api, Some(WireApi::Responses));
    assert!(entry.requires_openai_auth);
}

/// Round-trip test for the config CLI: a TOML file with a typed-family
/// model entry must deserialize, find via the typed accessor, and
/// re-serialize without losing any field.
#[test]
async fn provider_models_round_trips_through_load_apply_serialize() {
    let _env_guard = env_override_lock().await;
    let toml_in = r#"
schema_version = 3

[providers.models.openrouter.default]
uri = "https://example.invalid/v1"
model = "primary-model"
"#;

    let config: Config = toml::from_str(toml_in).expect("parse toml");

    assert_eq!(
        config
            .providers
            .models
            .find("openrouter", "default")
            .and_then(|e| e.model.as_deref()),
        Some("primary-model"),
    );

    // What `config save` would write back to disk.
    let toml_out = toml::to_string(&config).expect("serialize toml");
    assert!(
        toml_out.contains("primary-model"),
        "serialized config must keep model value; got:\n{toml_out}",
    );
}

/// `resolve_default_model` returns the first available `models.*` entry's
/// model. Returning `None` is reserved for "no model_provider has any model
/// configured", which callers must surface as a configuration error
/// rather than silently substituting a vendor default.
#[test]
async fn resolve_default_model_picks_first_available() {
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    // Empty config: no model anywhere -> None (caller errors loudly).
    assert_eq!(config.resolve_default_model(), None);

    // Add an entry without a model -> still None.
    config
        .providers
        .models
        .anthropic
        .insert("default".into(), AnthropicModelProviderConfig::default());
    assert_eq!(config.resolve_default_model(), None);

    // Add an entry with a model -> first-available wins.
    config.providers.models.together.insert(
        "default".to_string(),
        TogetherModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("tertiary-model".to_string()),
                ..Default::default()
            },
        },
    );
    assert_eq!(
        config.resolve_default_model().as_deref(),
        Some("tertiary-model"),
    );

    // Add a model_provider with a model — resolve_default_model finds it.
    config.providers.models.openrouter.insert(
        "default".to_string(),
        OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("primary-model".to_string()),
                ..Default::default()
            },
        },
    );
    // resolve_default_model returns the first non-empty model across all model_providers.
    assert!(config.resolve_default_model().is_some());
}

#[test]
async fn save_repairs_bare_config_filename_using_runtime_resolution() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("workspace");
    let resolved_config_path = temp_home.join(".zeroclaw").join("config.toml");

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let mut config = Config {
        data_dir: workspace_dir,
        config_path: PathBuf::from("config.toml"),
        ..Default::default()
    };
    config.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig {
            base: ModelProviderConfig {
                temperature: Some(0.5),
                ..Default::default()
            },
        },
    );
    // ModelProvider fields are now resolved directly — no cache needed.
    config.save().await.unwrap();

    assert!(resolved_config_path.exists());
    let saved = tokio::fs::read_to_string(&resolved_config_path)
        .await
        .unwrap();
    let parsed = parse_test_config(&saved);
    assert!(
        (parsed
            .providers
            .models
            .find("anthropic", "default")
            .and_then(|e| e.temperature)
            .unwrap_or(0.7)
            - 0.5)
            .abs()
            < f64::EPSILON
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = tokio::fs::remove_dir_all(temp_home).await;
}

#[test]
async fn validate_ollama_cloud_model_requires_remote_api_url() {
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    config.providers.models.ollama.insert(
        "default".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("glm-5:cloud".to_string()),
                uri: None,
                api_key: Some("ollama-key".to_string()),
                ..Default::default()
            },
        },
    );

    let error = config.validate().expect_err("expected validation to fail");
    assert!(error.to_string().contains(
        "providers.models.ollama.default.model uses ':cloud', but uri is local or unset"
    ));
}

#[test]
async fn validate_ollama_cloud_model_accepts_private_remote_without_api_key() {
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    config.providers.models.ollama.insert(
        "default".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("glm-5:cloud".to_string()),
                uri: Some("http://192.168.1.100:11434".to_string()),
                api_key: None,
                ..Default::default()
            },
        },
    );

    let result = config.validate();
    assert!(result.is_ok(), "expected validation to pass: {result:?}");
}

#[test]
async fn validate_ollama_cloud_model_requires_api_key_for_official_endpoint() {
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    config.providers.models.ollama.insert(
        "default".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("glm-5:cloud".to_string()),
                uri: Some("https://ollama.com/api".to_string()),
                api_key: None,
                ..Default::default()
            },
        },
    );

    let error = config.validate().expect_err("expected validation to fail");
    assert!(error.to_string().contains(
        "providers.models.ollama.default.model uses ':cloud', but no API key is configured"
    ));
}

#[test]
async fn validate_ollama_cloud_model_accepts_remote_endpoint_with_typed_api_key() {
    // V0.8.0: env-var fallback (`OLLAMA_API_KEY`) eradicated.
    // Operators set the credential on the typed alias.
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    config.providers.models.ollama.insert(
        "default".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("glm-5:cloud".to_string()),
                uri: Some("https://ollama.com/api".to_string()),
                api_key: Some("ollama-typed-key".to_string()),
                ..Default::default()
            },
        },
    );

    let result = config.validate();
    assert!(result.is_ok(), "expected validation to pass: {result:?}");
}

#[test]
async fn validate_ollama_cloud_model_checks_each_alias_for_official_key() {
    let _env_guard = env_override_lock().await;
    let mut config = Config::default();
    config.providers.models.ollama.insert(
        "local".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("llama3".to_string()),
                uri: Some("http://192.168.1.100:11434".to_string()),
                ..Default::default()
            },
        },
    );
    config.providers.models.ollama.insert(
        "cloud".to_string(),
        OllamaModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("glm-5:cloud".to_string()),
                uri: Some("https://ollama.com/api".to_string()),
                api_key: None,
                ..Default::default()
            },
        },
    );

    let error = config.validate().expect_err("expected validation to fail");
    assert!(error.to_string().contains(
        "providers.models.ollama.cloud.model uses ':cloud', but no API key is configured"
    ));
}

#[test]
async fn deserialize_rejects_unknown_model_provider_wire_api() {
    let toml = r#"
schema_version = 3

[providers.models.openrouter.default]
uri = "https://api.tonsof.blue/v1"
wire_api = "ws"
"#;
    let err = toml::from_str::<Config>(toml).expect_err("expected deserialize failure");
    let msg = err.to_string();
    assert!(
        msg.contains("wire_api") || msg.contains("ws"),
        "error should reference the invalid wire_api value, got: {msg}"
    );
}

#[test]
async fn resolve_runtime_config_dirs_accepts_legacy_zeroclaw_workspace() {
    let _env_guard = env_override_lock().await;
    let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let default_workspace_dir = default_config_dir.join("workspace");
    let workspace_dir = default_config_dir.join("profile-a");

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };
    let (config_dir, resolved_workspace_dir, source) =
        resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
            .await
            .unwrap();

    // ZEROCLAW_WORKSPACE is the deprecated alias for ZEROCLAW_DATA_DIR.
    // Resolution treats the path as the config root and derives the data
    // sub-dir from it; the source label reflects the deprecated entry.
    assert_eq!(source, ConfigResolutionSource::EnvWorkspaceLegacy);
    assert_eq!(config_dir, workspace_dir);
    assert_eq!(resolved_workspace_dir, workspace_dir.join("data"));

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    let _ = fs::remove_dir_all(default_config_dir).await;
}

#[test]
async fn resolve_runtime_config_dirs_uses_env_config_dir_first() {
    let _env_guard = env_override_lock().await;
    let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let default_workspace_dir = default_config_dir.join("workspace");
    let explicit_config_dir = default_config_dir.join("explicit-config");

    fs::create_dir_all(&default_config_dir).await.unwrap();

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", &explicit_config_dir) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };

    let (config_dir, resolved_workspace_dir, source) =
        resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
            .await
            .unwrap();

    assert_eq!(source, ConfigResolutionSource::EnvConfigDir);
    assert_eq!(config_dir, explicit_config_dir);
    assert_eq!(resolved_workspace_dir, explicit_config_dir.join("data"));

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_CONFIG_DIR") };
    let _ = fs::remove_dir_all(default_config_dir).await;
}

#[test]
async fn resolve_runtime_config_dirs_falls_back_to_default_layout() {
    let _env_guard = env_override_lock().await;
    let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    let default_workspace_dir = default_config_dir.join("workspace");

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    let (config_dir, resolved_workspace_dir, source) =
        resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
            .await
            .unwrap();

    assert_eq!(source, ConfigResolutionSource::DefaultConfigDir);
    assert_eq!(config_dir, default_config_dir);
    assert_eq!(resolved_workspace_dir, default_workspace_dir);

    let _ = fs::remove_dir_all(default_config_dir).await;
}

#[test]
async fn classify_runtime_config_kind_uses_runtime_resolution_source() {
    let _env_guard = env_override_lock().await;
    #[cfg(unix)]
    let fake_home =
        PathBuf::from("/non-temp-zeroclaw-test-home").join(uuid::Uuid::new_v4().to_string());
    #[cfg(not(unix))]
    let fake_home = UserDirs::new()
        .expect("user directories should be available")
        .home_dir()
        .to_path_buf();
    let explicit_config_dir = fake_home.join("explicit-config");

    #[cfg(unix)]
    let _home_guard = EnvValueGuard::set("HOME", &fake_home);
    #[cfg(not(unix))]
    let _home_guard = EnvValueGuard::remove("HOME");
    let _data_guard = EnvValueGuard::remove("ZEROCLAW_DATA_DIR");
    let _workspace_guard = EnvValueGuard::remove("ZEROCLAW_WORKSPACE");

    assert_eq!(
        classify_runtime_config_kind(&fake_home.join(".zeroclaw").join("config.toml")).await,
        RuntimeConfigKind::Default
    );

    let _config_guard = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", &explicit_config_dir);
    assert_eq!(
        classify_runtime_config_kind(&explicit_config_dir.join("config.toml")).await,
        RuntimeConfigKind::Custom
    );
}

#[test]
async fn classify_runtime_config_kind_reports_temporary_paths() {
    let tmp = tempfile::TempDir::new().unwrap();

    assert_eq!(
        classify_runtime_config_kind(&tmp.path().join("config.toml")).await,
        RuntimeConfigKind::Temporary
    );
}

async fn create_homebrew_prefix() -> TempDir {
    let prefix = TempDir::new().expect("homebrew prefix temp dir");
    fs::create_dir_all(prefix.path().join("Cellar"))
        .await
        .expect("create Cellar marker");
    prefix
}

#[test]
async fn try_resolve_macos_homebrew_config_dir_detects_cellar_layout() {
    let prefix = create_homebrew_prefix().await;
    let exe = prefix
        .path()
        .join("Cellar")
        .join("zeroclaw")
        .join("0.7.0")
        .join("bin")
        .join("zeroclaw");

    let config_dir = try_resolve_macos_homebrew_config_dir(&exe)
        .await
        .expect("expected Homebrew layout");

    assert_eq!(config_dir, prefix.path().join("var").join("zeroclaw"));
}

#[test]
async fn try_resolve_macos_homebrew_config_dir_detects_prefix_bin_layout() {
    let prefix = create_homebrew_prefix().await;
    let exe = prefix.path().join("bin").join("zeroclaw");

    let config_dir = try_resolve_macos_homebrew_config_dir(&exe)
        .await
        .expect("expected Homebrew layout");

    assert_eq!(config_dir, prefix.path().join("var").join("zeroclaw"));
}

#[test]
async fn try_resolve_macos_homebrew_config_dir_detects_opt_bin_layout() {
    let prefix = create_homebrew_prefix().await;
    let exe = prefix
        .path()
        .join("opt")
        .join("zeroclaw")
        .join("bin")
        .join("zeroclaw");

    let config_dir = try_resolve_macos_homebrew_config_dir(&exe)
        .await
        .expect("expected Homebrew layout");

    assert_eq!(config_dir, prefix.path().join("var").join("zeroclaw"));
}

#[test]
async fn try_resolve_macos_homebrew_config_dir_rejects_non_homebrew_layout() {
    let prefix = TempDir::new().expect("non-homebrew temp dir");
    let exe = prefix.path().join("bin").join("zeroclaw");

    assert!(try_resolve_macos_homebrew_config_dir(&exe).await.is_none());
}

#[test]
async fn default_path_under_config_dir_respects_zeroclaw_config_dir() {
    let _env_guard = env_override_lock().await;
    let custom_dir = std::env::temp_dir().join("zeroclaw-test-profile");
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", &custom_dir) };

    let result = default_path_under_config_dir("knowledge.db");

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_CONFIG_DIR") };

    assert_eq!(
        result,
        custom_dir.join("knowledge.db").to_string_lossy().as_ref(),
        "expected path under ZEROCLAW_CONFIG_DIR, got: {result}"
    );
}

#[test]
async fn load_or_init_workspace_override_uses_workspace_root_for_config() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    // V3 fresh init: `config.data_dir` lives at `<install>/data/`
    // (the shared databases root); the install root holds
    // `config.toml`. No synthesized `agents/default/workspace/` is
    // created at boot — `default` is migration-only, and per-agent
    // workspaces are created lazily at agent-loop entry.
    assert_eq!(config.data_dir, workspace_dir.join("data"));
    assert_eq!(config.config_path, workspace_dir.join("config.toml"));
    assert!(workspace_dir.join("config.toml").exists());
    assert!(
        !workspace_dir.join("agents").exists(),
        "fresh init must not create agents/ tree"
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
async fn load_or_init_invalid_composition_hard_errors() {
    // `composition` is brand-new (no released config can carry a value
    // this binary didn't ship), so an invalid value is an operator
    // typo, never a legacy artifact. It must fail the load with the
    // documented value list instead of being silently salvaged to
    // absent → `full`, which would widen the assembled tool surface
    // past what the operator asked for.
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let existing_dir = temp_home.join("profile-invalid-composition");
    let existing_path = existing_dir.join("config.toml");
    fs::create_dir_all(&existing_dir).await.unwrap();
    fs::write(
        &existing_path,
        "composition = \"everything\"\ndefault_temperature = 0.7\n",
    )
    .await
    .unwrap();
    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &existing_dir) };

    let err = Box::pin(Config::load_or_init())
        .await
        .expect_err("invalid composition must fail config load");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("composition"),
        "error must name the offending key: {msg}"
    );
    assert!(
        msg.contains("minimal") && msg.contains("full") && msg.contains("legacy"),
        "error must list the valid values (minimal/full/legacy): {msg}"
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
async fn load_or_init_warns_about_retired_feishu_and_lark_settings() {
    let _env_guard = env_override_lock().await;
    let dir = TempDir::new().unwrap();
    let _config_guard = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", dir.path());
    let config_path = dir.path().join("config.toml");

    for (version, channels, retired, setting, remedy) in [
        (
            "",
            "channels_config",
            "feishu",
            "[channels_config.feishu]\nenabled = true\n",
            "Remove the section.",
        ),
        (
            "",
            "channels_config",
            "lark",
            "[channels_config.lark]\nuse_feishu = true\n",
            "Remove the section.",
        ),
        (
            "",
            "channels_config",
            "feishu",
            "[channels_config]\nfeishu = false\n",
            "Remove the key.",
        ),
        (
            "schema_version = 2\n",
            "channels",
            "feishu",
            "[channels.feishu]\nenabled = true\n",
            "Remove the section.",
        ),
        (
            "schema_version = 2\n",
            "channels",
            "lark",
            "[channels.lark]\nreceive_mode = 'webhook'\n",
            "Remove the section.",
        ),
        (
            "schema_version = 2\n",
            "channels",
            "feishu",
            "[channels]\nfeishu = false\n",
            "Remove the key.",
        ),
    ] {
        let retired_path = format!("{channels}.{retired}");
        let contents = format!("{version}{setting}\n[{channels}.webhook]\nport = 8080\n");
        fs::write(&config_path, contents).await.unwrap();

        let config = Box::pin(Config::load_or_init()).await.unwrap();
        assert_eq!(config.channels.webhook["default"].port, 8080);

        let warnings = config.collect_warnings();
        let warning = warnings
            .iter()
            .find(|warning| warning.path == retired_path)
            .unwrap_or_else(|| panic!("missing warning for [{retired_path}]: {warnings:?}"));
        assert_eq!(warning.code, "retired_channel_transport_removed");
        assert!(warning.message.contains(&retired_path));
        assert!(warning.message.contains(remedy));
    }
}

// Real file loading must diagnose the pre-migration path, while preserving
// adjacent live credentials, guild/channel restrictions and peer allowlists.
async fn assert_retired_channel_settings_load_with_warning(settings: &[(&str, &str)]) {
    let _env_guard = env_override_lock().await;
    let dir = TempDir::new().unwrap();
    let _config_guard = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", dir.path());
    let config_path = dir.path().join("config.toml");
    const RETIRED_SECRET: &str = "synthetic-retired-credential";
    for version in 1..=3 {
        let (stamp, root, alias) = match version {
            1 => ("", "channels_config", ""),
            2 => ("schema_version = 2\n", "channels", ""),
            _ => ("schema_version = 3\n", "channels", ".default"),
        };
        for (kind, fields) in settings {
            let retired_path = format!("{root}.{kind}");
            let allowlist = if version < 3 {
                "allowed_users = ['retained-peer']\n"
            } else {
                ""
            };
            let peer_group = if version == 3 {
                "[agents.default]\nchannels = ['discord.default']\n\n[peer_groups.discord_default]\nchannel = 'discord'\nagents = ['default']\nexternal_peers = ['retained-peer']\nignore = []\n"
            } else {
                ""
            };
            let contents = format!(
                "{stamp}[{retired_path}{alias}]\nenabled = false\n{fields}\nallowed_users = ['retired-peer']\n\n\
                 [{root}.discord{alias}]\nenabled = true\nbot_token = 'synthetic-live-token'\n\
                 guild_ids = ['retained-guild']\nchannel_ids = ['retained-channel']\n\
                 mention_only = true\nlisten_to_bots = false\n{allowlist}\n\
                 [{root}.email{alias}]\nenabled = false\nimap_host = 'imap.example.test'\n\
                 smtp_host = 'smtp.example.test'\nusername = 'synthetic@example.test'\n\
                 password = 'synthetic-live-password'\nfrom_address = 'synthetic@example.test'\n\n\
                 {peer_group}"
            );
            fs::write(&config_path, contents).await.unwrap();
            let config = Box::pin(Config::load_or_init()).await.unwrap();
            let discord = &config.channels.discord["default"];
            assert_eq!(discord.bot_token, "synthetic-live-token");
            assert_eq!(discord.guild_ids, ["retained-guild"]);
            assert_eq!(discord.channel_ids, ["retained-channel"]);
            assert!(discord.mention_only);
            assert!(!discord.listen_to_bots);
            let email = &config.channels.email["default"];
            assert_eq!(email.password, "synthetic-live-password");
            assert_eq!(email.username, "synthetic@example.test");
            let group = &config.peer_groups["discord_default"];
            assert_eq!(group.channel.to_string(), "discord");
            assert_eq!(
                group
                    .external_peers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                ["retained-peer"]
            );
            assert!(group.ignore.is_empty());
            assert!(!group.admin_for_agent_scope);
            assert!(!config.peer_groups.contains_key(&format!("{kind}_default")));
            let warnings = config.collect_warnings();
            let retired: Vec<_> = warnings.iter().filter(|w| w.path == retired_path).collect();
            assert_eq!(
                retired.len(),
                1,
                "V{version} {retired_path} must warn exactly once"
            );
            assert_eq!(retired[0].code, "retired_channel_transport_removed");
            assert!(retired[0].message.contains("Remove the section."));
            assert!(warnings.iter().all(|w| !w.message.contains(RETIRED_SECRET)));
        }
    }
}

#[test]
async fn load_or_init_warns_about_retired_batch_b_channel_settings() {
    assert_retired_channel_settings_load_with_warning(&[
        ("line", "channel_access_token = 'synthetic-retired-credential'\nchannel_secret = 'synthetic-retired-credential'\ndm_policy = 'allowlist'\ngroup_policy = 'mention'"),
        ("dingtalk", "client_id = 'synthetic-client'\nclient_secret = 'synthetic-retired-credential'"),
        ("wecom", "webhook_key = 'synthetic-retired-credential'"),
        ("wecom_ws", "bot_id = 'synthetic-bot'\nsecret = 'synthetic-retired-credential'\nallowed_groups = ['retired-group']"),
        ("wechat", "api_base_url = 'https://synthetic-retired-credential.example.test'"),
        ("qq", "app_id = 'synthetic-app'\napp_secret = 'synthetic-retired-credential'"),
        ("twitter", "bearer_token = 'synthetic-retired-credential'"),
        ("mochat", "api_url = 'https://example.test'\napi_token = 'synthetic-retired-credential'"),
    ]).await;
}

#[test]
async fn load_or_init_warns_about_retired_batch_c_channel_settings() {
    assert_retired_channel_settings_load_with_warning(&[
        ("nostr", "private_key = 'synthetic-retired-credential'\nallowed_pubkeys = ['retired-pubkey']"),
        ("clawdtalk", "api_key = 'synthetic-retired-credential'\nconnection_id = 'synthetic-connection'\nfrom_number = '+15555550110'"),
        ("reddit", "client_id = 'synthetic-client'\nclient_secret = 'synthetic-retired-credential'\nrefresh_token = 'synthetic-retired-credential'\nusername = 'synthetic-user'\nsubreddit = 'synthetic-community'"),
        ("bluesky", "handle = 'synthetic.example.test'\napp_password = 'synthetic-retired-credential'"),
        ("git", "provider = 'gitea'\napi_base_url = 'https://example.test'\naccess_token = 'synthetic-retired-credential'\nrepos = ['synthetic/repository']"),
        ("voice_call", "account_id = 'synthetic-account'\nauth_token = 'synthetic-retired-credential'\nfrom_number = '+15555550111'"),
        ("voice_wake", "wake_word = 'synthetic-retired-credential'"),
        ("amqp", "amqp_url = 'amqp://synthetic:synthetic-retired-credential@example.test'\nexchange = 'synthetic-exchange'\nrouting_keys = ['synthetic-key']"),
    ]).await;
}

#[test]
async fn load_or_init_warns_about_retired_whatsapp_security_settings() {
    let _env_guard = env_override_lock().await;
    let dir = TempDir::new().unwrap();
    let _config_guard = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", dir.path());
    let config_path = dir.path().join("config.toml");

    for (version, channels, alias) in [
        ("", "channels_config", ""),
        ("schema_version = 2\n", "channels", ""),
        ("schema_version = 3\n", "channels", ".personal"),
    ] {
        let retired_path = format!("{channels}.whatsapp");
        let contents = format!(
            r#"{version}[{retired_path}{alias}]
enabled = true
session_path = "/tmp/retired-whatsapp.db"
access_token = "retired-token"
app_secret = "retired-secret"
verify_token = "retired-verify"
allowed_numbers = ["+15555550102"]
allowed_groups = ["group-id"]
dm_policy = "allowlist"
group_policy = "allowlist"
approval_timeout_secs = 60
excluded_tools = ["shell"]

[transcription]
max_duration_secs = 90
transcribe_non_ptt_audio = true

[{channels}.webhook{alias}]
enabled = true
port = 8080
"#
        );
        fs::write(&config_path, contents).await.unwrap();

        let config = Box::pin(Config::load_or_init()).await.unwrap();
        let webhook_alias = if alias.is_empty() {
            "default"
        } else {
            "personal"
        };
        assert_eq!(config.channels.webhook[webhook_alias].port, 8080);
        assert_eq!(config.transcription.max_duration_secs, 90);
        assert!(
            config.peer_groups.is_empty(),
            "retired restrictions must not authorize another transport"
        );
        assert!(
            config.degraded_sections.is_empty(),
            "{:?}",
            config.degraded_sections
        );
        assert!(!config.channels.is_known_channel("whatsapp"));
        assert!(
            !toml::to_string(&config)
                .unwrap()
                .contains("[channels.whatsapp")
        );
        let warnings = config.collect_warnings();
        assert!(warnings.iter().any(|warning| {
            warning.path == "transcription.transcribe_non_ptt_audio"
                && warning.code == "whatsapp_audio_option_removed"
        }));
        let retired: Vec<_> = warnings
            .iter()
            .filter(|warning| warning.path == retired_path)
            .collect();
        assert_eq!(retired.len(), 1, "{warnings:?}");
        let warning = retired[0];
        assert_eq!(warning.code, "retired_channel_transport_removed");
        assert!(warning.message.contains(&retired_path));
        assert!(warning.message.contains("no runtime consumer"));
        assert!(warning.message.contains("Remove the section."));
        for sensitive in [
            "/tmp/retired-whatsapp.db",
            "retired-token",
            "retired-secret",
            "retired-verify",
            "+15555550102",
            "group-id",
        ] {
            assert!(
                warnings
                    .iter()
                    .all(|warning| !warning.message.contains(sensitive))
            );
        }
    }
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_warns_for_legacy_eval_and_preserves_runtime_scorer() {
    let _env_guard = env_override_lock().await;
    let home = TempDir::new().unwrap();
    let workspace = home.path().join("eval-profile");
    fs::create_dir_all(&workspace).await.unwrap();
    fs::write(
        workspace.join("config.toml"),
        r#"schema_version = 3
[eval]
suite_dir = "custom-traces"
mode = "replay"
[runtime_profiles.default.eval]
enabled = true
min_quality_score = 0.73
max_retries = 3
"#,
    )
    .await
    .unwrap();
    let _home = EnvValueGuard::set("HOME", home.path());
    let _workspace = EnvValueGuard::remove("ZEROCLAW_WORKSPACE");
    let _data_dir = EnvValueGuard::remove("ZEROCLAW_DATA_DIR");
    let _config_dir = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", &workspace);

    let config = Box::pin(Config::load_or_init()).await.unwrap();
    assert_eq!(config.config_path, workspace.join("config.toml"));
    assert_eq!(config.data_dir, workspace.join("data"));
    assert!(config.degraded_sections.is_empty());
    let warnings = config.collect_warnings();
    let warning = warnings
        .iter()
        .find(|warning| warning.code == "eval_harness_moved_to_dev_tool")
        .expect("legacy eval must produce a load warning");
    assert_eq!(warning.path, "eval");
    assert!(warning.message.contains("--suite"));
    let scorer = &config.runtime_profiles["default"].eval;
    assert!(scorer.enabled);
    assert_eq!(scorer.min_quality_score, 0.73);
    assert_eq!(scorer.max_retries, 3);
}

#[test]
async fn load_or_init_retired_pairing_dashboard_section_parses_with_tombstone_warning() {
    // Section-parse finding: `[gateway.pairing_dashboard]` in a config
    // file is an unknown (silently ignored) nested section after schema
    // removal — serde drops it because `GatewayConfig` does not use
    // `deny_unknown_fields`, so deployments carrying the section keep
    // parsing. This tombstone mirrors the env-prefix shim: the load
    // succeeds and the retired section surfaces one structured
    // `gateway_pairing_dashboard_removed` warning.
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let existing_dir = temp_home.join("profile-retired-section");
    let existing_path = existing_dir.join("config.toml");
    fs::create_dir_all(&existing_dir).await.unwrap();
    fs::write(
        &existing_path,
        "default_temperature = 0.7\n\n[gateway.pairing_dashboard]\ncode_length = 8\ncode_ttl_secs = 3600\nmax_pending_codes = 3\nmax_failed_attempts = 5\nlockout_secs = 300\n",
    )
    .await
    .unwrap();
    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &existing_dir) };

    let config = Box::pin(Config::load_or_init())
        .await
        .expect("retired section must keep parsing (tombstone, not error)");

    // Strict migration path tolerates the section too (serde ignores
    // unknown nested sections) — the file section is not a load error
    // on any path.
    let contents = fs::read_to_string(&existing_path).await.unwrap();
    crate::migration::migrate_to_current(&contents)
        .expect("strict parse must also tolerate the retired section");

    let warnings = config.collect_warnings();
    let warning = warnings
        .iter()
        .find(|w| w.path == "gateway.pairing_dashboard")
        .unwrap_or_else(|| panic!("expected tombstone warning, got: {warnings:?}"));
    assert_eq!(warning.code, "gateway_pairing_dashboard_removed");
    assert!(
        warning.message.contains("ignored"),
        "warning must state the section is ignored: {}",
        warning.message
    );
    assert!(
        warning.message.contains("[gateway.pairing_dashboard]"),
        "warning must name the retired section: {}",
        warning.message
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
async fn load_or_init_fresh_install_pins_minimal_composition_and_keeps_existing_full() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    // Fresh install: the bootstrap writes the minimal composition into
    // the brand-new file and the in-memory config carries the same
    // value, so the first turn assembles the minimal surface.
    let fresh = Box::pin(Config::load_or_init()).await.unwrap();
    assert_eq!(
        fresh.composition,
        Some(crate::composition::Composition::Minimal)
    );
    let raw = fs::read_to_string(fresh.config_path.clone()).await.unwrap();
    assert!(
        raw.contains("composition = \"minimal\""),
        "fresh config must pin composition = \"minimal\", got: {raw}"
    );

    // Existing install without the key: no migration. The field stays
    // absent on disk and in memory, so it keeps resolving as `full`.
    let existing_dir = temp_home.join("profile-existing");
    let existing_path = existing_dir.join("config.toml");
    fs::create_dir_all(&existing_dir).await.unwrap();
    fs::write(
        &existing_path,
        r#"default_temperature = 0.7
default_model = "persisted-profile"
"#,
    )
    .await
    .unwrap();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &existing_dir) };
    let existing = Box::pin(Config::load_or_init()).await.unwrap();
    assert!(
        existing.composition.is_none(),
        "existing config without the key must not gain one"
    );
    assert_eq!(
        crate::composition::Composition::effective(existing.composition),
        crate::composition::Composition::Full
    );
    let raw_existing = fs::read_to_string(&existing_path).await.unwrap();
    assert!(
        !raw_existing.contains("composition"),
        "existing config file must not be rewritten with a composition: {raw_existing}"
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
async fn load_or_init_workspace_suffix_uses_legacy_config_layout() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("workspace");
    let legacy_config_dir = temp_home.join(".zeroclaw");
    let legacy_config_path = legacy_config_dir.join("config.toml");

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    // V3: `config.data_dir` lives at `<install>/data/`. The
    // ZEROCLAW_WORKSPACE env var (deprecated alias) resolved to the
    // legacy config layout where the install root is the parent of
    // the env-var path; data sits at `<install>/data/`.
    assert_eq!(config.data_dir, legacy_config_dir.join("data"));
    assert_eq!(config.config_path, legacy_config_path);
    assert!(config.config_path.exists());

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
async fn load_or_init_workspace_override_keeps_existing_legacy_config() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("custom-workspace");
    let legacy_config_dir = temp_home.join(".zeroclaw");
    let legacy_config_path = legacy_config_dir.join("config.toml");

    fs::create_dir_all(&legacy_config_dir).await.unwrap();
    fs::write(
        &legacy_config_path,
        r#"default_temperature = 0.7
default_model = "legacy-model"
"#,
    )
    .await
    .unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    // V3: `config.data_dir` resolves to `<install>/data/` under
    // the install root (the directory holding the existing
    // `config.toml`), regardless of the ZEROCLAW_WORKSPACE
    // (deprecated) override.
    assert_eq!(config.data_dir, legacy_config_dir.join("data"));
    assert_eq!(config.config_path, legacy_config_path);
    assert_eq!(
        config
            .providers
            .models
            .find("openrouter", "default")
            .and_then(|e| e.model.as_deref()),
        Some("legacy-model")
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
async fn load_or_init_decrypts_telegram_channel_secrets() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let config_dir = temp_home.join(".zeroclaw");
    let config_path = config_dir.join("config.toml");

    fs::create_dir_all(&config_dir).await.unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };

    let mut config = Config {
        config_path: config_path.clone(),
        data_dir: config_dir.join("workspace"),
        ..Default::default()
    };
    config.secrets.encrypt = true;
    config.channels.telegram.insert(
        "primary".to_string(),
        TelegramConfig {
            enabled: true,
            bot_token: "telegram-bot-fixture".into(),
            ..TelegramConfig::default()
        },
    );
    config.save().await.unwrap();

    let loaded = Box::pin(Config::load_or_init()).await.unwrap();
    let tg = loaded.channels.telegram.get("primary").unwrap();
    assert_eq!(tg.bot_token, "telegram-bot-fixture");

    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_logs_existing_config_as_initialized() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");
    let config_path = workspace_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).await.unwrap();
    fs::write(
        &config_path,
        r#"default_temperature = 0.7
default_model = "persisted-profile"
"#,
    )
    .await
    .unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let mut rx = capture_log_events();

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    let logs = drain_captured(&mut rx);

    // V3: shared databases live at `<install>/data/`, per-agent
    // identity at `<install>/agents/<alias>/workspace/`. The
    // ZEROCLAW_WORKSPACE env var (deprecated alias for
    // ZEROCLAW_DATA_DIR) pinned the install root, so data_dir is
    // `<install>/data/` derived from the resolved root.
    assert_eq!(config.data_dir, workspace_dir.join("data"));
    assert_eq!(config.config_path, config_path);
    assert_eq!(
        config
            .providers
            .models
            .find("openrouter", "default")
            .and_then(|e| e.model.as_deref()),
        Some("persisted-profile")
    );
    assert!(logs.contains("Config loaded"), "{logs}");
    assert!(logs.contains("\"initialized\":true"), "{logs}");
    assert!(!logs.contains("\"initialized\":false"), "{logs}");

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_assigns_degraded_security_for_malformed_section() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");
    let config_path = workspace_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).await.unwrap();
    // `[security] audit` must be a table; a scalar forces the security
    // section to drop to its default on the resilient daemon path.
    fs::write(
        &config_path,
        r#"schema_version = 3
audit = "should-be-a-table-not-a-string"

[security]
audit = "should-be-a-table-not-a-string"
"#,
    )
    .await
    .unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    assert!(
        config.degraded_security.iter().any(|s| s == "security"),
        "load_or_init must surface a dropped [security] section on degraded_security, got {:?}",
        config.degraded_security
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_assigns_degraded_sections_for_malformed_channel_alias() {
    // Regression: `doctor` was blind to degraded_sections even though
    // load_or_init already populates it correctly. A [channels.telegram]
    // alias with a type-corrupt `bot_token` (not merely missing - see
    // the scenario where a missing bot_token must survive salvage instead of
    // being dropped) must be pruned (not fatal) and its path recorded on
    // `degraded_sections` so downstream diagnostics (zeroclaw-runtime's
    // check_degraded_sections) can name it.
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");
    let config_path = workspace_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).await.unwrap();
    fs::write(
        &config_path,
        r#"schema_version = 3

[channels.telegram.default]
enabled = true
bot_token = 42
"#,
    )
    .await
    .unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    assert!(
        config
            .degraded_sections
            .iter()
            .any(|s| s == "channels.telegram.default"),
        "load_or_init must surface a dropped [channels.telegram.default] alias on \
         degraded_sections, got {:?}",
        config.degraded_sections
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
async fn load_or_init_keeps_partial_channel_alias_out_of_degraded_sections() {
    // End-to-end companion to the salvage-layer tests: through the
    // real load_or_init entry point, a partial (tokenless) telegram alias
    // must load intact and must NOT be reported on degraded_sections.
    // Disabled so Config::validate() stays quiet about the missing token.
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");
    let config_path = workspace_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).await.unwrap();
    fs::write(
        &config_path,
        r#"schema_version = 3

[channels.telegram.default]
enabled = false
"#,
    )
    .await
    .unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    assert!(
        config.channels.telegram.contains_key("default"),
        "a partial (tokenless) alias must survive load_or_init, got {:?}",
        config.channels.telegram.keys().collect::<Vec<_>>()
    );
    assert!(
        config.degraded_sections.is_empty(),
        "a partial (tokenless) alias must not be reported as degraded, got {:?}",
        config.degraded_sections
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_marks_whole_config_degraded_for_unparseable_file() {
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");
    let config_path = workspace_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).await.unwrap();
    // Not valid TOML at all: the whole config defaults, so every
    // security-critical section is lost at once. load_or_init must surface
    // that on degraded_security so the serving gate refuses to start.
    fs::write(&config_path, "this is not valid TOML {{{")
        .await
        .unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    assert!(
        !config.degraded_security.is_empty(),
        "load_or_init must surface a whole-config loss on degraded_security, got {:?}",
        config.degraded_security
    );

    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_warns_on_retired_delegate_roster() {
    // The delegate tool was deleted with the wall-1 demolition, so a
    // legacy `[agents.*].delegates` roster no longer names anything the
    // runtime can honor. The field tombstone ignores the key (serde
    // drops the unknown field) but surfaces a structured warning naming
    // the retirement, so the rest of the section keeps working and the
    // operator is told to clean up.
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    let workspace_dir = temp_home.join("profile-a");
    let config_path = workspace_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).await.unwrap();
    fs::write(
        &config_path,
        r#"schema_version = 3

[providers.models.ollama.default]

[risk_profiles.shared]

[runtime_profiles.default]

[agents.task_orchestrator]
model_provider = "ollama.default"
risk_profile = "shared"
runtime_profile = "default"
delegates = [
  "reviewer",
  { agent = "sysadmin", mode = "independent" },
]

[agents.reviewer]
model_provider = "ollama.default"
risk_profile = "shared"
runtime_profile = "default"

[agents.sysadmin]
model_provider = "ollama.default"
risk_profile = "shared"
runtime_profile = "default"
"#,
    )
    .await
    .unwrap();

    let original_home = std::env::var("HOME").ok();
    // SAFETY: test-only, guarded by env_override_lock.
    unsafe { std::env::set_var("HOME", &temp_home) };
    // SAFETY: test-only, guarded by env_override_lock.
    unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

    let config = Box::pin(Config::load_or_init()).await.unwrap();

    // SAFETY: test-only, guarded by env_override_lock.
    unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    if let Some(home) = original_home {
        // SAFETY: test-only, guarded by env_override_lock.
        unsafe { std::env::set_var("HOME", home) };
    } else {
        // SAFETY: test-only, guarded by env_override_lock.
        unsafe { std::env::remove_var("HOME") };
    }
    let _ = fs::remove_dir_all(temp_home).await;

    let hits: Vec<_> = config
        .retired_surface_warnings
        .iter()
        .filter(|warning| warning.code == "delegate_config_removed")
        .collect();
    assert!(
        hits.iter()
            .any(|warning| warning.path == "agents.*.delegates"),
        "expected an agents.*.delegates tombstone, got {hits:?}"
    );
    // The surviving section is untouched: the retired key does not
    // degrade its siblings.
    assert!(config.agents.contains_key("task_orchestrator"));
    assert!(config.agents.contains_key("reviewer"));
    assert!(config.agents.contains_key("sysadmin"));
    assert!(
        config.degraded_sections.is_empty(),
        "{:?}",
        config.degraded_sections
    );
}
#[test]
async fn validate_rejects_out_of_range_temperature() {
    let mut config = Config::default();
    config.providers.models.openrouter.insert(
        "default".to_string(),
        OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("sk-test".into()),
                temperature: Some(99.0),
                ..Default::default()
            },
        },
    );
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("temperature"),
        "expected temperature validation error, got: {err}"
    );
}

#[test]
async fn validate_rejects_negative_temperature() {
    let mut config = Config::default();
    config.providers.models.openrouter.insert(
        "default".to_string(),
        OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("sk-test".into()),
                temperature: Some(-0.5),
                ..Default::default()
            },
        },
    );
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("temperature"),
        "expected temperature validation error, got: {err}"
    );
}

#[test]
async fn validate_accepts_valid_temperature() {
    let mut config = Config::default();
    config.providers.models.openrouter.insert(
        "default".to_string(),
        OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                temperature: Some(0.7),
                ..Default::default()
            },
        },
    );
    assert!(config.validate().is_ok());
}

#[test]
async fn proxy_config_scope_services_requires_entries_when_enabled() {
    let proxy = ProxyConfig {
        enabled: true,
        http_proxy: Some("http://127.0.0.1:7890".into()),
        https_proxy: None,
        all_proxy: None,
        no_proxy: Vec::new(),
        scope: ProxyScope::Services,
        services: Vec::new(),
    };

    let error = proxy.validate().unwrap_err().to_string();
    assert!(error.contains("proxy.scope='services'"));
}

#[test]
async fn gateway_config_default_values() {
    let g = GatewayConfig::default();
    assert_eq!(g.port, 42617);
    assert_eq!(g.host, "127.0.0.1");
    assert!(g.require_pairing);
    assert!(!g.allow_public_bind);
    assert!(g.paired_tokens.is_empty());
    assert!(!g.trust_forwarded_headers);
    assert_eq!(g.rate_limit_max_keys, 10_000);
    assert_eq!(g.idempotency_max_keys, 10_000);
}

// ── Peripherals config ───────────────────────────────────────

// ── LINE ──────────────────────────────────────────────────

// ── Config file permission hardening (Unix only) ───────────────

#[cfg(unix)]
#[test]
async fn new_config_file_has_restricted_permissions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Create a config and save it
    let config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.save().await.unwrap();

    let meta = fs::metadata(&config_path).await.unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "New config file should be owner-only (0600), got {mode:o}"
    );
}

#[test]
async fn save_refuses_unproven_overwrite_of_existing_config() {
    // Regression bar (per the leaf contract): a Config that never read the target file
    // must not be able to overwrite an operator's populated config, and
    // the refusal must leave the existing bytes untouched.
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    let operator_bytes = "# operator's hand-written config\n[observability]\nenabled = false\n";
    tokio::fs::write(&config_path, operator_bytes)
        .await
        .unwrap();

    let config = Config {
        config_path: config_path.clone(),
        data_dir: tmp.path().join("data"),
        ..Default::default()
    };

    let err = config
        .save()
        .await
        .expect_err("unproven overwrite must fail");
    assert!(
        err.to_string().contains("Refusing to overwrite"),
        "error should name the refusal, got: {err:#}"
    );
    let after = tokio::fs::read_to_string(&config_path).await.unwrap();
    assert_eq!(
        after, operator_bytes,
        "refused save must leave the existing file byte-identical"
    );
}

#[test]
async fn force_save_overwrites_without_provenance() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    tokio::fs::write(&config_path, "old = true\n")
        .await
        .unwrap();

    let config = Config {
        config_path: config_path.clone(),
        data_dir: tmp.path().join("data"),
        ..Default::default()
    };
    config.force_save().await.unwrap();
    let after = tokio::fs::read_to_string(&config_path).await.unwrap();
    assert!(
        !after.contains("old = true"),
        "force_save must actually replace the file content, got: {after}"
    );
    assert!(
        after.contains("schema_version"),
        "force_save must write a real config body, got: {after}"
    );
}

#[test]
async fn save_refuses_when_provenance_points_at_a_different_file() {
    // Path-bound provenance: a value loaded from file A that is later
    // repointed at a different existing file B must not overwrite B.
    let tmp = tempfile::TempDir::new().unwrap();
    let path_a = tmp.path().join("a-config.toml");
    let path_b = tmp.path().join("b-config.toml");
    let b_bytes = "# operator's file B\n[observability]\nenabled = false\n";
    tokio::fs::write(&path_a, "# source file A\n")
        .await
        .unwrap();
    tokio::fs::write(&path_b, b_bytes).await.unwrap();

    let mut config = Config {
        config_path: path_a.clone(),
        data_dir: tmp.path().join("data"),
        ..Default::default()
    };
    config.loaded_from = Some(path_a.clone());
    config.config_path = path_b.clone();

    let err = config.save().await.expect_err("repointed save must fail");
    assert!(
        err.to_string().contains("Refusing to overwrite"),
        "error should name the refusal, got: {err:#}"
    );
    let after = tokio::fs::read_to_string(&path_b).await.unwrap();
    assert_eq!(after, b_bytes, "file B must stay byte-identical");
}

#[test]
async fn save_creates_missing_file_without_provenance() {
    // First-run creation stays open: the guard only protects an
    // existing file.
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let config = Config {
        config_path: config_path.clone(),
        data_dir: tmp.path().join("data"),
        ..Default::default()
    };
    config.save().await.unwrap();
    assert!(config_path.exists());
}

#[test]
async fn load_or_init_config_saves_over_existing_file() {
    // load_or_init establishes provenance in both branches: a value
    // that read the file (or just created it) keeps full-save rights.
    let _env_guard = env_override_lock().await;
    let temp_home =
        std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
    // ZEROCLAW_* vars outrank HOME in path resolution; remove them so an
    // inherited developer environment cannot redirect this test at a
    // real operator config. RAII guards restore on panic.
    let _home = EnvValueGuard::set("HOME", &temp_home);
    let _config_dir = EnvValueGuard::remove("ZEROCLAW_CONFIG_DIR");
    let _data_dir = EnvValueGuard::remove("ZEROCLAW_DATA_DIR");
    let _workspace = EnvValueGuard::remove("ZEROCLAW_WORKSPACE");

    let fresh = Box::pin(Config::load_or_init()).await.unwrap();
    fresh
        .save()
        .await
        .expect("fresh-init value keeps save rights");
    let loaded = Box::pin(Config::load_or_init()).await.unwrap();
    loaded
        .save()
        .await
        .expect("loaded config keeps save rights");

    let _ = fs::remove_dir_all(temp_home).await;
}

#[cfg(unix)]
#[test]
async fn save_restricts_existing_world_readable_config_to_owner_only() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.save().await.unwrap();
    // This value just wrote the file; mirror load_or_init's fresh-init
    // provenance so the second save below exercises the permission
    // repair, not the unproven-overwrite guard.
    config.loaded_from = Some(config_path.clone());

    // Simulate the regression state observed in issue.
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let loose_mode = std::fs::metadata(&config_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        loose_mode, 0o644,
        "test setup requires world-readable config"
    );

    if let Some(entry) = config.providers.models.ensure("openrouter", "default") {
        entry.temperature = Some(0.6);
    }
    config.save().await.unwrap();

    let hardened_mode = std::fs::metadata(&config_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        hardened_mode, 0o600,
        "Saving config should restore owner-only permissions (0600)"
    );
}

#[test]
async fn save_dirty_stamps_current_schema_version_on_stale_label() {
    // Regression for. An incremental save writes current-schema-shaped
    // sections, but `schema_version` is never a dirty path. Without an
    // explicit stamp, a file first written by an older binary keeps its
    // stale `schema_version` label while gaining a current-schema body — a
    // state that crashes older binaries with `missing field ...`.
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Seed an on-disk file labeled with a stale schema version so the
    // incremental path (not the new-file fallback to full `save`) runs.
    std::fs::write(
        &config_path,
        "schema_version = 2\n\n[observability]\nbackend = \"none\"\n",
    )
    .unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.observability.backend = ObservabilityBackend::Prometheus;
    config.mark_dirty("observability.backend");
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains(&format!(
            "schema_version = {}",
            crate::migration::CURRENT_SCHEMA_VERSION
        )),
        "save_dirty must stamp the current schema_version; got:\n{written}"
    );
    assert!(
        !written.contains("schema_version = 2"),
        "stale schema_version label must be overwritten; got:\n{written}"
    );
    // The dirty value still lands, and the stamp sits at the top of the file.
    assert!(
        written.contains("backend = \"prometheus\""),
        "dirty value must still be written; got:\n{written}"
    );
    assert!(
        written.trim_start().starts_with("schema_version ="),
        "schema_version should remain the first key; got:\n{written}"
    );
}

/// Regression for the per-field `[[mcp.servers]]` editor: after
/// `d06ed25` shipped the natural-key arm, in-memory edits succeed
/// (the TUI / dashboard show the new value) but `save_dirty` is
/// silently a no-op because `apply_dirty_path` walks the serialized
/// TOML as if every segment is a `Table` — `mcp.servers` is an
/// array of tables, so `lookup_path_in_table` returns `None` at the
/// natural-key segment, the path is misclassified as
/// `should_delete`, and `delete_path_in_doc` bails when it hits the
/// array too. Net effect: the on-disk file keeps its stale value.
#[test]
async fn save_dirty_persists_mcp_server_field_via_natural_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Seed an on-disk file with a single MCP server so the
    // incremental path (not the new-file fallback to full `save`)
    // runs. Schema version is stamped to current so the writer
    // doesn't have to migrate anything.
    let seed = format!(
        "schema_version = {}\n\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         transport = \"stdio\"\n\
         command = \"/usr/bin/mcp-fs\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    // Build the in-memory config to match the seeded file. We
    // don't need to round-trip through deserialization — the bug
    // is purely on the save side, and the on-disk seed gives
    // `save_dirty` an existing file to do an incremental write
    // into (the new-file fallback to full `save` would mask the
    // dirty-path bug because it serializes the whole struct).
    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    });
    assert_eq!(config.mcp.servers[0].command, "/usr/bin/mcp-fs");

    // The same call site the dashboard / TUI use: set_prop_persistent
    // on a natural-key-routed inner path, then flush via save_dirty.
    config
        .set_prop_persistent("mcp.servers.fs.command", "/usr/local/bin/mcp-fs")
        .expect("set_prop_persistent must route through the natural-key arm");
    // The in-memory mutation must land — this is what the UI sees.
    assert_eq!(config.mcp.servers[0].command, "/usr/local/bin/mcp-fs");

    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("/usr/local/bin/mcp-fs"),
        "save_dirty must write the new command for `mcp.servers.fs.command`; \
         on-disk file still reads:\n{written}"
    );
    assert!(
        !written.contains("/usr/bin/mcp-fs"),
        "stale command must be overwritten; got:\n{written}"
    );
    // The natural-key field itself must stay on disk — losing
    // `name` would orphan every other field in the [[mcp.servers]]
    // entry and break subsequent loads.
    assert!(
        written.contains("name = \"fs\""),
        "natural-key `name` must survive the incremental save; got:\n{written}"
    );
}

/// `cost.rates.providers.models.<type>` is a
/// `#[resource_key]` `HashMap<String, ModelCostRates>` — its key is a
/// model id, not an operator-chosen alias, and may contain dots
/// (`gpt-4.1`). Before the map-key-section branch landed,
/// `apply_dirty_path` blindly split the dirty path on `.`, fragmenting
/// the key into `gpt-4` + `1`, finding neither in the in-memory table,
/// and silently deleting (no-op-ing) the write instead of persisting
/// it.
#[test]
async fn save_dirty_persists_dotted_map_key_field() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [cost.rates.providers.models.openai.\"gpt-4.1\"]\n\
         input_per_mtok = 1.0\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.cost.rates.providers.models.openai.insert(
        "gpt-4.1".to_string(),
        ModelCostRates {
            input_per_mtok: Some(1.0),
            ..Default::default()
        },
    );

    config
        .set_prop_persistent(
            "cost.rates.providers.models.openai.gpt-4.1.input_per_mtok",
            "9.9",
        )
        .expect("set_prop_persistent must route through the dotted resource key");
    assert_eq!(
        config
            .cost
            .rates
            .providers
            .models
            .openai
            .get("gpt-4.1")
            .and_then(|r| r.input_per_mtok),
        Some(9.9)
    );

    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("9.9"),
        "save_dirty must write the new input_per_mtok for the dotted model key; \
         on-disk file still reads:\n{written}"
    );
    assert!(
        written.contains("\"gpt-4.1\""),
        "the dotted key must survive as one quoted TOML key, not be split apart; got:\n{written}"
    );

    let reloaded: Config = toml::from_str(&written)
        .unwrap_or_else(|e| panic!("rewritten config must reparse: {e}\n---\n{written}"));
    assert_eq!(
        reloaded
            .cost
            .rates
            .providers
            .models
            .openai
            .get("gpt-4.1")
            .and_then(|r| r.input_per_mtok),
        Some(9.9),
        "reloaded config must see the persisted value; got:\n{written}"
    );
}

/// Control for `save_dirty_persists_dotted_map_key_field`: a dot-free
/// resource key must keep working through the same map-key-section
/// branch (it's no longer special-cased out — every `HashMap<String,
/// T>` write now routes through `apply_dirty_map_key_path`).
#[test]
async fn save_dirty_persists_dot_free_map_key_field() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [cost.rates.providers.models.openai.gpt-4o]\n\
         input_per_mtok = 1.0\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.cost.rates.providers.models.openai.insert(
        "gpt-4o".to_string(),
        ModelCostRates {
            input_per_mtok: Some(1.0),
            ..Default::default()
        },
    );

    config
        .set_prop_persistent(
            "cost.rates.providers.models.openai.gpt-4o.input_per_mtok",
            "5.5",
        )
        .unwrap();
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("5.5"),
        "dot-free map key writes must still persist; got:\n{written}"
    );
}

/// Delete path: removing a dotted map key in memory
/// (`delete_map_key`) must still drop the matching on-disk table.
/// Before this fix the on-disk key was never located because the
/// dirty path (`<section>.<key>`, no inner suffix) is exactly the
/// shape the naive `raw.split('.')` walker mis-parses.
#[test]
async fn save_dirty_removes_dotted_map_key_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [cost.rates.providers.models.openai.\"gpt-4.1\"]\n\
         input_per_mtok = 1.0\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.cost.rates.providers.models.openai.insert(
        "gpt-4.1".to_string(),
        ModelCostRates {
            input_per_mtok: Some(1.0),
            ..Default::default()
        },
    );

    let removed = config
        .delete_map_key("cost.rates.providers.models.openai", "gpt-4.1")
        .expect("delete_map_key must accept the dotted resource key");
    assert!(removed);
    config.mark_dirty("cost.rates.providers.models.openai.gpt-4.1");

    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !written.contains("gpt-4.1"),
        "deleted dotted map key must be dropped from disk; got:\n{written}"
    );
}

/// ZeroClaw never writes inline tables but loads hand-edited ones
/// fine, so a map-key section shaped `openai = { "gpt-4.1" = { ... } }`
/// parses as `Item::Value(Value::InlineTable)` — invisible to a
/// `Table`-only doc walk. Both halves must go through `TableLike`:
/// resolving the key (read side) so the batch doesn't abort, and
/// actually removing it from the inline table (write side) so the
/// deletion isn't reported as successful while the key survives on
/// disk — a mutable traversal that only understood `Table` would
/// resolve the key, then silently return without deleting anything.
#[test]
async fn save_dirty_resolves_map_key_from_inline_table_on_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [cost.rates.providers.models]\n\
         openai = {{ \"gpt-4.1\" = {{ input_per_mtok = 1.0 }} }}\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    // Key absent from memory (the delete half): resolution can only
    // come from the on-disk doc, i.e. through the inline table.
    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.mark_dirty("cost.rates.providers.models.openai.gpt-4.1");

    config
        .save_dirty()
        .await
        .expect("a key living only in an on-disk inline table must resolve, not abort the save");

    let written = std::fs::read_to_string(&config_path).unwrap();
    let doc = written
        .parse::<toml_edit::DocumentMut>()
        .unwrap_or_else(|e| panic!("rewritten config must reparse: {e}\n---\n{written}"));

    // The delete must actually take effect on disk, not just resolve
    // and then no-op: reporting `Ok(())` while the key survives is
    // the silent-persistence failure this section of `save_dirty`
    // exists to eliminate.
    assert!(
        !written.contains("gpt-4.1"),
        "deleted key must not remain anywhere in the rewritten file; got:\n{written}"
    );
    let openai_item = doc
        .get("cost")
        .and_then(|i| i.get("rates"))
        .and_then(|i| i.get("providers"))
        .and_then(|i| i.get("models"))
        .and_then(|i| i.get("openai"))
        .expect("openai entry must survive the delete of its only sub-key");
    let openai_table = openai_item
        .as_table_like()
        .expect("openai entry must still be table-like (Table or InlineTable) after the delete");
    assert!(
        !openai_table.contains_key("gpt-4.1"),
        "gpt-4.1 must be removed from the on-disk openai inline table; got:\n{written}"
    );
}

/// Write half of the inline-table fix: a value living inside a
/// hand-edited inline table must be updated in place, not just
/// resolved-and-ignored. Same doc shape as
/// `save_dirty_resolves_map_key_from_inline_table_on_disk`, but the
/// key stays in memory with a changed value instead of being dropped.
#[test]
async fn save_dirty_persists_write_into_inline_table_on_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [cost.rates.providers.models]\n\
         openai = {{ \"gpt-4.1\" = {{ input_per_mtok = 1.0 }} }}\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.cost.rates.providers.models.openai.insert(
        "gpt-4.1".to_string(),
        ModelCostRates {
            input_per_mtok: Some(1.0),
            ..Default::default()
        },
    );
    config
        .set_prop_persistent(
            "cost.rates.providers.models.openai.gpt-4.1.input_per_mtok",
            "9.9",
        )
        .expect("set_prop_persistent must route through the dotted resource key");

    config.save_dirty().await.expect(
        "a write into a key resolved through an on-disk inline table must not abort the save",
    );

    let written = std::fs::read_to_string(&config_path).unwrap();
    let doc = written
        .parse::<toml_edit::DocumentMut>()
        .unwrap_or_else(|e| panic!("rewritten config must reparse: {e}\n---\n{written}"));

    assert!(
        written.contains("9.9"),
        "save_dirty must write the new input_per_mtok into the inline table; got:\n{written}"
    );
    let rates_item = doc
        .get("cost")
        .and_then(|i| i.get("rates"))
        .and_then(|i| i.get("providers"))
        .and_then(|i| i.get("models"))
        .and_then(|i| i.get("openai"))
        .and_then(|i| i.get("gpt-4.1"))
        .and_then(|i| i.get("input_per_mtok"))
        .expect("input_per_mtok must survive as a leaf inside the on-disk inline table");
    assert_eq!(
        rates_item.as_float(),
        Some(9.9),
        "the on-disk inline-table leaf must reflect the written value; got:\n{written}"
    );
}

/// Loud-failure guard: a dirty path that resolves to a
/// map-key section but whose key exists in neither the in-memory
/// config nor the on-disk doc must fail `save_dirty` instead of
/// silently no-op-ing (the original bug's symptom — reported success,
/// nothing written).
#[test]
async fn save_dirty_errors_on_unresolvable_map_key_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [observability]\n\
         backend = \"none\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    // Neither `full_table` nor the on-disk doc has a
    // `cost.rates.providers.models.openai.ghost-model` entry — mark it
    // dirty directly the way a stale/duplicate `mark_dirty` call
    // (e.g. a bug elsewhere, or a manually crafted RPC) would.
    config.mark_dirty("cost.rates.providers.models.openai.ghost-model.input_per_mtok");

    let err = config
        .save_dirty()
        .await
        .expect_err("an unresolvable map-key dirty path must fail loudly, not no-op");
    let msg = err.to_string();
    assert!(
        msg.contains("cost.rates.providers.models.openai.ghost-model.input_per_mtok"),
        "error must name the offending dirty path; got: {msg}"
    );
}

/// `create_map_key("mcp.servers", "new")` followed by per-field
/// edits must produce a complete `[[mcp.servers]]` table on disk —
/// including the seeded natural-key field. This is the path the
/// dashboard's `+ Add MCP server` affordance walks: insert, then
/// edit `command` / `transport` etc.
#[test]
async fn save_dirty_writes_new_mcp_server_added_via_create_map_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Seed an unrelated existing entry so we exercise the
    // append-into-existing-array path (not the create-array-from-
    // scratch path). Both matter; the empty-doc case is covered by
    // `save` instead of `save_dirty` (see the `!config_path.exists()`
    // branch at the top of `save_dirty`).
    let seed = format!(
        "schema_version = {}\n\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         transport = \"stdio\"\n\
         command = \"/usr/bin/mcp-fs\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    });

    // The handle_config_map_key_create dispatch path runs
    // create_map_key + mark_dirty(`<section>.<key>`). Replicate
    // that here so we're testing the same wire sequence.
    let created = config
        .create_map_key("mcp.servers", "github")
        .expect("create_map_key on a natural-key section must succeed");
    assert!(created);
    config.mark_dirty("mcp.servers.github");

    // Per-field edit on the freshly-added entry.
    config
        .set_prop_persistent("mcp.servers.github.transport", "http")
        .expect("set transport on freshly-added entry must route");
    config
        .set_prop_persistent("mcp.servers.github.url", "https://mcp.example/")
        .expect("set url on freshly-added entry must route");

    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    // Both entries survive, with their distinct fields.
    assert!(
        written.contains("name = \"fs\""),
        "pre-existing entry must survive; got:\n{written}"
    );
    assert!(
        written.contains("name = \"github\""),
        "new entry's natural-key field must land on disk; got:\n{written}"
    );
    assert!(
        written.contains("transport = \"http\""),
        "per-field edit on the new entry must land; got:\n{written}"
    );
    assert!(
        written.contains("url = \"https://mcp.example/\""),
        "second per-field edit on the new entry must land; got:\n{written}"
    );
    // Round-trip: parsing the written file must yield exactly the
    // shape we built up in memory. Catches mis-shaped output (e.g.
    // a nested `mcp.servers.github` inline table sneaking in instead
    // of a second `[[mcp.servers]]`).
    let reparsed: Config = toml::from_str(&written).unwrap();
    assert_eq!(reparsed.mcp.servers.len(), 2);
    let gh = reparsed
        .mcp
        .servers
        .iter()
        .find(|s| s.name == "github")
        .expect("reparse must surface the new entry by natural key");
    assert_eq!(gh.transport, McpTransport::Http);
    assert_eq!(gh.url.as_deref(), Some("https://mcp.example/"));
}

/// `rename_map_key("mcp.servers", "fs", "filesystem")` rewrites the
/// in-memory entry's `name` field in place and marks BOTH aliases
/// dirty. The incremental writer must update the matching
/// `[[mcp.servers]]` entry's `name` to the new value without
/// leaving a stale duplicate behind.
#[test]
async fn save_dirty_persists_mcp_server_rename_via_natural_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         transport = \"stdio\"\n\
         command = \"/usr/bin/mcp-fs\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    });

    // Mirror handle_config_map_key_rename: rename, then mark both
    // the old and new aliases dirty.
    let renamed = config
        .rename_map_key("mcp.servers", "fs", "filesystem")
        .expect("rename of a unique alias must succeed");
    assert!(renamed);
    config.mark_dirty("mcp.servers.fs");
    config.mark_dirty("mcp.servers.filesystem");

    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("name = \"filesystem\""),
        "rename target must land on disk; got:\n{written}"
    );
    assert!(
        !written.contains("name = \"fs\""),
        "stale rename source must NOT remain on disk; got:\n{written}"
    );
    // Other fields on the renamed entry are preserved.
    assert!(
        written.contains("command = \"/usr/bin/mcp-fs\""),
        "rename must preserve sibling fields on the entry; got:\n{written}"
    );

    let reparsed: Config = toml::from_str(&written).unwrap();
    assert_eq!(reparsed.mcp.servers.len(), 1);
    assert_eq!(reparsed.mcp.servers[0].name, "filesystem");
    assert_eq!(reparsed.mcp.servers[0].command, "/usr/bin/mcp-fs");
}

/// `delete_map_key("mcp.servers", "fs")` removes the in-memory
/// entry and marks the alias dirty. The incremental writer must
/// drop the corresponding `[[mcp.servers]]` entry from disk,
/// dropping the array slot entirely when no entries remain so the
/// file doesn't carry a dangling `[[mcp.servers]]` section header.
#[test]
async fn save_dirty_removes_mcp_server_deleted_via_natural_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Two entries on disk; we'll delete one and assert the other
    // survives.
    let seed = format!(
        "schema_version = {}\n\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         transport = \"stdio\"\n\
         command = \"/usr/bin/mcp-fs\"\n\n\
         [[mcp.servers]]\n\
         name = \"github\"\n\
         transport = \"http\"\n\
         url = \"https://mcp.example/\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    });
    config.mcp.servers.push(McpServerConfig {
        name: "github".into(),
        transport: McpTransport::Http,
        url: Some("https://mcp.example/".to_string()),
        ..Default::default()
    });

    let deleted = config
        .delete_map_key("mcp.servers", "fs")
        .expect("delete by natural key must resolve");
    assert!(deleted);
    config.mark_dirty("mcp.servers.fs");

    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !written.contains("name = \"fs\""),
        "deleted entry must not survive incremental save; got:\n{written}"
    );
    assert!(
        written.contains("name = \"github\""),
        "untouched sibling entry must survive; got:\n{written}"
    );

    let reparsed: Config = toml::from_str(&written).unwrap();
    assert_eq!(reparsed.mcp.servers.len(), 1);
    assert_eq!(reparsed.mcp.servers[0].name, "github");
}

/// Deleting the last `[[mcp.servers]]` entry drops the array slot
/// entirely. A vestigial empty `[[mcp.servers]]` header would
/// reparse as a single default-shaped element with an empty
/// natural-key field, which the validator then rejects on next
/// load — actively breaking the file the writer just produced.
#[test]
async fn save_dirty_drops_array_header_when_last_mcp_server_removed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         transport = \"stdio\"\n\
         command = \"/usr/bin/mcp-fs\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    });

    config.delete_map_key("mcp.servers", "fs").unwrap();
    config.mark_dirty("mcp.servers.fs");
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !written.contains("[[mcp.servers]]"),
        "empty array header must be dropped, otherwise it reparses as a default entry; got:\n{written}"
    );
    let reparsed: Config = toml::from_str(&written).unwrap();
    assert!(reparsed.mcp.servers.is_empty());
}

/// `Config::map_key_sections()` must surface the natural-key field
/// for `mcp.servers`. This is the metadata `apply_dirty_path` reads
/// to decide whether to take the array-of-tables branch; if the
/// derive ever stops emitting it for `#[natural_key = "..."]` Vec
/// fields, the dirty-path writer falls back to the broken
/// Table-only walker and silently drops MCP edits on the floor
/// again. Lock the contract here.
#[test]
async fn map_key_sections_exposes_natural_key_for_mcp_servers() {
    let sections = Config::map_key_sections();
    let entry = sections
        .iter()
        .find(|s| s.path == "mcp.servers")
        .expect("mcp.servers must be discoverable in map_key_sections()");
    assert_eq!(entry.kind, crate::traits::MapKeyKind::List);
    assert_eq!(
        entry.natural_key,
        Some("name"),
        "natural_key must mirror the `#[natural_key = \"name\"]` attribute \
         on McpConfig::servers; the dirty-path writer keys off this to take \
         the array-of-tables branch"
    );

    // Sanity: a representative HashMap section (alias IS the TOML
    // key) carries `natural_key: None`. The dirty-path writer's
    // branch decision falls through to the generic Table walker
    // for these.
    let anthropic = sections
        .iter()
        .find(|s| s.path == "providers.models.anthropic")
        .expect("providers.models.anthropic must surface as a HashMap-backed map-keyed section");
    assert_eq!(anthropic.kind, crate::traits::MapKeyKind::Map);
    assert_eq!(anthropic.natural_key, None);
}

/// `model_routes` and `embedding_routes` are `#[nested]` Vec fields
/// with `#[natural_key = "hint"]` — they must surface in
/// `map_key_sections()` as `List` entries so the dashboard and the
/// incremental TOML writer can address individual route entries.
#[tokio::test]
async fn map_key_sections_exposes_natural_key_for_model_routes() {
    let sections = Config::map_key_sections();
    let entry = sections
        .iter()
        .find(|s| s.path == "model_routes")
        .expect("model_routes must be discoverable in map_key_sections()");
    assert_eq!(entry.kind, crate::traits::MapKeyKind::List);
    assert_eq!(
        entry.natural_key,
        Some("hint"),
        "natural_key must mirror the `#[natural_key = \"hint\"]` attribute \
         on Config::model_routes; the dirty-path writer keys off this to take \
         the array-of-tables branch"
    );
}

#[tokio::test]
async fn map_key_sections_exposes_natural_key_for_embedding_routes() {
    let sections = Config::map_key_sections();
    let entry = sections
        .iter()
        .find(|s| s.path == "embedding_routes")
        .expect("embedding_routes must be discoverable in map_key_sections()");
    assert_eq!(entry.kind, crate::traits::MapKeyKind::List);
    assert_eq!(
        entry.natural_key,
        Some("hint"),
        "natural_key must mirror the `#[natural_key = \"hint\"]` attribute \
         on Config::embedding_routes; the dirty-path writer keys off this to take \
         the array-of-tables branch"
    );
}

/// A dirty path with a kebab-shaped inner field (e.g.
/// `mcp.servers.fs.tool-timeout-secs`) must resolve through the
/// shared `resolve_dirty_segments` helper inside the natural-key
/// branch the same way the top-level Table walker does — landing
/// on the snake `tool_timeout_secs` field on disk. This pins the
/// dash-aware resolution that's load-bearing for any future
/// natural-key struct field whose snake_case name is multi-word.
/// Without this, a UI client that emits kebab field names would
/// recreate the exact symptom the PR fixes (memory updates, disk
/// stays stale) for any such field.
#[test]
async fn save_dirty_persists_mcp_server_kebab_inner_field_via_natural_key() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    let seed = format!(
        "schema_version = {}\n\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         transport = \"stdio\"\n\
         command = \"/usr/bin/mcp-fs\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        tool_timeout_secs: Some(45),
        ..Default::default()
    });

    // mark_dirty directly with a kebab leaf segment so the
    // dash-aware resolver inside `resolve_dirty_segments` is the
    // only thing that can possibly land this on disk. set_prop /
    // set_prop_persistent route through the macro which has its
    // own snake-only field-name lookup; this test isolates the
    // writer-side resolution. The in-memory mutation above
    // simulates the dispatcher having already routed the
    // set_prop side; what we're testing here is the save side.
    config.mark_dirty("mcp.servers.fs.tool-timeout-secs");
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("tool_timeout_secs = 45"),
        "kebab dirty segment must resolve to the snake on-disk field; \
         got:\n{written}"
    );
    assert!(
        !written.contains("tool-timeout-secs"),
        "kebab field name must never appear on disk; got:\n{written}"
    );
    // Other fields on the entry survive the targeted edit.
    let reparsed: Config = toml::from_str(&written).unwrap();
    assert_eq!(reparsed.mcp.servers.len(), 1);
    assert_eq!(reparsed.mcp.servers[0].name, "fs");
    assert_eq!(reparsed.mcp.servers[0].command, "/usr/bin/mcp-fs");
    assert_eq!(reparsed.mcp.servers[0].tool_timeout_secs, Some(45));
}

/// An explicit per-field unset (the in-memory field reverts to
/// `None`, but the dirty path still names that specific inner
/// field rather than the whole element) must drive the case-1
/// `mem missing → delete` branch — removing the field from the
/// `[[mcp.servers]]` entry on disk without touching its siblings.
/// The pre-existing whole-element delete test covers a different
/// path (the rename source / `delete_map_key`); this one pins
/// the per-field delete branch independently. Without it, a
/// future refactor that collapses case-1's `None` branch into
/// the whole-element path would silently break "clear this field"
/// edits — the field would survive on disk while showing as
/// unset in the UI.
#[test]
async fn save_dirty_unset_per_field_drops_field_from_mcp_server_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Seed an entry with tool_timeout_secs set — this is the
    // field we'll unset via a per-field dirty path.
    let seed = format!(
        "schema_version = {}\n\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         transport = \"stdio\"\n\
         command = \"/usr/bin/mcp-fs\"\n\
         tool_timeout_secs = 45\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    // In-memory mirror without the optional field set — i.e. the
    // UI user just cleared `tool_timeout_secs`.
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        tool_timeout_secs: None,
        ..Default::default()
    });
    // Per-field dirty path — exactly what a UI "clear this
    // field" affordance emits. The whole-element bare alias
    // `mcp.servers.fs` is intentionally NOT marked: the bare
    // alias is the rename/delete shape; the bug-prone case is
    // the per-field one.
    config.mark_dirty("mcp.servers.fs.tool_timeout_secs");
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !written.contains("tool_timeout_secs"),
        "explicit per-field unset must drop the field from disk; got:\n{written}"
    );
    // Siblings survive: the entry isn't deleted, just the one
    // field, and the natural-key field itself must stay so
    // subsequent loads still know which alias this entry is.
    assert!(
        written.contains("name = \"fs\""),
        "natural-key field must survive a per-field unset; got:\n{written}"
    );
    assert!(
        written.contains("command = \"/usr/bin/mcp-fs\""),
        "sibling fields must survive a per-field unset; got:\n{written}"
    );

    let reparsed: Config = toml::from_str(&written).unwrap();
    assert_eq!(reparsed.mcp.servers.len(), 1);
    assert_eq!(reparsed.mcp.servers[0].name, "fs");
    assert_eq!(reparsed.mcp.servers[0].tool_timeout_secs, None);
}

/// When the on-disk node at `mcp.servers` exists but has the
/// wrong kind — e.g. a hand-edited `mcp.servers = "foo"`, or an
/// inline-array-of-tables literal `servers = [{ ... }]` that
/// `toml_edit` parses as `Item::Value(Value::Array)` rather than
/// `Item::ArrayOfTables` — the writer must refuse to clobber
/// rather than data-loss the user's hand-edit. The bail is
/// observable (a `WARN`-level log event), but here we just pin
/// the don't-clobber behavior at the disk level: the original
/// shape survives the save unchanged. This is the explicit
/// contract test for the wrong-kind bail surface; without it,
/// a refactor that "fixes" the bail by overwriting silently
/// would corrupt every operator who has either of these
/// (otherwise valid) TOML shapes in their config.
#[test]
async fn save_dirty_refuses_to_clobber_wrong_kind_mcp_servers_node() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Hand-edited file: operator wrote `mcp.servers` as a scalar.
    // This is invalid against the schema but is what
    // `Item::Value(Value::String)` looks like in toml_edit, and
    // it's representative of the wrong-kind case (the same bail
    // path also fires for `Item::Value(Value::Array)` inline
    // arrays). Schema validation would reject this on load; the
    // test forces the writer to face the shape regardless.
    let seed = format!(
        "schema_version = {}\n\n\
         [mcp]\n\
         servers = \"hand-edited-scalar\"\n",
        crate::migration::CURRENT_SCHEMA_VERSION
    );
    std::fs::write(&config_path, &seed).unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    // In-memory has a real server; the per-field edit would
    // normally land on `[[mcp.servers]]` on disk.
    config.mcp.servers.push(McpServerConfig {
        name: "fs".into(),
        transport: McpTransport::Stdio,
        command: "/usr/bin/mcp-fs".into(),
        ..Default::default()
    });
    config.mark_dirty("mcp.servers.fs.command");

    // The save itself must succeed — bail is a no-op for the
    // wrong-kind node, not an error.
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    // The hand-edited scalar shape survives untouched: we'd
    // rather a load-time validation error the operator sees than
    // a silent overwrite of their (possibly intentional) file
    // surgery.
    assert!(
        written.contains("servers = \"hand-edited-scalar\""),
        "wrong-kind `mcp.servers` node must survive the save unchanged; \
         got:\n{written}"
    );
    assert!(
        !written.contains("[[mcp.servers]]"),
        "writer must not synthesize an array-of-tables next to a scalar \
         hand-edit; got:\n{written}"
    );
    assert!(
        !written.contains("/usr/bin/mcp-fs"),
        "in-memory command must not leak past a wrong-kind bail; \
         got:\n{written}"
    );
}

#[test]
async fn collect_warnings_flags_wire_api_on_fixed_protocol_family() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    // mistral has a fixed wire protocol and ignores wire_api.
    config
        .providers
        .models
        .ensure("mistral", "primary")
        .unwrap()
        .wire_api = Some(WireApi::Responses);
    // custom honors wire_api — must NOT warn.
    config
        .providers
        .models
        .ensure("custom", "vllm")
        .unwrap()
        .wire_api = Some(WireApi::Responses);

    let warnings = config.collect_warnings();
    assert_eq!(warnings.len(), 1, "exactly the mistral entry should warn");
    let w = &warnings[0];
    assert_eq!(w.code, "wire_api_not_supported_for_family");
    assert_eq!(w.path, "providers.models.mistral.primary.wire_api");
    assert!(
        !warnings.iter().any(|w| w.path.contains("custom.vllm")),
        "custom honors wire_api and must not warn",
    );
}

#[cfg(unix)]
#[test]
async fn world_readable_config_is_detectable() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Create a config file with intentionally loose permissions
    std::fs::write(&config_path, "# test config").unwrap();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let meta = std::fs::metadata(&config_path).unwrap();
    let mode = meta.permissions().mode();
    assert!(
        mode & 0o004 != 0,
        "Test setup: file should be world-readable (mode {mode:o})"
    );
}

#[test]
async fn transcription_max_audio_bytes_round_trips_through_prop_path() {
    let mut config = Config::default();

    assert_eq!(
        config
            .get_prop("transcription.max_audio_bytes")
            .unwrap()
            .as_str(),
        "<unset>"
    );

    config
        .set_prop("transcription.max_audio_bytes", "65536")
        .unwrap();
    assert_eq!(config.transcription.max_audio_bytes, Some(65_536));
    assert_eq!(
        config.get_prop("transcription.max_audio_bytes").unwrap(),
        "65536"
    );

    config
        .set_prop("transcription.max_audio_bytes", "")
        .unwrap();
    assert!(config.transcription.max_audio_bytes.is_none());
    assert_eq!(
        config.get_prop("transcription.max_audio_bytes").unwrap(),
        "<unset>"
    );
}

#[test]
async fn config_validate_rejects_zero_transcription_max_audio_bytes() {
    let mut config = Config::default();
    config.transcription.max_audio_bytes = Some(0);

    let err = config.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("transcription.max_audio_bytes must be greater than zero"),
        "got: {err}"
    );
}

#[test]
async fn config_without_transcription_uses_defaults() {
    let toml_str = r#"
        default_model_provider = "openrouter"
        default_model = "test-model"
        default_temperature = 0.7
    "#;
    let parsed = parse_test_config(toml_str);
    assert!(!parsed.transcription.enabled);
    assert_eq!(parsed.transcription.max_duration_secs, 120);
}

#[test]
async fn security_defaults_are_backward_compatible() {
    let parsed = parse_test_config(
        r#"
default_model_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7
"#,
    );

    assert!(!parsed.security.otp.enabled);
    assert_eq!(parsed.security.otp.method, OtpMethod::Totp);
    assert!(!parsed.security.estop.enabled);
    assert!(parsed.security.estop.require_otp_to_resume);
    assert!(parsed.security.leak_detection.enabled);
    assert_eq!(parsed.security.leak_detection.sensitivity, 0.7);
    assert!(parsed.security.leak_detection.high_entropy_tokens);
}

#[test]
async fn security_toml_parses_otp_and_estop_sections() {
    let parsed = parse_test_config(
        r#"
default_model_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7

[security.otp]
enabled = true
method = "totp"
token_ttl_secs = 30
cache_valid_secs = 120
gated_actions = ["shell", "browser_open"]
gated_domains = ["*.chase.com", "accounts.google.com"]
gated_domain_categories = ["banking"]

[security.estop]
enabled = true
state_file = "~/.zeroclaw/estop-state.json"
require_otp_to_resume = true
"#,
    );

    assert!(parsed.security.otp.enabled);
    assert!(parsed.security.estop.enabled);
    assert_eq!(parsed.security.otp.gated_actions.len(), 2);
    assert_eq!(parsed.security.otp.gated_domains.len(), 2);
    parsed.validate().unwrap();
}

#[test]
async fn security_toml_parses_leak_detection_section() {
    let parsed = parse_test_config(
        r#"
default_model_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7

[security.leak_detection]
enabled = false
sensitivity = 0.35
high_entropy_tokens = false
"#,
    );

    assert!(!parsed.security.leak_detection.enabled);
    assert_eq!(parsed.security.leak_detection.sensitivity, 0.35);
    assert!(!parsed.security.leak_detection.high_entropy_tokens);
    parsed.validate().unwrap();
}

#[test]
async fn security_validation_rejects_out_of_range_leak_detection_sensitivity() {
    let mut config = Config::default();
    config.security.leak_detection.sensitivity = 1.5;

    let err = config
        .validate()
        .expect_err("expected invalid leak-detection sensitivity");
    assert!(
        err.to_string()
            .contains("security.leak_detection.sensitivity"),
        "got: {err}"
    );
}

#[test]
async fn security_validation_rejects_invalid_domain_glob() {
    let mut config = Config::default();
    config.security.otp.gated_domains = vec!["bad domain.com".into()];

    let err = config.validate().expect_err("expected invalid domain glob");
    assert!(err.to_string().contains("gated_domains"));
}

#[test]
async fn security_validation_accepts_all_default_gated_actions_without_warning() {
    let mut config = Config::default();
    config.security.otp.gated_actions = default_otp_gated_actions();

    config
        .validate()
        .expect("the canonical default gated actions must validate clean");
}

#[test]
async fn security_validation_accepts_unknown_gated_action_but_does_not_bail() {
    // An unknown but well-formed action name must not reject the config
    // during the deprecation window: `gated_actions` is deprecated and
    // never enforced (no OTP action-gating exists), and the operator's
    // whole config must keep parsing. The runtime emits a WARN naming
    // the unknown entry, and `collect_warnings` emits the
    // `otp_action_gating_unsupported` deprecation diagnostic. This
    // asserts the warn-and-continue contract: load succeeds.
    let mut config = Config::default();
    config.security.otp.gated_actions = vec!["kubectl_write".into()];

    config
        .validate()
        .expect("an unknown gated action must warn, not reject the config");
}

#[test]
async fn collect_warnings_flags_deprecated_otp_action_gating_knobs() {
    // The four action-gating knobs are misleading config: they must
    // keep parsing/validating (compat) but every non-default value
    // must surface an explicit deprecation diagnostic naming the
    // knob, stating it is not enforced, and naming the intended path.
    let mut config = Config::default();
    config.security.otp.gated_actions = vec!["shell".to_string()];
    config.security.otp.gated_domains = vec!["*.example.com".to_string()];
    config.security.otp.gated_domain_categories = vec!["banking".to_string()];
    config.security.otp.challenge_max_attempts = 5;

    config
        .validate()
        .expect("deprecated OTP gate knobs must keep validating (compat)");

    let warnings = config.collect_warnings();
    for path in [
        "security.otp.gated_actions",
        "security.otp.gated_domains",
        "security.otp.gated_domain_categories",
        "security.otp.challenge_max_attempts",
    ] {
        let warning = warnings
            .iter()
            .find(|w| w.path == path && w.code == "otp_action_gating_unsupported");
        let warning = warning.unwrap_or_else(|| {
            panic!("expected otp_action_gating_unsupported warning for {path}, got: {warnings:?}")
        });
        assert!(
            warning.message.contains("not enforced"),
            "warning for {path} must state the knob is not enforced: {}",
            warning.message
        );
    }

    // The deprecation message must also name the intended authorization
    // path so the diagnostic points somewhere real.
    let gated_actions_warning = warnings
        .iter()
        .find(|w| w.path == "security.otp.gated_actions")
        .expect("gated_actions warning");
    assert!(
        gated_actions_warning
            .message
            .contains("Tachi approval/grant")
    );
    assert!(gated_actions_warning.message.contains("Node"));
}

#[test]
async fn collect_warnings_stay_silent_for_live_otp_knobs_and_defaults() {
    // Live OTP mechanics must not be over-deprecated: a config that only
    // touches the genuinely consumed knobs (enabled, token_ttl_secs,
    // cache_valid_secs) produces no OTP warning, and the untouched
    // default config is silent as well. `method` is parsed but never
    // read at runtime; it is out of scope for this deprecation.
    let mut config = Config::default();
    config.security.otp.enabled = true;
    config.security.otp.token_ttl_secs = 60;
    config.security.otp.cache_valid_secs = 120;

    config.validate().expect("live OTP knobs must validate");
    for warnings in [
        config.collect_warnings(),
        Config::default().collect_warnings(),
    ] {
        assert!(
            warnings
                .iter()
                .all(|w| w.code != "otp_action_gating_unsupported"),
            "no OTP action-gating warning expected, got: {warnings:?}"
        );
    }
}

#[test]
async fn security_validation_still_rejects_malformed_gated_action() {
    // The unknown-name warn must not weaken the existing hard checks from
    // the charset/empty validation: a name with invalid characters still
    // bails rather than degrading to a warn.
    let mut config = Config::default();
    config.security.otp.gated_actions = vec!["kubectl write".into()];

    let err = config
        .validate()
        .expect_err("malformed gated action must still be rejected");
    assert!(err.to_string().contains("gated_actions"));
}

// The two `validate_*_transcription_default_provider` tests were removed
// alongside the deleted `TranscriptionConfig.default_transcription_provider`
// field in. there is no global default-provider concept; the equivalent
// dangling-reference enforcement now lives on the per-agent
// `agent.transcription_provider` field (see
// `Config::validate()` checks for `tts_provider` / `transcription_provider`).

#[tokio::test]
async fn channel_secret_telegram_bot_token_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "zeroclaw_test_tg_bot_token_{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).await.unwrap();

    let plaintext_token = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";

    let mut config = Config {
        data_dir: dir.join("workspace"),
        config_path: dir.join("config.toml"),
        ..Default::default()
    };
    config.channels.telegram.insert(
        "default".to_string(),
        TelegramConfig {
            enabled: true,
            bot_token: plaintext_token.into(),
            api_base_url: default_telegram_api_base_url(),
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: default_draft_update_interval_ms(),
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: default_telegram_approval_timeout_secs(),
            excluded_tools: vec![],
            reply_min_interval_secs: 0,
            reply_queue_depth_max: 0,
            debounce_ms: None,
        },
    );

    // Save (triggers encryption)
    config.save().await.unwrap();

    // Read raw TOML and verify plaintext token is NOT present
    let raw_toml = tokio::fs::read_to_string(&config.config_path)
        .await
        .unwrap();
    assert!(
        !raw_toml.contains(plaintext_token),
        "Saved TOML must not contain the plaintext bot_token"
    );

    // Parse stored TOML and verify the value is encrypted
    let stored: Config = toml::from_str(&raw_toml).unwrap();
    let stored_token = &stored.channels.telegram.get("default").unwrap().bot_token;
    assert!(
        crate::secrets::SecretStore::is_encrypted(stored_token),
        "Stored bot_token must be marked as encrypted"
    );

    // Decrypt and verify it matches the original plaintext
    let store = crate::secrets::SecretStore::new(&dir, true);
    assert_eq!(store.decrypt(stored_token).unwrap(), plaintext_token);

    // Simulate a full load: deserialize then decrypt (mirrors load_or_init logic)
    let mut loaded: Config = toml::from_str(&raw_toml).unwrap();
    loaded.config_path = dir.join("config.toml");
    let load_store = crate::secrets::SecretStore::new(&dir, loaded.secrets.encrypt);
    loaded.decrypt_secrets(&load_store).unwrap();
    assert_eq!(
        loaded.channels.telegram.get("default").unwrap().bot_token,
        plaintext_token,
        "Loaded bot_token must match the original plaintext after decryption"
    );

    let _ = fs::remove_dir_all(&dir).await;
}

#[test]
async fn security_validation_rejects_unknown_domain_category() {
    let mut config = Config::default();
    config.security.otp.gated_domain_categories = vec!["not_real".into()];

    let err = config
        .validate()
        .expect_err("expected unknown domain category");
    assert!(err.to_string().contains("gated_domain_categories"));
}

#[test]
async fn security_validation_rejects_zero_token_ttl() {
    let mut config = Config::default();
    config.security.otp.token_ttl_secs = 0;

    let err = config
        .validate()
        .expect_err("expected ttl validation failure");
    assert!(err.to_string().contains("token_ttl_secs"));
}

// ── MCP config validation ─────────────────────────────────────────────

fn stdio_server(name: &str, command: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransport::Stdio,
        command: command.to_string(),
        ..Default::default()
    }
}

fn http_server(name: &str, url: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransport::Http,
        url: Some(url.to_string()),
        ..Default::default()
    }
}

fn sse_server(name: &str, url: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransport::Sse,
        url: Some(url.to_string()),
        ..Default::default()
    }
}

#[test]
async fn validate_mcp_config_empty_servers_ok() {
    let cfg = McpConfig::default();
    assert!(validate_mcp_config(&cfg).is_ok());
}

#[test]
async fn validate_mcp_config_valid_stdio_ok() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![stdio_server("fs", "/usr/bin/mcp-fs")],
        ..Default::default()
    };
    assert!(validate_mcp_config(&cfg).is_ok());
}

#[test]
async fn validate_mcp_config_valid_http_ok() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![http_server("svc", "http://localhost:8080/mcp")],
        ..Default::default()
    };
    assert!(validate_mcp_config(&cfg).is_ok());
}

#[test]
async fn validate_mcp_config_valid_sse_ok() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![sse_server("svc", "https://example.com/events")],
        ..Default::default()
    };
    assert!(validate_mcp_config(&cfg).is_ok());
}

#[test]
async fn validate_mcp_config_rejects_empty_name() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![stdio_server("", "/usr/bin/tool")],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("empty name should fail");
    assert!(
        err.to_string().contains("name must not be empty"),
        "got: {err}"
    );
}

#[test]
async fn validate_mcp_config_rejects_whitespace_name() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![stdio_server("   ", "/usr/bin/tool")],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("whitespace name should fail");
    assert!(
        err.to_string().contains("name must not be empty"),
        "got: {err}"
    );
}

#[test]
async fn validate_mcp_config_rejects_duplicate_names() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![
            stdio_server("fs", "/usr/bin/mcp-a"),
            stdio_server("fs", "/usr/bin/mcp-b"),
        ],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("duplicate name should fail");
    assert!(err.to_string().contains("duplicate name"), "got: {err}");
}

#[test]
async fn validate_mcp_config_rejects_zero_timeout() {
    let mut server = stdio_server("fs", "/usr/bin/mcp-fs");
    server.tool_timeout_secs = Some(0);
    let cfg = McpConfig {
        enabled: true,
        servers: vec![server],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("zero timeout should fail");
    assert!(err.to_string().contains("greater than 0"), "got: {err}");
}

#[test]
async fn validate_mcp_config_rejects_timeout_exceeding_max() {
    let mut server = stdio_server("fs", "/usr/bin/mcp-fs");
    server.tool_timeout_secs = Some(MCP_MAX_TOOL_TIMEOUT_SECS + 1);
    let cfg = McpConfig {
        enabled: true,
        servers: vec![server],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("oversized timeout should fail");
    assert!(err.to_string().contains("exceeds max"), "got: {err}");
}

#[test]
async fn validate_mcp_config_allows_max_timeout_exactly() {
    let mut server = stdio_server("fs", "/usr/bin/mcp-fs");
    server.tool_timeout_secs = Some(MCP_MAX_TOOL_TIMEOUT_SECS);
    let cfg = McpConfig {
        enabled: true,
        servers: vec![server],
        ..Default::default()
    };
    assert!(validate_mcp_config(&cfg).is_ok());
}

#[test]
async fn validate_mcp_config_rejects_stdio_with_empty_command() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![stdio_server("fs", "")],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("empty command should fail");
    assert!(
        err.to_string().contains("requires non-empty command"),
        "got: {err}"
    );
}

#[test]
async fn validate_mcp_config_rejects_http_without_url() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![McpServerConfig {
            name: "svc".to_string(),
            transport: McpTransport::Http,
            url: None,
            ..Default::default()
        }],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("http without url should fail");
    assert!(err.to_string().contains("requires url"), "got: {err}");
}

#[test]
async fn validate_mcp_config_rejects_sse_without_url() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![McpServerConfig {
            name: "svc".to_string(),
            transport: McpTransport::Sse,
            url: None,
            ..Default::default()
        }],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("sse without url should fail");
    assert!(err.to_string().contains("requires url"), "got: {err}");
}

#[test]
async fn validate_mcp_config_rejects_non_http_scheme() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![http_server("svc", "ftp://example.com/mcp")],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("non-http scheme should fail");
    assert!(err.to_string().contains("http/https"), "got: {err}");
}

#[test]
async fn validate_mcp_config_rejects_invalid_url() {
    let cfg = McpConfig {
        enabled: true,
        servers: vec![http_server("svc", "not a url at all !!!")],
        ..Default::default()
    };
    let err = validate_mcp_config(&cfg).expect_err("invalid url should fail");
    assert!(err.to_string().contains("valid URL"), "got: {err}");
}

#[test]
async fn mcp_transport_required_leaf_is_the_single_source() {
    // The relationship every consumer reads. Wire names must match the
    // `rename_all = "lowercase"` serde representation.
    assert_eq!(McpTransport::Stdio.required_leaf(), "command");
    assert_eq!(McpTransport::Http.required_leaf(), "url");
    assert_eq!(McpTransport::Sse.required_leaf(), "url");
    assert_eq!(McpTransport::Stdio.wire_name(), "stdio");
    assert_eq!(McpTransport::Http.wire_name(), "http");
    assert_eq!(McpTransport::Sse.wire_name(), "sse");
    // The schema-derived enumerator must surface every variant, and
    // `wire_name` must agree with serde for each, or the emitted metadata
    // desyncs from the wire representation the form reads.
    let transports = mcp_transports();
    assert_eq!(
        transports,
        vec![McpTransport::Stdio, McpTransport::Http, McpTransport::Sse]
    );
    for transport in transports {
        let wire = serde_json::to_value(transport).expect("transport serializes");
        assert_eq!(
            wire,
            serde_json::Value::String(transport.wire_name().into())
        );
    }
}

#[test]
async fn validate_mcp_config_enforces_required_leaf_for_every_transport() {
    // Drift guard: whatever `required_leaf` declares, the validator must
    // actually enforce, so the schema metadata and the runtime check can
    // never disagree about which field a transport needs.
    for transport in mcp_transports() {
        let mut server = McpServerConfig {
            name: "svc".to_string(),
            transport,
            ..Default::default()
        };
        // Populate every required leaf except the one under test, leaving
        // the declared `required_leaf` empty, and confirm rejection.
        match transport.required_leaf() {
            "command" => server.command = String::new(),
            "url" => {
                server.command = "echo".to_string();
                server.url = None;
            }
            other => panic!("unhandled required leaf {other} for {transport:?}"),
        }
        let cfg = McpConfig {
            enabled: true,
            servers: vec![server],
            ..Default::default()
        };
        validate_mcp_config(&cfg).expect_err(&format!(
            "{transport:?} must reject an empty {}",
            transport.required_leaf()
        ));
    }
}

#[test]
async fn mcp_server_schema_emits_required_by_transport_metadata() {
    // The config form reads `x-required-by-transport` off the
    // `McpServerConfig` element schema; assert it is present and projects
    // exactly `McpTransport::required_leaf` for every variant.
    #[cfg(feature = "schema-export")]
    let schema = schemars::schema_for!(McpServerConfig);
    let schema_json = serde_json::to_value(&schema).expect("schema serializes to json");
    let map = schema_json
        .get("x-required-by-transport")
        .and_then(serde_json::Value::as_object)
        .expect("schema should carry the x-required-by-transport extension");
    let transports = mcp_transports();
    assert_eq!(map.len(), transports.len());
    for transport in transports {
        assert_eq!(
            map.get(transport.wire_name())
                .and_then(serde_json::Value::as_str),
            Some(transport.required_leaf()),
            "metadata for {transport:?} must match required_leaf",
        );
    }
}

#[test]
async fn full_config_schema_nests_required_by_transport_on_mcp_server_def() {
    // The gateway serves `schema_for!(Config)` to the Operator Console; the
    // extension must survive into that full document (under `$defs`) where
    // the form resolves the `mcp.servers` element type, not just on the
    // standalone struct schema.
    #[cfg(feature = "schema-export")]
    let schema = schemars::schema_for!(Config);
    let schema_json = serde_json::to_value(&schema).expect("schema serializes to json");
    assert!(schema_json.pointer("/properties/eval").is_none());
    assert!(
        schema_json
            .pointer("/$defs/RuntimeProfileConfig/properties/eval")
            .is_some()
    );

    fn find_extension(
        value: &serde_json::Value,
    ) -> Option<&serde_json::Map<String, serde_json::Value>> {
        match value {
            serde_json::Value::Object(obj) => {
                if let Some(found) = obj
                    .get("x-required-by-transport")
                    .and_then(serde_json::Value::as_object)
                {
                    return Some(found);
                }
                obj.values().find_map(find_extension)
            }
            serde_json::Value::Array(items) => items.iter().find_map(find_extension),
            _ => None,
        }
    }

    let map = find_extension(&schema_json)
        .expect("full Config schema should carry x-required-by-transport on the mcp server def");
    for transport in mcp_transports() {
        assert_eq!(
            map.get(transport.wire_name())
                .and_then(serde_json::Value::as_str),
            Some(transport.required_leaf()),
        );
    }
}

#[test]
async fn mcp_config_parsed_missing_flags_uses_enabled_eager_defaults() {
    let raw = r#"
[mcp]

[[mcp.servers]]
name = "svc"
transport = "http"
url = "http://localhost:8080/mcp"
"#;
    let parsed = parse_test_config(raw);
    assert!(parsed.mcp.enabled);
    assert!(!parsed.mcp.deferred_loading);
    assert_eq!(parsed.mcp.servers.len(), 1);
}

#[test]
async fn mcp_config_explicit_disable_and_deferred_loading_are_respected() {
    let raw = r#"
[mcp]
enabled = false
deferred_loading = true

[[mcp.servers]]
name = "svc"
transport = "http"
url = "http://localhost:8080/mcp"
"#;
    let parsed = parse_test_config(raw);
    assert!(!parsed.mcp.enabled);
    assert!(parsed.mcp.deferred_loading);
    assert_eq!(parsed.mcp.servers.len(), 1);
}

#[test]
async fn mcp_transport_serde_roundtrip_lowercase() {
    let cases = [
        (McpTransport::Stdio, "\"stdio\""),
        (McpTransport::Http, "\"http\""),
        (McpTransport::Sse, "\"sse\""),
    ];
    for (variant, expected_json) in &cases {
        let serialized = serde_json::to_string(variant).expect("serialize");
        assert_eq!(&serialized, expected_json, "variant: {variant:?}");
        let deserialized: McpTransport = serde_json::from_str(expected_json).expect("deserialize");
        assert_eq!(&deserialized, variant);
    }
}

#[tokio::test]
async fn nevis_client_secret_encrypt_decrypt_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "zeroclaw_test_nevis_secret_{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).await.unwrap();

    let plaintext_secret = "nevis-test-client-secret-value";

    let mut config = Config {
        data_dir: dir.join("workspace"),
        config_path: dir.join("config.toml"),
        ..Default::default()
    };
    config.security.nevis.client_secret = Some(plaintext_secret.into());

    // Save (triggers encryption)
    config.save().await.unwrap();

    // Read raw TOML and verify plaintext secret is NOT present
    let raw_toml = tokio::fs::read_to_string(&config.config_path)
        .await
        .unwrap();
    assert!(
        !raw_toml.contains(plaintext_secret),
        "Saved TOML must not contain the plaintext client_secret"
    );

    // Parse stored TOML and verify the value is encrypted
    let stored: Config = toml::from_str(&raw_toml).unwrap();
    let stored_secret = stored.security.nevis.client_secret.as_ref().unwrap();
    assert!(
        crate::secrets::SecretStore::is_encrypted(stored_secret),
        "Stored client_secret must be marked as encrypted"
    );

    // Decrypt and verify it matches the original plaintext
    let store = crate::secrets::SecretStore::new(&dir, true);
    assert_eq!(store.decrypt(stored_secret).unwrap(), plaintext_secret);

    // Simulate a full load: deserialize then decrypt (mirrors load_or_init logic)
    let mut loaded: Config = toml::from_str(&raw_toml).unwrap();
    loaded.config_path = dir.join("config.toml");
    let load_store = crate::secrets::SecretStore::new(&dir, loaded.secrets.encrypt);
    loaded.decrypt_secrets(&load_store).unwrap();
    assert_eq!(
        loaded.security.nevis.client_secret.as_deref().unwrap(),
        plaintext_secret,
        "Loaded client_secret must match the original plaintext after decryption"
    );

    let _ = fs::remove_dir_all(&dir).await;
}

// ══════════════════════════════════════════════════════════
// Nevis config validation tests
// ══════════════════════════════════════════════════════════

#[test]
async fn nevis_config_validate_disabled_accepts_empty_fields() {
    let cfg = NevisConfig::default();
    assert!(!cfg.enabled);
    assert!(cfg.validate().is_ok());
}

#[test]
async fn nevis_config_validate_rejects_empty_instance_url() {
    let cfg = NevisConfig {
        enabled: true,
        instance_url: String::new(),
        client_id: "test-client".into(),
        ..NevisConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("instance_url"));
}

#[test]
async fn nevis_config_validate_rejects_empty_client_id() {
    let cfg = NevisConfig {
        enabled: true,
        instance_url: "https://nevis.example.com".into(),
        client_id: String::new(),
        ..NevisConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("client_id"));
}

#[test]
async fn nevis_config_validate_rejects_empty_realm() {
    let cfg = NevisConfig {
        enabled: true,
        instance_url: "https://nevis.example.com".into(),
        client_id: "test-client".into(),
        realm: String::new(),
        ..NevisConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("realm"));
}

#[test]
async fn nevis_config_validate_rejects_local_without_jwks() {
    let cfg = NevisConfig {
        enabled: true,
        instance_url: "https://nevis.example.com".into(),
        client_id: "test-client".into(),
        token_validation: "local".into(),
        jwks_url: None,
        ..NevisConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("jwks_url"));
}

#[test]
async fn nevis_config_validate_rejects_zero_session_timeout() {
    let cfg = NevisConfig {
        enabled: true,
        instance_url: "https://nevis.example.com".into(),
        client_id: "test-client".into(),
        token_validation: "remote".into(),
        session_timeout_secs: 0,
        ..NevisConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(err.contains("session_timeout_secs"));
}

#[test]
async fn nevis_config_validate_accepts_valid_enabled_config() {
    let cfg = NevisConfig {
        enabled: true,
        instance_url: "https://nevis.example.com".into(),
        realm: "master".into(),
        client_id: "test-client".into(),
        token_validation: "remote".into(),
        session_timeout_secs: 3600,
        ..NevisConfig::default()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
async fn nevis_config_validate_rejects_invalid_token_validation() {
    let cfg = NevisConfig {
        enabled: true,
        instance_url: "https://nevis.example.com".into(),
        realm: "master".into(),
        client_id: "test-client".into(),
        token_validation: "invalid_mode".into(),
        session_timeout_secs: 3600,
        ..NevisConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.contains("invalid value 'invalid_mode'"),
        "Expected invalid token_validation error, got: {err}"
    );
}

#[test]
async fn nevis_config_debug_redacts_client_secret() {
    let cfg = NevisConfig {
        client_secret: Some("super-secret".into()),
        ..NevisConfig::default()
    };
    let debug_output = format!("{:?}", cfg);
    assert!(
        !debug_output.contains("super-secret"),
        "Debug output must not contain the raw client_secret"
    );
    assert!(
        debug_output.contains("[REDACTED]"),
        "Debug output must show [REDACTED] for client_secret"
    );
}

#[test]
async fn telegram_config_ack_reactions_is_tri_state() {
    for (line, expected) in [
        ("ack_reactions = false", Some(false)),
        ("ack_reactions = true", Some(true)),
        ("", None),
    ] {
        let toml_str = format!("bot_token = \"123:ABC\"\nallowed_users = [\"alice\"]\n{line}\n");
        let cfg: TelegramConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(cfg.ack_reactions, expected, "{line:?}");
    }
}

// ── Bootstrap files ─────────────────────────────────────

#[tokio::test]
async fn ensure_bootstrap_files_creates_missing_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = tmp.path().join("workspace");
    let _: () = tokio::fs::create_dir_all(&ws).await.unwrap();

    ensure_bootstrap_files(&ws).await.unwrap();

    let soul: String = tokio::fs::read_to_string(ws.join("SOUL.md")).await.unwrap();
    let identity: String = tokio::fs::read_to_string(ws.join("IDENTITY.md"))
        .await
        .unwrap();
    assert!(soul.contains("SOUL.md"));
    assert!(identity.contains("IDENTITY.md"));
}

#[tokio::test]
async fn ensure_bootstrap_files_does_not_overwrite_existing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let ws = tmp.path().join("workspace");
    let _: () = tokio::fs::create_dir_all(&ws).await.unwrap();

    let custom = "# My custom SOUL";
    let _: () = tokio::fs::write(ws.join("SOUL.md"), custom).await.unwrap();

    ensure_bootstrap_files(&ws).await.unwrap();

    let soul: String = tokio::fs::read_to_string(ws.join("SOUL.md")).await.unwrap();
    assert_eq!(
        soul, custom,
        "ensure_bootstrap_files must not overwrite existing files"
    );

    // IDENTITY.md should still be created since it was missing
    let identity: String = tokio::fs::read_to_string(ws.join("IDENTITY.md"))
        .await
        .unwrap();
    assert!(identity.contains("IDENTITY.md"));
}

// ── PacingConfig serde defaults ─────────────────────────────

#[test]
async fn pacing_config_serde_defaults_match_manual_default() {
    // Deserialise an empty TOML table and verify the loop-detection
    // fields receive the same defaults as `PacingConfig::default()`.
    let from_toml: PacingConfig = toml::from_str("").unwrap();
    let manual = PacingConfig::default();

    assert_eq!(
        from_toml.loop_detection_enabled,
        manual.loop_detection_enabled
    );
    assert_eq!(
        from_toml.loop_detection_window_size,
        manual.loop_detection_window_size
    );
    assert_eq!(
        from_toml.loop_detection_max_repeats,
        manual.loop_detection_max_repeats
    );

    // Verify concrete values so a silent change to the defaults is caught.
    assert!(from_toml.loop_detection_enabled, "default should be true");
    assert_eq!(from_toml.loop_detection_window_size, 20);
    assert_eq!(from_toml.loop_detection_max_repeats, 3);
}

#[test]
async fn cost_enforcement_config_defaults() {
    let config = CostEnforcementConfig::default();
    assert_eq!(config.mode, "warn");
    assert_eq!(config.route_down_model, None);
    assert_eq!(config.reserve_percent, 10);
}

// ── Configurable macro tests ──

#[test]
async fn set_secret_updates_field() {
    let mut mx = TelegramConfig {
        bot_token: "old".into(),
        ..Default::default()
    };
    mx.set_secret("channels.telegram.bot_token", "new-token".into())
        .unwrap();
    assert_eq!(mx.bot_token, "new-token");
}

#[test]
async fn set_secret_unknown_name_fails() {
    let mut mx = TelegramConfig {
        bot_token: "tok".into(),
        ..Default::default()
    };
    assert!(
        mx.set_secret("channels.telegram.nonexistent", "val".into())
            .is_err()
    );
}

#[test]
async fn config_tree_traversal_discovers_nested_secrets() {
    let mut config = Config::default();
    // Set api_key on first model_provider entry (or create one)
    config
        .providers
        .models
        .ensure("anthropic", "default")
        .expect("anthropic typed slot")
        .api_key = Some("test-key".into());
    config.channels.telegram.insert(
        "default".to_string(),
        TelegramConfig {
            bot_token: "old".into(),
            ..Default::default()
        },
    );

    let fields = config.secret_fields();
    let names: Vec<&str> = fields.iter().map(|f| f.name).collect();
    assert!(names.contains(&"channels.telegram.bot_token"));

    assert!(
        names.contains(&"http_request.secrets"),
        "http_request.secrets must be classified as a secret map"
    );
}

#[test]
async fn config_set_secret_dispatches_to_child() {
    let mut config = Config::default();
    config.channels.telegram.insert(
        "default".to_string(),
        TelegramConfig {
            bot_token: "old".into(),
            ..Default::default()
        },
    );

    config
        .set_secret("channels.telegram.bot_token", "new".into())
        .unwrap();
    assert_eq!(
        config.channels.telegram.get("default").unwrap().bot_token,
        "new"
    );
}

#[test]
async fn config_set_secret_unknown_fails() {
    let mut config = Config::default();
    assert!(
        config
            .set_secret("nonexistent.field", "val".into())
            .is_err()
    );
}

#[test]
async fn config_set_http_request_secret_map_key_is_masked_and_encrypted() {
    let dir = TempDir::new().unwrap();
    let config_path = dir.path().join("config.toml");
    tokio::fs::write(&config_path, "schema_version = 1\n")
        .await
        .unwrap();
    let mut config = Config {
        config_path: config_path.clone(),
        data_dir: dir.path().join("workspace"),
        secrets: SecretsConfig { encrypt: true },
        ..Config::default()
    };
    let path = "http_request.secrets.api_token";

    assert!(
        Config::prop_is_secret(path),
        "dynamic http_request secret map entries must be classified as secret before the key exists"
    );
    config
        .set_prop_persistent(path, "Bearer from-config-set")
        .unwrap();

    assert_eq!(
        config
            .http_request
            .secrets
            .get("api_token")
            .map(String::as_str),
        Some("Bearer from-config-set")
    );
    assert_eq!(config.get_prop(path).unwrap(), "****");

    let field = config
        .prop_fields()
        .into_iter()
        .find(|field| field.name == path)
        .expect("dynamic secret map prop field");
    assert!(field.is_secret);
    assert_eq!(field.display_value, "****");
    assert_eq!(
        field.credential_class,
        Some(crate::config::CredentialSurfaceClass::EncryptedSecret)
    );

    config.save_dirty().await.unwrap();
    let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
    assert!(
        !contents.contains("Bearer from-config-set"),
        "auth secret must not be written in plaintext: {contents}"
    );

    let stored = crate::migration::migrate_to_current(&contents).unwrap();
    let encrypted = stored.http_request.secrets.get("api_token").unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(encrypted));
    let store = crate::secrets::SecretStore::new(dir.path(), true);
    assert_eq!(store.decrypt(encrypted).unwrap(), "Bearer from-config-set");
}

#[test]
async fn encrypt_decrypt_roundtrip_via_macro() {
    let dir = TempDir::new().unwrap();
    let store = crate::secrets::SecretStore::new(dir.path(), true);

    let mut mx = TelegramConfig {
        bot_token: "plaintext-token".into(),
        ..Default::default()
    };

    // Encrypt
    mx.encrypt_secrets(&store).unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(&mx.bot_token));
    assert_ne!(mx.bot_token, "plaintext-token");

    // Decrypt
    mx.decrypt_secrets(&store).unwrap();
    assert_eq!(mx.bot_token, "plaintext-token");
}

#[test]
async fn encrypt_skips_already_encrypted() {
    let dir = TempDir::new().unwrap();
    let store = crate::secrets::SecretStore::new(dir.path(), true);

    let mut mx = TelegramConfig {
        bot_token: "plaintext-token".into(),
        ..Default::default()
    };

    mx.encrypt_secrets(&store).unwrap();
    let first_encrypted = mx.bot_token.clone();

    // Encrypt again — should be idempotent
    mx.encrypt_secrets(&store).unwrap();
    assert_eq!(mx.bot_token, first_encrypted);
}

#[test]
async fn encrypt_no_op_on_disabled_store() {
    let dir = TempDir::new().unwrap();
    let store = crate::secrets::SecretStore::new(dir.path(), false);

    let mut mx = TelegramConfig {
        bot_token: "plaintext-token".into(),
        ..Default::default()
    };

    mx.encrypt_secrets(&store).unwrap();
    // With encryption disabled, value should stay plaintext
    assert_eq!(mx.bot_token, "plaintext-token");
}

// ── Property method tests ──

fn test_telegram_config() -> TelegramConfig {
    TelegramConfig {
        bot_token: "tok".into(),
        api_base_url: "https://m.org".into(),
        proxy_url: Some("@bot:m.org".into()),
        draft_update_interval_ms: 1500,
        ..Default::default()
    }
}

#[test]
async fn prop_fields_returns_typed_entries() {
    let mx = test_telegram_config();
    let fields = mx.prop_fields();
    let by_name: std::collections::HashMap<&str, &crate::traits::PropFieldInfo> =
        fields.iter().map(|f| (f.name.as_str(), f)).collect();

    // String field
    let homeserver = by_name["channels.telegram.api_base_url"];
    assert_eq!(homeserver.type_hint, "String");
    assert_eq!(homeserver.display_value, "https://m.org");

    // Option<String> — set
    let user_id = by_name["channels.telegram.proxy_url"];
    assert_eq!(user_id.type_hint, "Option<String>");
    assert_eq!(user_id.display_value, "@bot:m.org");

    // Option<String> — unset (proxy_url cleared on a clone)
    let mut cleared = test_telegram_config();
    cleared.proxy_url = None;
    let fields = cleared.prop_fields();
    let unset = fields
        .iter()
        .find(|f| f.name == "channels.telegram.proxy_url")
        .expect("proxy_url field present");
    assert_eq!(unset.display_value, "<unset>");

    // u64 field
    let interval = by_name["channels.telegram.draft_update_interval_ms"];
    assert_eq!(interval.type_hint, "u64");
    assert_eq!(interval.display_value, "1500");

    // Enum field
    let stream = by_name["channels.telegram.stream_mode"];
    assert!(stream.is_enum());
    assert!(stream.enum_variants.is_some());

    // Secret field — masked
    let token = by_name["channels.telegram.bot_token"];
    assert!(token.is_secret);
    assert_eq!(token.display_value, "****");

    // All fields have correct category
    for field in &fields {
        assert_eq!(field.category, "Channels");
    }
}

#[test]
async fn generated_config_fields_keep_operator_descriptions() {
    fn assert_description(fields: &[crate::traits::PropFieldInfo], suffix: &str, expected: &str) {
        let matches = fields
            .iter()
            .filter(|field| field.name.ends_with(suffix))
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one configurable field ending in `{suffix}`"
        );
        let field = matches[0];
        assert!(
            field
                .description
                .to_ascii_lowercase()
                .contains(&expected.to_ascii_lowercase()),
            "description for {} must retain `{expected}`: {}",
            field.name,
            field.description,
        );
    }

    let workspace = crate::multi_agent::AgentWorkspaceConfig::default().prop_fields();
    assert_description(&workspace, ".access", "cross-agent workspace allowlist");
    assert_description(
        &workspace,
        ".read_memory_from",
        "Cross-agent memory allowlist",
    );

    let thinking = crate::scattered_types::ThinkingConfig::default().prop_fields();
    assert_description(&thinking, ".native_thinking", "selected level has a budget");

    let compression = crate::scattered_types::ContextCompressionConfig::default().prop_fields();
    assert_description(&compression, ".summary_provider", "<type>.<alias>");
    assert_description(&compression, ".summary_model", "DEPRECATED bare model id");

    let email = crate::scattered_types::EmailConfig::default().prop_fields();
    assert_description(&email, ".observer_mode", "never modifies any IMAP flag");
}

#[test]
async fn agent_workspace_path_is_a_settable_property() {
    let mut workspace = crate::multi_agent::AgentWorkspaceConfig::default();

    let path = workspace
        .prop_fields()
        .into_iter()
        .find(|field| field.name == "agent_workspace.path")
        .expect("workspace path property");
    assert_eq!(path.kind, crate::config::PropKind::String);
    assert_eq!(path.display_value, crate::config::UNSET_DISPLAY);

    workspace
        .set_prop("agent_workspace.path", "/srv/zeroclaw/assistant")
        .unwrap();
    assert_eq!(
        workspace.path,
        Some(std::path::PathBuf::from("/srv/zeroclaw/assistant"))
    );
    assert_eq!(
        workspace.get_prop("agent_workspace.path").unwrap(),
        "/srv/zeroclaw/assistant"
    );

    workspace.set_prop("agent_workspace.path", "").unwrap();
    assert_eq!(workspace.path, None);
}

#[cfg(feature = "schema-export")]
#[test]
async fn generated_config_types_keep_schema_descriptions() {
    fn assert_schema_description<T: schemars::JsonSchema>(name: &str) {
        let schema =
            serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes to json");
        let description = schema
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{name} schema must have a top-level description"));
        assert!(
            !description.trim().is_empty(),
            "{name} schema description must not be empty",
        );
    }

    use crate::autonomy::{ApprovalRoute, AutonomyLevel};
    use crate::multi_agent::{
        AccessMode, AgentMemoryConfig, AgentWorkspaceConfig, MemoryBackendKind, OutputModality,
    };
    use crate::presets::{
        BuilderSubmission, ChannelQuickStart, ModelProviderChoice, SelectorChoice,
    };
    use crate::providers::{ModelProviders, Providers};
    use crate::scattered_types::ChannelPrecheckConfig;
    use crate::sections::{Section, SectionGroup};
    use crate::validation_warnings::ValidationWarning;

    assert_schema_description::<AutonomyLevel>("AutonomyLevel");
    assert_schema_description::<ApprovalRoute>("ApprovalRoute");
    assert_schema_description::<AccessMode>("AccessMode");
    assert_schema_description::<MemoryBackendKind>("MemoryBackendKind");
    assert_schema_description::<AgentWorkspaceConfig>("AgentWorkspaceConfig");
    assert_schema_description::<AgentMemoryConfig>("AgentMemoryConfig");
    assert_schema_description::<OutputModality>("OutputModality");
    assert_schema_description::<ModelProviderChoice>("ModelProviderChoice");
    assert_schema_description::<ChannelQuickStart>("ChannelQuickStart");
    assert_schema_description::<BuilderSubmission>("BuilderSubmission");
    assert_schema_description::<SelectorChoice<ModelProviderChoice>>("SelectorChoice");
    assert_schema_description::<ModelProviders>("ModelProviders");
    assert_schema_description::<Providers>("Providers");
    assert_schema_description::<ChannelPrecheckConfig>("ChannelPrecheckConfig");
    assert_schema_description::<SectionGroup>("SectionGroup");
    assert_schema_description::<Section>("Section");
    assert_schema_description::<ValidationWarning>("ValidationWarning");

    let map_key_schema = serde_json::to_value(schemars::schema_for!(crate::traits::MapKeySection))
        .expect("MapKeySection schema serializes to json");
    let natural_key = map_key_schema
        .pointer("/properties/natural_key/description")
        .and_then(serde_json::Value::as_str)
        .expect("MapKeySection.natural_key must have a schema description");
    assert!(natural_key.contains("natural key"));
}

#[test]
async fn get_prop_returns_values_by_path() {
    let mx = test_telegram_config();

    assert_eq!(
        mx.get_prop("channels.telegram.api_base_url").unwrap(),
        "https://m.org"
    );
    assert_eq!(
        mx.get_prop("channels.telegram.draft_update_interval_ms")
            .unwrap(),
        "1500"
    );
    assert_eq!(
        mx.get_prop("channels.telegram.proxy_url").unwrap(),
        "@bot:m.org"
    );
    assert_eq!(
        mx.get_prop("channels.telegram.mention_only").unwrap(),
        "false"
    );
    // Secrets return masked value
    assert_eq!(
        mx.get_prop("channels.telegram.bot_token").unwrap(),
        "**** (encrypted)"
    );
}

#[test]
async fn get_prop_unknown_path_fails() {
    let mx = test_telegram_config();
    assert!(mx.get_prop("channels.telegram.nonexistent").is_err());
}

#[test]
async fn set_prop_string() {
    let mut mx = test_telegram_config();
    mx.set_prop("channels.telegram.api_base_url", "https://new.org")
        .unwrap();
    assert_eq!(mx.api_base_url, "https://new.org");
}

#[test]
async fn set_prop_bool() {
    let mut mx = test_telegram_config();
    mx.set_prop("channels.telegram.interrupt_on_new_message", "true")
        .unwrap();
    assert!(mx.interrupt_on_new_message);
}

#[test]
async fn set_prop_bool_rejects_invalid() {
    let mut mx = test_telegram_config();
    let err = mx
        .set_prop("channels.telegram.interrupt_on_new_message", "yes")
        .unwrap_err();
    assert!(err.to_string().contains("bool"));
}

#[test]
async fn set_prop_u64() {
    let mut mx = test_telegram_config();
    mx.set_prop("channels.telegram.draft_update_interval_ms", "3000")
        .unwrap();
    assert_eq!(mx.draft_update_interval_ms, 3000);
}

#[test]
async fn set_prop_u64_rejects_invalid() {
    let mut mx = test_telegram_config();
    assert!(
        mx.set_prop("channels.telegram.draft_update_interval_ms", "abc")
            .is_err()
    );
}

#[test]
async fn set_prop_option_string_set_and_clear() {
    let mut mx = test_telegram_config();
    mx.set_prop("channels.telegram.proxy_url", "@new:m.org")
        .unwrap();
    assert_eq!(mx.proxy_url.as_deref(), Some("@new:m.org"));

    // Empty string clears Option
    mx.set_prop("channels.telegram.proxy_url", "").unwrap();
    assert!(mx.proxy_url.is_none());
}

#[test]
async fn set_prop_enum() {
    let mut mx = test_telegram_config();
    mx.set_prop("channels.telegram.stream_mode", "partial")
        .unwrap();
    assert_eq!(mx.stream_mode, StreamMode::Partial);

    mx.set_prop("channels.telegram.stream_mode", "multi_message")
        .unwrap();
    assert_eq!(mx.stream_mode, StreamMode::MultiMessage);
}

#[test]
async fn set_prop_enum_rejects_invalid() {
    let mut mx = test_telegram_config();
    let err = mx
        .set_prop("channels.telegram.stream_mode", "invalid")
        .unwrap_err();
    assert!(err.to_string().contains("expected one of"));
}

#[test]
async fn set_prop_unknown_path_fails() {
    let mut mx = test_telegram_config();
    assert!(mx.set_prop("channels.telegram.nonexistent", "val").is_err());
}

#[test]
async fn prop_is_secret_static_check() {
    assert!(TelegramConfig::prop_is_secret(
        "channels.telegram.bot_token"
    ));
    assert!(!TelegramConfig::prop_is_secret(
        "channels.telegram.api_base_url"
    ));
    assert!(!TelegramConfig::prop_is_secret(
        "channels.telegram.interrupt_on_new_message"
    ));
}
#[test]
async fn apply_env_overrides_rejects_schema_version() {
    let _env_guard = env_override_lock().await;
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::set_var("ZEROCLAW_schema_version", "99") };
    let mut config = Config::default();
    let result = crate::env_overrides::apply_env_overrides(&mut config);
    // SAFETY: test-only, single-threaded test runner.
    unsafe { std::env::remove_var("ZEROCLAW_schema_version") };

    let err = result.expect_err("schema_version override must be rejected");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("schema_version") && msg.contains("not overridable"),
        "error must name the path and the reason: {msg}",
    );
    // Untouched on rejection.
    assert_eq!(
        config.schema_version,
        crate::migration::CURRENT_SCHEMA_VERSION
    );
}

#[test]
async fn prop_is_env_overridden_reflects_env_overridden_paths() {
    // Empty by default — no env applied.
    let mut cfg = Config::default();
    assert!(!cfg.prop_is_env_overridden("channels.telegram.api_base_url"));
    assert!(!cfg.prop_is_env_overridden("gateway.request_timeout_secs"));

    // Populate the field directly (the same set that
    // `apply_env_overrides` returns from `load_or_init`).
    cfg.env_overridden_paths = EnvOverriddenPaths::from_set(std::collections::HashSet::from([
        "channels.telegram.api_base_url".to_string(),
        "gateway.request_timeout_secs".to_string(),
    ]));

    // True for paths in the list, false for anything else.
    assert!(cfg.prop_is_env_overridden("channels.telegram.api_base_url"));
    assert!(cfg.prop_is_env_overridden("gateway.request_timeout_secs"));
    assert!(!cfg.prop_is_env_overridden("channels.telegram.bot_token"));
    assert!(!cfg.prop_is_env_overridden("gateway.host"));
    // Empty path / non-schema path → false.
    assert!(!cfg.prop_is_env_overridden(""));
    assert!(!cfg.prop_is_env_overridden("does.not.exist"));
}

#[test]
async fn prop_is_secret_routes_through_hashmap_keyed_paths() {
    // Regression: the macro's HashMap<String, T> arm previously passed the
    // full materialised path (e.g. `model_providers.openrouter.api-key`)
    // straight to the inner type's `prop_is_secret`, which then matched on
    // its own configurable_prefix and returned false. Result: the CLI's
    // `config set --json` and the gateway's PropResponse both took the
    // non-secret branch and emitted `{value}` instead of `{populated}` for
    // any secret on a map-keyed nested type.
    assert!(Config::prop_is_secret(
        "providers.models.openrouter.default.api_key"
    ));
    assert!(Config::prop_is_secret(
        "providers.models.anthropic.default.api_key"
    ));
    assert!(!Config::prop_is_secret(
        "providers.models.openrouter.default.endpoint"
    ));
    assert!(!Config::prop_is_secret(
        "providers.models.openrouter.default.context-window"
    ));
}

#[test]
async fn file_transfer_header_maps_are_secret() {
    assert!(Config::prop_is_secret(
        "file_download.headers.Authorization"
    ));
    assert!(Config::prop_is_secret("file_upload.headers.Authorization"));
    assert!(Config::prop_is_secret(
        "file_upload_bundle.headers.Authorization"
    ));
    assert!(Config::prop_is_secret(
        "mcp.servers.acme.headers.Authorization"
    ));
    assert!(!Config::prop_is_secret("file_download.timeout_secs"));
    assert!(!Config::prop_is_secret("file_download.headers"));
}

#[test]
async fn typed_custom_slot_round_trips_uri_through_save_and_load() {
    // Legacy colon-URL keys (`custom:https://...`) are gone — `custom`
    // is a typed slot whose `uri` field carries the operator URL.
    // This pins: secret routing, save/encrypt, and round-trip reload
    // for the typed `custom` slot.
    let dir = TempDir::new().unwrap();
    let mut config = Config {
        config_path: dir.path().join("config.toml"),
        data_dir: dir.path().join("workspace"),
        ..Default::default()
    };
    let alias = "default";
    config
        .providers
        .models
        .ensure("custom", alias)
        .expect("custom typed slot");

    let prefix = format!("providers.models.custom.{alias}");
    let api_key_path = format!("{prefix}.api_key");
    let uri_path = format!("{prefix}.uri");
    let model_path = format!("{prefix}.model");
    let temperature_path = format!("{prefix}.temperature");

    assert!(
        Config::prop_is_secret(&api_key_path),
        "typed custom-slot api-key must route through the secret marker",
    );

    config.set_prop(&api_key_path, "sk-test-custom").unwrap();
    config
        .set_prop(&uri_path, "https://api.example.invalid/v1")
        .unwrap();
    config.set_prop(&model_path, "local-large").unwrap();
    config.set_prop(&temperature_path, "0.2").unwrap();

    let provider = config
        .providers
        .models
        .find("custom", alias)
        .expect("custom typed slot entry must be present");
    assert_eq!(provider.api_key.as_deref(), Some("sk-test-custom"));
    assert_eq!(
        provider.uri.as_deref(),
        Some("https://api.example.invalid/v1")
    );
    assert_eq!(provider.model.as_deref(), Some("local-large"));
    assert_eq!(provider.temperature, Some(0.2));

    assert_eq!(config.get_prop(&api_key_path).unwrap(), "**** (encrypted)");
    assert_eq!(
        config.get_prop(&uri_path).unwrap(),
        "https://api.example.invalid/v1"
    );

    config.save().await.unwrap();
    let raw_toml = tokio::fs::read_to_string(&config.config_path)
        .await
        .unwrap();
    assert!(
        raw_toml.contains("[providers.models.custom.default]"),
        "saved TOML should write under the typed custom slot",
    );
    assert!(
        !raw_toml.contains("sk-test-custom"),
        "saved TOML must not contain the plaintext custom provider API key",
    );

    let mut loaded: Config = crate::migration::migrate_to_current(&raw_toml).unwrap();
    loaded.config_path = config.config_path.clone();
    loaded.data_dir = config.data_dir.clone();
    let store = crate::secrets::SecretStore::new(dir.path(), loaded.secrets.encrypt);
    loaded.decrypt_secrets(&store).unwrap();
    let loaded_provider = loaded
        .providers
        .models
        .find("custom", alias)
        .expect("typed custom slot entry must round-trip through save/load");
    assert_eq!(loaded_provider.api_key.as_deref(), Some("sk-test-custom"));
    assert_eq!(
        loaded_provider.uri.as_deref(),
        Some("https://api.example.invalid/v1")
    );
    assert_eq!(loaded_provider.model.as_deref(), Some("local-large"));
    assert_eq!(loaded_provider.temperature, Some(0.2));
}

#[test]
async fn env_override_save_cycle_preserves_on_disk_secret() {
    // Regression bar for the data-loss bug identified in PR
    // review: an operator with a real on-disk credential who sets a
    // `ZEROCLAW_*` env override for the same path and triggers any
    // save (dashboard auto-save, CLI `config set` for an unrelated
    // field, Quickstart finalizer) must NOT corrupt the disk file.
    //
    // Pre-fix behavior: `mask_env_overrides_for_save` read disk via
    // `get_prop`, which returns `"**** (encrypted)"` for secret-typed
    // fields regardless of underlying state. That mask string then got
    // re-encrypted as plaintext and written to disk, destroying the
    // operator's real credential on the next reload.
    //
    // Post-fix: `apply_env_overrides` snapshots the post-decrypt
    // plaintext at apply time; `mask_env_overrides_for_save` restores
    // from that snapshot before `encrypt_secrets()` runs. The disk
    // secret survives the cycle.
    let dir = TempDir::new().unwrap();
    let mut config = Config {
        config_path: dir.path().join("config.toml"),
        data_dir: dir.path().join("workspace"),
        ..Default::default()
    };
    let original_secret = "sk-ant-real-on-disk-credential";
    let api_key_path = "providers.models.anthropic.default.api_key";
    config
        .providers
        .models
        .ensure("anthropic", "default")
        .expect("typed slot");
    config.set_prop(api_key_path, original_secret).unwrap();

    // First save: encrypts the original plaintext, writes to disk.
    config.save().await.unwrap();

    // Reload from disk to confirm the original landed correctly.
    let raw = tokio::fs::read_to_string(&config.config_path)
        .await
        .unwrap();
    let mut reloaded: Config = crate::migration::migrate_to_current(&raw).unwrap();
    reloaded.config_path = config.config_path.clone();
    reloaded.data_dir = config.data_dir.clone();
    // Parsed from the on-disk file, like load_or_init's existing-file
    // branch; keep full-save provenance for the save below.
    reloaded.loaded_from = Some(reloaded.config_path.clone());
    let store = crate::secrets::SecretStore::new(dir.path(), reloaded.secrets.encrypt);
    reloaded.decrypt_secrets(&store).unwrap();
    assert_eq!(
        reloaded
            .providers
            .models
            .anthropic
            .get("default")
            .and_then(|c| c.base.api_key.as_deref()),
        Some(original_secret),
        "baseline: original secret round-trips through one save/reload cycle",
    );

    // Simulate `apply_env_overrides` having injected a different value
    // for the same path — this is the state `Config::load_or_init`
    // leaves the in-memory config in when an operator boots with
    // `ZEROCLAW_providers__models__anthropic__default__api_key=...`
    // set in the environment.
    let env_value = "sk-ant-from-env-DIFFERENT";
    reloaded.env_overridden_paths =
        EnvOverriddenPaths::from_set(std::collections::HashSet::from([api_key_path.to_string()]));
    reloaded.pre_override_snapshots = PreOverrideSnapshots::from_map(
        std::collections::HashMap::from([(api_key_path.to_string(), original_secret.to_string())]),
    );
    reloaded.set_prop(api_key_path, env_value).unwrap();

    // Save again. With the pre-fix code path, this is the moment the
    // disk file got corrupted with the encrypted display mask.
    reloaded.save().await.unwrap();

    // Reload, decrypt, and confirm the original secret survived
    // (and the env value did NOT leak to disk, and the literal mask
    // string was NOT persisted).
    let raw_after = tokio::fs::read_to_string(&reloaded.config_path)
        .await
        .unwrap();
    assert!(
        !raw_after.contains(env_value),
        "env-injected value must never reach disk: {raw_after}",
    );
    assert!(
        !raw_after.contains("**** (encrypted)"),
        "display mask must never be persisted as a secret value: {raw_after}",
    );

    let mut after: Config = crate::migration::migrate_to_current(&raw_after).unwrap();
    after.config_path = reloaded.config_path.clone();
    after.data_dir = reloaded.data_dir.clone();
    let store2 = crate::secrets::SecretStore::new(dir.path(), after.secrets.encrypt);
    after.decrypt_secrets(&store2).unwrap();
    assert_eq!(
        after
            .providers
            .models
            .anthropic
            .get("default")
            .and_then(|c| c.base.api_key.as_deref()),
        Some(original_secret),
        "original on-disk secret must survive an env-override + save cycle",
    );
}

#[cfg(unix)]
fn tachi_vault_fixture(reference: &str, slow: bool) -> (TempDir, Vec<EnvValueGuard>) {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        r#"#!/bin/sh
if [ "$*" = 'vault status' ]; then
  printf '%s\n' '{{"initialized":true,"locked":true,"secure_store":{{"auto_unlock_available":true}}}}'
  exit 0
fi
if [ "$*" = 'vault get OPENAI_OFFICIAL_API_KEY --reveal --json --keychain' ]; then
  {}
  printf '%s\n' '{{"name":"OPENAI_OFFICIAL_API_KEY","revealed":true,"value":"synthetic-vault-secret"}}'
  exit 0
fi
exit 65
"#,
        if slow { "/bin/sleep 1" } else { ":" }
    );
    let fake = write_fake_op(&bin, &script);
    std::fs::rename(fake, bin.join("tachi")).unwrap();
    // A fake op uses the same synthetic plaintext for parity live-edit checks.
    write_fake_op(&bin, "#!/bin/sh\nprintf '%s\\n' 'synthetic-op-secret'\n");
    let guards = vec![
        EnvValueGuard::set("PATH", &bin),
        EnvValueGuard::set("HOME", dir.path()),
        EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", dir.path()),
        EnvValueGuard::remove("ZEROCLAW_WORKSPACE"),
        EnvValueGuard::set("ZEROCLAW_DATA_DIR", dir.path().join("workspace")),
    ];
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            r#"schema_version = 3
[providers.models.openai.default]
model = "gpt-5"
api_key = "{reference}"
"#
        ),
    )
    .unwrap();
    (dir, guards)
}

#[cfg(unix)]
#[test]
async fn config_load_resolves_tachi_vault_ref_and_save_preserves_reference() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let mut config = Config::load_or_init().await.unwrap();
    let path = "providers.models.openai.default.api_key";
    assert_eq!(
        crate::env_overrides::raw_value_for_path(&config, path).as_deref(),
        Some("synthetic-vault-secret")
    );
    config.save().await.unwrap();
    config
        .set_prop_persistent("providers.models.openai.default.model", "gpt-5-mini")
        .unwrap();
    config.save_dirty().await.unwrap();
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(raw.contains("vault:OPENAI_OFFICIAL_API_KEY"));
    assert!(!raw.contains("synthetic-vault-secret"));
    assert!(!raw.contains("enc2:"));
    assert!(!dir.path().join(".secret_key").exists());
}

#[cfg(unix)]
#[allow(
    clippy::disallowed_methods,
    reason = "test asserts Tokio worker responsiveness"
)]
#[test(flavor = "multi_thread", worker_threads = 1)]
async fn tachi_vault_reference_load_does_not_block_runtime_worker() {
    let _lock = env_override_lock().await;
    let (_dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", true);
    let started = std::time::Instant::now();
    let task = tokio::spawn(Config::load_or_init());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(started.elapsed() < std::time::Duration::from_millis(500));
    let config = task.await.unwrap().unwrap();
    assert_eq!(
        crate::env_overrides::raw_value_for_path(
            &config,
            "providers.models.openai.default.api_key"
        )
        .as_deref(),
        Some("synthetic-vault-secret")
    );
}

#[cfg(unix)]
#[test]
async fn dirty_tachi_vault_secret_edit_replaces_reference_and_subsequent_full_save_stays_replaced()
{
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let path = "providers.models.openai.default.api_key";
    for replacement in ["synthetic-direct-replacement", ""] {
        let mut config = Config::load_or_init().await.unwrap();
        config.set_prop_persistent(path, replacement).unwrap();
        config.save_dirty().await.unwrap();
        assert!(!config.onepassword_reference_snapshots.contains_key(path));
        config.save().await.unwrap();
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!raw.contains("vault:"));
        assert!(!raw.contains("synthetic-direct-replacement"));
        let mut stored: Config = toml::from_str(&raw).unwrap();
        stored
            .decrypt_secrets(&crate::secrets::SecretStore::new(dir.path(), true))
            .unwrap();
        assert_eq!(
            crate::env_overrides::raw_value_for_path(&stored, path).unwrap_or_default(),
            replacement
        );
        // Restore the reference to exercise clearing a loaded external field too.
        config
            .set_prop_persistent(path, "vault:OPENAI_OFFICIAL_API_KEY")
            .unwrap();
        config.save_dirty().await.unwrap();
    }
}

#[cfg(unix)]
#[test]
async fn dirty_edit_to_tachi_vault_reference_persists_ref_and_resolves_in_memory() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("synthetic-original", false);
    let path = "providers.models.openai.default.api_key";
    let mut config = Config::load_or_init().await.unwrap();
    for (reference, plaintext) in [
        ("vault:OPENAI_OFFICIAL_API_KEY", "synthetic-vault-secret"),
        ("op://vault/item/field", "synthetic-op-secret"),
    ] {
        config.set_prop_persistent(path, reference).unwrap();
        config.save_dirty().await.unwrap();
        assert_eq!(
            crate::env_overrides::raw_value_for_path(&config, path).as_deref(),
            Some(plaintext)
        );
        assert_eq!(
            config.onepassword_reference_snapshots.get(path).as_deref(),
            Some(reference)
        );
        assert!(config.dirty_paths.is_empty());
        config.save().await.unwrap();
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(raw.contains(reference));
        assert!(!raw.contains(plaintext));
        assert!(!raw.contains("enc2:"));
    }
    assert!(!dir.path().join(".secret_key").exists());
}

#[cfg(unix)]
#[test]
async fn dirty_tachi_vault_reference_failure_preserves_disk_and_pending_live_state() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let path = "providers.models.openai.default.api_key";
    let mut config = Config::load_or_init().await.unwrap();
    let original = std::fs::read(dir.path().join("config.toml")).unwrap();
    for reference in ["vault:-invalid", "vault:MISSING_KEY", "op://invalid"] {
        config.set_prop_persistent(path, reference).unwrap();
        let snapshots = config.onepassword_reference_snapshots.clone();
        assert!(config.save_dirty().await.is_err());
        assert!(config.save().await.is_err());
        assert_eq!(
            std::fs::read(dir.path().join("config.toml")).unwrap(),
            original
        );
        assert_eq!(config.onepassword_reference_snapshots, snapshots);
        assert_eq!(
            crate::env_overrides::raw_value_for_path(&config, path).as_deref(),
            Some(reference)
        );
        assert!(config.dirty_paths.contains(path));
    }
    assert!(!dir.path().join(".secret_key").exists());
}

#[cfg(unix)]
#[test]
async fn dirty_tachi_vault_reference_create_fallback_resolves_in_memory() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("synthetic-original", false);
    let path = "providers.models.openai.default.api_key";
    let mut config = Config::load_or_init().await.unwrap();
    std::fs::remove_file(dir.path().join("config.toml")).unwrap();
    config
        .set_prop_persistent(path, "vault:OPENAI_OFFICIAL_API_KEY")
        .unwrap();
    config.save_dirty().await.unwrap();
    assert_eq!(
        crate::env_overrides::raw_value_for_path(&config, path).as_deref(),
        Some("synthetic-vault-secret")
    );
    config.save().await.unwrap();
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(raw.contains("vault:OPENAI_OFFICIAL_API_KEY"));
    assert!(!raw.contains("synthetic-vault-secret"));
    assert!(!dir.path().join(".secret_key").exists());
}

#[cfg(unix)]
#[test]
async fn tachi_vault_env_override_plus_dirty_edit_never_leaks_vault_secret_to_disk() {
    let _lock = env_override_lock().await;
    let (dir, mut guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    std::fs::write(
        dir.path().join("config.toml"),
        r#"schema_version = 3

[secrets]
encrypt = false

[providers.models.openai.default]
model = "gpt-5"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
    )
    .unwrap();
    guards.push(EnvValueGuard::set(
        "ZEROCLAW_providers__models__openai__default__api_key",
        "synthetic-env-override",
    ));

    let path = "providers.models.openai.default.api_key";
    for replacement in ["synthetic-operator-replacement", ""] {
        std::fs::write(
            dir.path().join("config.toml"),
            r#"schema_version = 3

[secrets]
encrypt = false

[providers.models.openai.default]
model = "gpt-5"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
        )
        .unwrap();
        let mut config = Config::load_or_init().await.unwrap();
        assert_eq!(
            crate::env_overrides::raw_value_for_path(&config, path).as_deref(),
            Some("synthetic-env-override")
        );

        config.set_prop_persistent(path, replacement).unwrap();
        config.save_dirty().await.unwrap();
        let raw_dirty = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!raw_dirty.contains("synthetic-vault-secret"), "{raw_dirty}");
        assert!(!raw_dirty.contains("synthetic-env-override"), "{raw_dirty}");
        assert!(!raw_dirty.contains("vault:"), "{raw_dirty}");
        assert!(!config.env_overridden_paths.contains(path));
        assert!(!config.pre_override_snapshots.contains_key(path));

        // Follow-up full `save()` must also keep the replacement rather than
        // resurrecting the pre-override resolved Vault secret.
        config.save().await.unwrap();
        let raw_full = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!raw_full.contains("synthetic-vault-secret"), "{raw_full}");
        assert!(!raw_full.contains("synthetic-env-override"), "{raw_full}");
        let stored: Config = toml::from_str(&raw_full).unwrap();
        assert_eq!(
            crate::env_overrides::raw_value_for_path(&stored, path).unwrap_or_default(),
            replacement
        );
    }
}

#[cfg(unix)]
#[test]
async fn tachi_vault_collection_and_natural_key_secrets_roundtrip_and_resolve() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    std::fs::write(
        dir.path().join("config.toml"),
        r#"schema_version = 3

[gateway]
paired_tokens = ["vault:OPENAI_OFFICIAL_API_KEY", "plain-paired"]

[reliability]
api_keys = ["vault:OPENAI_OFFICIAL_API_KEY"]

[[mcp.servers]]
name = "primary"
command = "npx"

[mcp.servers.env]
API_TOKEN = "vault:OPENAI_OFFICIAL_API_KEY"
TOKEN = "plain-sibling"
"TOKEN[0]" = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
    )
    .unwrap();

    let mut config = Config::load_or_init().await.unwrap();
    assert_eq!(
        config.gateway.paired_tokens,
        vec!["synthetic-vault-secret", "plain-paired"]
    );
    assert_eq!(config.reliability.api_keys, vec!["synthetic-vault-secret"]);
    assert_eq!(
        config.mcp.servers[0]
            .env
            .get("API_TOKEN")
            .map(String::as_str),
        Some("synthetic-vault-secret")
    );
    assert_eq!(
        config.mcp.servers[0]
            .env
            .get("TOKEN[0]")
            .map(String::as_str),
        Some("synthetic-vault-secret")
    );

    // Full save preserves element-level `vault:` references and bracketed map keys (`TOKEN[0]`),
    // while encrypting plain siblings.
    config.save().await.unwrap();
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!raw.contains("synthetic-vault-secret"), "{raw}");
    assert!(!raw.contains("plain-paired"), "{raw}");
    assert!(!raw.contains("plain-sibling"), "{raw}");
    let saved_raw: Config = toml::from_str(&raw).unwrap();
    assert_eq!(
        saved_raw.gateway.paired_tokens[0],
        "vault:OPENAI_OFFICIAL_API_KEY"
    );
    assert!(saved_raw.gateway.paired_tokens[1].starts_with("enc2:"));
    assert_eq!(
        saved_raw.reliability.api_keys,
        vec!["vault:OPENAI_OFFICIAL_API_KEY"]
    );
    assert_eq!(
        saved_raw.mcp.servers[0]
            .env
            .get("API_TOKEN")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    assert_eq!(
        saved_raw.mcp.servers[0]
            .env
            .get("TOKEN[0]")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    assert!(
        saved_raw.mcp.servers[0]
            .env
            .get("TOKEN")
            .is_some_and(|v| v.starts_with("enc2:"))
    );

    // Editing scalar sibling `TOKEN` must never evict bracketed `TOKEN[0]`'s `vault:` snapshot.
    config
        .set_prop_persistent("mcp.servers.primary.env.TOKEN", "updated-sibling")
        .unwrap();
    config.save_dirty().await.unwrap();
    config.save().await.unwrap();
    let raw_tok = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!raw_tok.contains("synthetic-vault-secret"), "{raw_tok}");
    assert!(!raw_tok.contains("updated-sibling"), "{raw_tok}");
    let saved_tok: Config = toml::from_str(&raw_tok).unwrap();
    assert_eq!(
        saved_tok.mcp.servers[0]
            .env
            .get("TOKEN[0]")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );

    // Live edit on a StringArray secret resolves elements in memory and preserves refs on disk.
    config
        .set_prop_persistent(
            "reliability.api_keys",
            r#"["op://vault/item/field", "plain-second"]"#,
        )
        .unwrap();
    config.save_dirty().await.unwrap();
    assert_eq!(
        config.reliability.api_keys,
        vec!["synthetic-op-secret", "plain-second"]
    );
    config.save().await.unwrap();
    let raw_after = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!raw_after.contains("synthetic-op-secret"), "{raw_after}");
    assert!(!raw_after.contains("plain-second"), "{raw_after}");
    let saved_after: Config = toml::from_str(&raw_after).unwrap();
    assert_eq!(saved_after.reliability.api_keys[0], "op://vault/item/field");
    assert!(saved_after.reliability.api_keys[1].starts_with("enc2:"));

    // Non-aliased natural-key rename (`mcp.servers`) rewrites external secret snapshots too.
    assert!(
        config
            .rename_map_key("mcp.servers", "primary", "secondary")
            .unwrap()
    );
    config.mark_dirty("mcp.servers.primary");
    config.mark_dirty("mcp.servers.secondary");
    config.save_dirty().await.unwrap();
    let raw_renamed = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        !raw_renamed.contains("synthetic-vault-secret"),
        "{raw_renamed}"
    );
    let saved_renamed: Config = toml::from_str(&raw_renamed).unwrap();
    assert_eq!(saved_renamed.mcp.servers[0].name, "secondary");
    assert_eq!(
        saved_renamed.mcp.servers[0]
            .env
            .get("API_TOKEN")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );

    // Invalid element ref in a StringArray fails closed without mutating disk or live state.
    let disk_before_fail = std::fs::read(dir.path().join("config.toml")).unwrap();
    config
        .set_prop_persistent("reliability.api_keys", r#"["vault:-invalid"]"#)
        .unwrap();
    assert!(config.save_dirty().await.is_err());
    assert_eq!(
        std::fs::read(dir.path().join("config.toml")).unwrap(),
        disk_before_fail
    );
}

#[cfg(unix)]
#[test]
async fn tachi_vault_provider_alias_rename_preserves_external_reference() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let mut config = Config::load_or_init().await.unwrap();

    let kind = crate::alias_refs::AliasKind::Provider {
        category: crate::alias_refs::ProviderCategory::Models,
        family: "openai".to_string(),
    };
    let report =
        crate::alias_refs::rename_with_cascade(&mut config, &kind, "default", "primary").unwrap();
    for dirty in &report.dirty_paths {
        config.mark_dirty(dirty);
    }
    config.save_dirty().await.unwrap();
    assert_eq!(
        crate::env_overrides::raw_value_for_path(
            &config,
            "providers.models.openai.primary.api_key"
        )
        .as_deref(),
        Some("synthetic-vault-secret")
    );

    config.save().await.unwrap();
    let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(raw.contains("[providers.models.openai.primary]"), "{raw}");
    assert!(!raw.contains("[providers.models.openai.default]"), "{raw}");
    assert!(raw.contains("vault:OPENAI_OFFICIAL_API_KEY"), "{raw}");
    assert!(!raw.contains("synthetic-vault-secret"), "{raw}");
    assert!(!raw.contains("enc2:"), "{raw}");
}

#[cfg(unix)]
#[test]
async fn tachi_vault_overlapping_keys_route_cascade_and_route_env_override() {
    let _lock = env_override_lock().await;
    let (dir, mut guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    std::fs::write(
        dir.path().join("config.toml"),
        r#"schema_version = 3

[providers.models.openai.keep]
model = "gpt-5"

[providers.models.openai.dropme]
model = "gpt-5"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"

[[model_routes]]
hint = "fast"
model_provider = "openai.dropme"
model = "gpt-5-mini"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"

[[model_routes]]
hint = "backup"
model_provider = "openai.keep"
model = "gpt-5-nano"
api_key = "plain-route-secret"

[[mcp.servers]]
name = "fs"
command = "npx"

[mcp.servers.env]
API_TOKEN = "vault:OPENAI_OFFICIAL_API_KEY"

[[mcp.servers]]
name = "fs.extra"
command = "npx"

[mcp.servers.env]
API_TOKEN = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
    )
    .unwrap();
    guards.push(EnvValueGuard::set(
        "ZEROCLAW_model_routes__fast__api_key",
        "synthetic-route-env",
    ));

    let mut config = Config::load_or_init().await.unwrap();
    assert_eq!(
        config.model_routes[0].api_key.as_deref(),
        Some("synthetic-route-env")
    );

    // 1. Natural-key route env override preserves `vault:` on disk across full save.
    config.save().await.unwrap();
    let raw_initial = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        !raw_initial.contains("synthetic-route-env"),
        "{raw_initial}"
    );
    assert!(
        !raw_initial.contains("synthetic-vault-secret"),
        "{raw_initial}"
    );
    let parsed_initial: Config = toml::from_str(&raw_initial).unwrap();
    assert_eq!(
        parsed_initial.model_routes[0].api_key.as_deref(),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );

    // 2. Overlapping natural keys (`fs` vs `fs.extra`): renaming and deleting `fs`
    // must never strip or migrate `fs.extra`'s `vault:` snapshot.
    assert!(
        config
            .rename_map_key("mcp.servers", "fs", "fs_main")
            .unwrap()
    );
    assert!(config.delete_map_key("mcp.servers", "fs_main").unwrap());
    config.mark_dirty("mcp.servers.fs");
    config.mark_dirty("mcp.servers.fs_main");
    config.save_dirty().await.unwrap();
    config.save().await.unwrap();
    let raw_mcp = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!raw_mcp.contains("synthetic-vault-secret"), "{raw_mcp}");
    let parsed_mcp: Config = toml::from_str(&raw_mcp).unwrap();
    assert_eq!(parsed_mcp.mcp.servers.len(), 1);
    assert_eq!(parsed_mcp.mcp.servers[0].name, "fs.extra");
    assert_eq!(
        parsed_mcp.mcp.servers[0]
            .env
            .get("API_TOKEN")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );

    // 3. Provider cascade delete drops scrubbed `model_routes.fast` snapshots so
    // renaming `backup` -> `fast` keeps its encrypted direct key instead of stale `vault:`.
    let kind = crate::alias_refs::AliasKind::Provider {
        category: crate::alias_refs::ProviderCategory::Models,
        family: "openai".to_string(),
    };
    let report = crate::alias_refs::delete_with_cascade(
        &mut config,
        &kind,
        "dropme",
        crate::alias_refs::CascadePolicy::RefuseOnHard,
    )
    .unwrap();
    for dirty in report.dirty_paths() {
        config.mark_dirty(&dirty);
    }
    config.save_dirty().await.unwrap();

    assert!(
        config
            .rename_map_key("model_routes", "backup", "fast")
            .unwrap()
    );
    config.mark_dirty("model_routes.backup");
    config.mark_dirty("model_routes.fast");
    config.save_dirty().await.unwrap();
    let raw_final = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    let parsed_final: Config = toml::from_str(&raw_final).unwrap();
    assert_eq!(parsed_final.model_routes.len(), 1);
    assert_eq!(parsed_final.model_routes[0].hint, "fast");
    assert!(
        parsed_final.model_routes[0]
            .api_key
            .as_deref()
            .is_some_and(|v| v.starts_with("enc2:")),
        "{raw_final}"
    );
}

#[cfg(unix)]
#[test]
async fn tachi_vault_dotted_natural_key_field_overlap_and_ambiguous_rejection() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
schema_version = 3

[secrets]
encrypt = false

[providers.models.openai.default]
model = "gpt-5"

[[model_routes]]
hint = "fast"
provider = "openai.default"
model = "gpt-5-mini"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"

[[model_routes]]
hint = "fast.api_key"
provider = "openai.default"
model = "gpt-5-nano"
api_key = "op://vault/item/field"

[[mcp.servers]]
name = "fs"
command = "npx"

[mcp.servers.env]
TOKEN = "vault:OPENAI_OFFICIAL_API_KEY"

[[mcp.servers]]
name = "fs.env"
command = "npx"

[[mcp.servers]]
name = "a"
command = "npx"

[mcp.servers.env]
"env.TOKEN" = "vault:OPENAI_OFFICIAL_API_KEY"

[[mcp.servers]]
name = "a.env"
command = "npx"

[mcp.servers.env]
TOKEN = "plain-other"
"#,
    )
    .unwrap();

    let mut config = Config::load_or_init().await.unwrap();
    assert_eq!(
        config.model_routes[0].api_key.as_deref(),
        Some("synthetic-vault-secret")
    );
    assert_eq!(
        config.model_routes[1].api_key.as_deref(),
        Some("synthetic-op-secret")
    );
    assert_eq!(
        config.mcp.servers[0].env.get("TOKEN").map(String::as_str),
        Some("synthetic-vault-secret")
    );
    assert_eq!(
        config.mcp.servers[2]
            .env
            .get("env.TOKEN")
            .map(String::as_str),
        Some("synthetic-vault-secret")
    );

    // 1. Editing `mcp.servers.a.env.env.TOKEN` routes to `a.env`'s `TOKEN` and must
    // persist directly via incremental `save_dirty()` (before any full `save()`) without
    // evicting `a`'s `"env.TOKEN"` `vault:` snapshot, even with `secrets.encrypt = false`.
    config
        .set_prop_persistent("mcp.servers.a.env.env.TOKEN", "updated-plain")
        .unwrap();
    config.save_dirty().await.unwrap();

    for step in ["after_save_dirty", "after_full_save"] {
        if step == "after_full_save" {
            config.save().await.unwrap();
        }
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!raw.contains("synthetic-vault-secret"), "{step}: {raw}");
        assert!(!raw.contains("synthetic-op-secret"), "{step}: {raw}");
        let parsed: Config = toml::from_str(&raw).unwrap();
        assert_eq!(
            parsed.model_routes[0].api_key.as_deref(),
            Some("vault:OPENAI_OFFICIAL_API_KEY"),
            "{step}"
        );
        assert_eq!(
            parsed.model_routes[1].api_key.as_deref(),
            Some("op://vault/item/field"),
            "{step}"
        );
        assert_eq!(
            parsed.mcp.servers[0].env.get("TOKEN").map(String::as_str),
            Some("vault:OPENAI_OFFICIAL_API_KEY"),
            "{step}"
        );
        assert_eq!(
            parsed.mcp.servers[2]
                .env
                .get("env.TOKEN")
                .map(String::as_str),
            Some("vault:OPENAI_OFFICIAL_API_KEY"),
            "{step}"
        );
        assert_eq!(
            parsed.mcp.servers[3].env.get("TOKEN").map(String::as_str),
            Some("updated-plain"),
            "{step}"
        );
    }

    // 2. Whole-entry delete of `hint = "fast.api_key"` marks `"model_routes.fast.api_key"`
    // dirty (`api_config.rs:handle_delete_map_key`). Neither `save_dirty()` nor `save()`
    // may misread that whole-entry marker as a secret edit on `hint = "fast"`.
    let pre_delete_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        config
            .delete_map_key("model_routes", "fast.api_key")
            .unwrap()
    );
    config.mark_dirty("model_routes.fast.api_key");

    let mut dirty_writer_cfg = config.clone();
    dirty_writer_cfg.save_dirty().await.unwrap();
    let raw_dirty_del = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        !raw_dirty_del.contains("synthetic-vault-secret"),
        "{raw_dirty_del}"
    );
    assert!(
        !raw_dirty_del.contains("synthetic-op-secret"),
        "{raw_dirty_del}"
    );
    let parsed_dirty_del: Config = toml::from_str(&raw_dirty_del).unwrap();
    assert_eq!(parsed_dirty_del.model_routes.len(), 1);
    assert_eq!(parsed_dirty_del.model_routes[0].hint, "fast");
    assert_eq!(
        parsed_dirty_del.model_routes[0].api_key.as_deref(),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );

    // Restore pre-delete file and verify full `save()` with the same pending whole-entry
    // dirty marker also preserves `fast`'s `vault:` reference without leaking plaintext.
    std::fs::write(dir.path().join("config.toml"), &pre_delete_disk).unwrap();
    config.save().await.unwrap();
    let raw_full_del = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        !raw_full_del.contains("synthetic-vault-secret"),
        "{raw_full_del}"
    );
    let parsed_full_del: Config = toml::from_str(&raw_full_del).unwrap();
    assert_eq!(parsed_full_del.model_routes.len(), 1);
    assert_eq!(parsed_full_del.model_routes[0].hint, "fast");
    assert_eq!(
        parsed_full_del.model_routes[0].api_key.as_deref(),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    config.clear_dirty();

    // 3. Empty/whitespace natural keys are rejected on create/rename, and direct struct
    // mutation to an empty natural key fails closed on both `save()` and `save_dirty()`.
    assert!(config.create_map_key("mcp.servers", "").is_err());
    assert!(config.create_map_key("mcp.servers", "   ").is_err());
    assert!(config.rename_map_key("mcp.servers", "fs", "").is_err());
    assert!(config.rename_map_key("mcp.servers", "fs", "   ").is_err());

    for bad_direct_name in ["", "fs.env", "unregistered_direct"] {
        let mut tampered = config.clone();
        tampered.mcp.servers[0].name = bad_direct_name.to_string();
        tampered.mark_dirty("mcp.servers");
        assert!(
            tampered.save().await.is_err(),
            "direct name={bad_direct_name:?}"
        );
        assert!(
            tampered.save_dirty().await.is_err(),
            "direct name={bad_direct_name:?}"
        );
        let raw_after_tamper = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(
            !raw_after_tamper.contains("synthetic-vault-secret"),
            "{raw_after_tamper}"
        );
    }

    // 4. Same-batch secret edit followed by `rename_map_key` must migrate dirty prop
    // tracking so invalid references still fail closed and valid edits are not clobbered
    // by the old reference snapshot.
    let mut bad_batch = config.clone();
    bad_batch
        .set_prop_persistent("model_routes.fast.api_key", "vault:-invalid")
        .unwrap();
    assert!(
        bad_batch
            .rename_map_key("model_routes", "fast", "fast.v2")
            .unwrap()
    );
    bad_batch.mark_dirty("model_routes.fast");
    bad_batch.mark_dirty("model_routes.fast.v2");
    assert!(bad_batch.save().await.is_err());
    assert!(bad_batch.save_dirty().await.is_err());

    let mut good_batch = config.clone();
    good_batch
        .set_prop_persistent("model_routes.fast.api_key", "op://vault/item/field")
        .unwrap();
    assert!(
        good_batch
            .rename_map_key("model_routes", "fast", "fast.v2")
            .unwrap()
    );
    good_batch.mark_dirty("model_routes.fast");
    good_batch.mark_dirty("model_routes.fast.v2");
    good_batch.save_dirty().await.unwrap();
    assert_eq!(good_batch.model_routes[0].hint, "fast.v2");
    assert_eq!(
        good_batch.model_routes[0].api_key.as_deref(),
        Some("synthetic-op-secret")
    );
    let raw_good_batch = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(!raw_good_batch.contains("synthetic-op-secret"));
    assert!(!raw_good_batch.contains("synthetic-vault-secret"));
    let parsed_good_batch: Config = toml::from_str(&raw_good_batch).unwrap();
    assert_eq!(parsed_good_batch.model_routes[0].hint, "fast.v2");
    assert_eq!(
        parsed_good_batch.model_routes[0].api_key.as_deref(),
        Some("op://vault/item/field")
    );

    // 5. Deleting or renaming a dotted sibling (`hint = "fast.api_key"`) must never drop
    // or migrate the shorter sibling's (`hint = "fast"`) `ZEROCLAW_*` env-override snapshot.
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
schema_version = 3

[secrets]
encrypt = false

[providers.models.openai.default]
model = "gpt-5"

[[model_routes]]
hint = "fast"
provider = "openai.default"
model = "gpt-5-mini"
api_key = "disk-inline-key"

[[model_routes]]
hint = "fast.api_key"
provider = "openai.default"
model = "gpt-5-nano"
"#,
    )
    .unwrap();
    {
        let _route_env = EnvValueGuard::set(
            "ZEROCLAW_model_routes__fast__api_key",
            "env-secret-override",
        );
        let mut env_cfg = Config::load_or_init().await.unwrap();
        assert_eq!(
            env_cfg.model_routes[0].api_key.as_deref(),
            Some("env-secret-override")
        );

        let mut env_rename_cfg = env_cfg.clone();
        assert!(
            env_rename_cfg
                .rename_map_key("model_routes", "fast.api_key", "slow")
                .unwrap()
        );
        env_rename_cfg.mark_dirty("model_routes.fast.api_key");
        env_rename_cfg.mark_dirty("model_routes.slow");
        env_rename_cfg.save_dirty().await.unwrap();
        env_rename_cfg.save().await.unwrap();
        let raw_env_rename = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(
            !raw_env_rename.contains("env-secret-override"),
            "{raw_env_rename}"
        );

        std::fs::write(
            dir.path().join("config.toml"),
            r#"
schema_version = 3

[secrets]
encrypt = false

[providers.models.openai.default]
model = "gpt-5"

[[model_routes]]
hint = "fast"
provider = "openai.default"
model = "gpt-5-mini"
api_key = "disk-inline-key"

[[model_routes]]
hint = "fast.api_key"
provider = "openai.default"
model = "gpt-5-nano"
"#,
        )
        .unwrap();
        assert!(
            env_cfg
                .delete_map_key("model_routes", "fast.api_key")
                .unwrap()
        );
        env_cfg.mark_dirty("model_routes.fast.api_key");
        env_cfg.save_dirty().await.unwrap();
        env_cfg.save().await.unwrap();
        let raw_env_del = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(
            !raw_env_del.contains("env-secret-override"),
            "{raw_env_del}"
        );
        let parsed_env_del: Config = toml::from_str(&raw_env_del).unwrap();
        assert_eq!(
            parsed_env_del.model_routes[0].api_key.as_deref(),
            Some("disk-inline-key")
        );
    }

    // 6. Env-only external reference (`ZEROCLAW_* = "vault:..."`) must never be persisted
    // on `save()` over the existing on-disk credential.
    {
        let _route_env = EnvValueGuard::set(
            "ZEROCLAW_model_routes__fast__api_key",
            "vault:OPENAI_OFFICIAL_API_KEY",
        );
        let env_ref_cfg = Config::load_or_init().await.unwrap();
        env_ref_cfg.save().await.unwrap();
        let raw_env_ref = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(
            !raw_env_ref.contains("vault:OPENAI_OFFICIAL_API_KEY"),
            "{raw_env_ref}"
        );
        let parsed_env_ref: Config = toml::from_str(&raw_env_ref).unwrap();
        assert_eq!(
            parsed_env_ref.model_routes[0].api_key.as_deref(),
            Some("disk-inline-key")
        );
    }

    // 7. Overlapping chained paths (`token` and `"env.token"` on MCP server `a`) renamed
    // `a -> a.env` must migrate two-phase without HashMap/HashSet iteration-order clobber.
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
schema_version = 3

[secrets]
encrypt = false

[[mcp.servers]]
name = "a"
command = "npx"

[mcp.servers.env]
token = "vault:OPENAI_OFFICIAL_API_KEY"
"env.token" = "op://vault/item/field"
"#,
    )
    .unwrap();
    let mut chained_cfg = Config::load_or_init().await.unwrap();
    chained_cfg.pre_override_snapshots.insert(
        "mcp.servers.a.env.token".to_string(),
        "disk-a-1".to_string(),
    );
    chained_cfg.pre_override_snapshots.insert(
        "mcp.servers.a.env.env.token".to_string(),
        "disk-a-2".to_string(),
    );
    chained_cfg
        .env_overridden_paths
        .insert("mcp.servers.a.env.token".to_string());
    chained_cfg
        .env_overridden_paths
        .insert("mcp.servers.a.env.env.token".to_string());
    chained_cfg
        .set_prop_persistent("mcp.servers.a.env.token", "edited-1")
        .unwrap();
    chained_cfg
        .set_prop_persistent("mcp.servers.a.env.env.token", "edited-2")
        .unwrap();
    assert!(
        chained_cfg
            .rename_map_key("mcp.servers", "a", "a.env")
            .unwrap()
    );
    assert_eq!(
        chained_cfg
            .pre_override_snapshots
            .get("mcp.servers.a.env.env.token")
            .as_deref(),
        Some("disk-a-1")
    );
    assert_eq!(
        chained_cfg
            .pre_override_snapshots
            .get("mcp.servers.a.env.env.env.token")
            .as_deref(),
        Some("disk-a-2")
    );
    assert!(
        chained_cfg
            .explicit_prop_dirty_paths
            .contains("mcp.servers.a.env.env.token")
    );
    assert!(
        chained_cfg
            .explicit_prop_dirty_paths
            .contains("mcp.servers.a.env.env.env.token")
    );
    chained_cfg.mark_dirty("mcp.servers.a");
    chained_cfg.mark_dirty("mcp.servers.a.env");
    chained_cfg.save_dirty().await.unwrap();
    let raw_chained = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    let parsed_chained: Config = toml::from_str(&raw_chained).unwrap();
    assert_eq!(
        parsed_chained.mcp.servers[0]
            .env
            .get("token")
            .map(String::as_str),
        Some("edited-1")
    );
    assert_eq!(
        parsed_chained.mcp.servers[0]
            .env
            .get("env.token")
            .map(String::as_str),
        Some("edited-2")
    );

    // 8. Renaming `b -> a.env` or creating `a.env` after editing `a.env["env.token"]`
    // (`mcp.servers.a.env.env.token`) must fail closed without rebinding the tracked
    // dirty path onto `a.env`'s `env["token"]` or leaking `b`'s resolved secret.
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
schema_version = 3

[secrets]
encrypt = false

[[mcp.servers]]
name = "a"
command = "npx"

[mcp.servers.env]
"env.token" = "vault:OPENAI_OFFICIAL_API_KEY"

[[mcp.servers]]
name = "b"
command = "uvx"

[mcp.servers.env]
token = "op://vault/item/field"
"#,
    )
    .unwrap();
    let mut rebind_cfg = Config::load_or_init().await.unwrap();
    rebind_cfg
        .set_prop_persistent("mcp.servers.a.env.env.token", "vault:-invalid")
        .unwrap();
    assert!(
        rebind_cfg
            .rename_map_key("mcp.servers", "b", "a.env")
            .is_err()
    );
    assert_eq!(rebind_cfg.mcp.servers[1].name, "b");
    assert!(rebind_cfg.create_map_key("mcp.servers", "a.env").is_err());
    assert_eq!(rebind_cfg.mcp.servers.len(), 2);
    assert!(rebind_cfg.save_dirty().await.is_err());
    assert!(rebind_cfg.save().await.is_err());

    let mut direct_rebind = rebind_cfg.clone();
    direct_rebind.mcp.servers[1].name = "a.env".to_string();
    direct_rebind.mark_dirty("mcp.servers.b");
    direct_rebind.mark_dirty("mcp.servers.a.env");
    assert!(direct_rebind.save_dirty().await.is_err());
    assert!(direct_rebind.save().await.is_err());
    let raw_rebind = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        !raw_rebind.contains("synthetic-vault-secret")
            && !raw_rebind.contains("synthetic-op-secret")
            && !raw_rebind.contains("vault:-invalid"),
        "{raw_rebind}"
    );

    // 9. Direct Rust mutation + `mark_dirty("mcp.servers.a.env.env.token")` followed by
    // introducing an exact-name whole entry `"a.env.env.token"` (via `create_map_key`,
    // `rename_map_key`, or direct struct push) must fail closed without dropping the leaf
    // dirty path or writing an unvalidated reference.
    let mut mark_dirty_shadow = Config::load_or_init().await.unwrap();
    mark_dirty_shadow.mcp.servers[0]
        .env
        .insert("env.token".to_string(), "vault:-invalid".to_string());
    mark_dirty_shadow.mark_dirty("mcp.servers.a.env.env.token");
    assert!(
        mark_dirty_shadow
            .create_map_key("mcp.servers", "a.env.env.token")
            .is_err()
    );
    assert_eq!(mark_dirty_shadow.mcp.servers.len(), 2);
    assert!(
        mark_dirty_shadow
            .rename_map_key("mcp.servers", "b", "a.env.env.token")
            .is_err()
    );
    assert_eq!(mark_dirty_shadow.mcp.servers[1].name, "b");
    assert!(mark_dirty_shadow.save_dirty().await.is_err());
    assert!(mark_dirty_shadow.save().await.is_err());

    let mut direct_push_shadow = mark_dirty_shadow.clone();
    direct_push_shadow.mcp.servers.push(McpServerConfig {
        name: "a.env.env.token".to_string(),
        command: "node".to_string(),
        ..Default::default()
    });
    direct_push_shadow.mark_dirty("mcp.servers.a.env.env.token");
    assert!(direct_push_shadow.save_dirty().await.is_err());
    assert!(direct_push_shadow.save().await.is_err());
    let raw_shadow = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        !raw_shadow.contains("synthetic-vault-secret")
            && !raw_shadow.contains("synthetic-op-secret")
            && !raw_shadow.contains("vault:-invalid"),
        "{raw_shadow}"
    );

    // 10. Section-level `mark_dirty("model_routes")` (e.g. `alias_refs` updating `provider`)
    // preserves `vault:` snapshots on disk and resolved secrets in memory without leaking plaintext.
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
schema_version = 3

[secrets]
encrypt = false

[[model_routes]]
hint = "fast"
provider = "openai.default"
model = "gpt-5"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
    )
    .unwrap();
    let mut section_dirty_cfg = Config::load_or_init().await.unwrap();
    section_dirty_cfg.model_routes[0].model_provider = "openai.renamed".to_string();
    section_dirty_cfg.mark_dirty("model_routes");
    section_dirty_cfg.save_dirty().await.unwrap();
    assert_eq!(
        section_dirty_cfg.model_routes[0].api_key.as_deref(),
        Some("synthetic-vault-secret")
    );
    let raw_sec_dirty = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(
        raw_sec_dirty.contains("openai.renamed")
            && raw_sec_dirty.contains("vault:OPENAI_OFFICIAL_API_KEY")
            && !raw_sec_dirty.contains("synthetic-vault-secret"),
        "{raw_sec_dirty}"
    );

    // 11. Duplicate or empty natural/map keys anywhere in a section carrying an external
    // secret reference fail closed on load, even when the ref lives on a sibling entry.
    for bad_toml in [
        r#"
schema_version = 3
[[model_routes]]
hint = "dup"
provider = "openai.default"
model = "gpt-5"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
[[model_routes]]
hint = "dup"
provider = "openai.default"
model = "gpt-5"
"#,
        r#"
schema_version = 3
[[model_routes]]
hint = ""
provider = "openai.default"
model = "gpt-5"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
        r#"
schema_version = 3
[[mcp.servers]]
name = "a"
command = "npx"
[mcp.servers.env]
TOKEN = "vault:OPENAI_OFFICIAL_API_KEY"
[[mcp.servers]]
name = ""
command = "uvx"
"#,
        r#"
schema_version = 3
[[mcp.servers]]
name = "a"
command = "npx"
[mcp.servers.env]
TOKEN = "vault:OPENAI_OFFICIAL_API_KEY"
[[mcp.servers]]
name = "dup"
command = "uvx"
[[mcp.servers]]
name = "dup"
command = "node"
"#,
        r#"
schema_version = 3
[providers.models.openai.default]
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
[providers.models.openai.""]
model = "gpt-5"
"#,
    ] {
        std::fs::write(dir.path().join("config.toml"), bad_toml).unwrap();
        assert!(
            Config::load_or_init().await.is_err(),
            "expected section-wide key invariant failure for:\n{bad_toml}"
        );
    }
}

#[cfg(unix)]
#[test]
async fn tachi_vault_whole_section_and_entry_dirty_expands_refs_and_isolates_env() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let config_path = dir.path().join("config.toml");
    std::fs::remove_file(&config_path).unwrap();

    // 1. Missing-file create fallback with section dirty `model_routes` carrying an
    // invalid `vault:-invalid` reference must fail closed without creating disk state.
    let mut missing_cfg = Config {
        config_path: config_path.clone(),
        data_dir: dir.path().join("workspace"),
        ..Config::default()
    };
    missing_cfg.secrets.encrypt = false;
    missing_cfg.model_routes.push(ModelRouteConfig {
        hint: "fast".to_string(),
        model_provider: "openai.default".to_string(),
        model: "gpt-5-mini".to_string(),
        api_key: Some("vault:-invalid".to_string()),
    });
    missing_cfg.mark_dirty("model_routes");
    assert!(missing_cfg.save_dirty().await.is_err());
    assert!(!config_path.exists());

    // 2. Valid `vault:OPENAI_OFFICIAL_API_KEY` under `mark_dirty("model_routes")` creates
    // disk with the external reference and resolves plaintext in live memory.
    missing_cfg.model_routes[0].api_key = Some("vault:OPENAI_OFFICIAL_API_KEY".to_string());
    missing_cfg.save_dirty().await.unwrap();
    assert_eq!(
        missing_cfg.model_routes[0].api_key.as_deref(),
        Some("synthetic-vault-secret")
    );
    let raw_created = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_created.contains("vault:OPENAI_OFFICIAL_API_KEY")
            && !raw_created.contains("synthetic-vault-secret"),
        "{raw_created}"
    );

    // 3. Replacing that route's external ref under section dirty `mark_dirty("model_routes")`
    // persists the new reference on disk, updates the snapshot, and resolves plaintext live.
    missing_cfg.model_routes[0].api_key = Some("op://vault/item/field".to_string());
    missing_cfg.mark_dirty("model_routes");
    missing_cfg.save_dirty().await.unwrap();
    assert_eq!(
        missing_cfg.model_routes[0].api_key.as_deref(),
        Some("synthetic-op-secret")
    );
    assert_eq!(
        missing_cfg
            .onepassword_reference_snapshots
            .get("model_routes.fast.api_key")
            .as_deref(),
        Some("op://vault/item/field")
    );
    let raw_replaced = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_replaced.contains("op://vault/item/field")
            && !raw_replaced.contains("synthetic-op-secret")
            && !raw_replaced.contains("vault:OPENAI_OFFICIAL_API_KEY"),
        "{raw_replaced}"
    );

    // 4. Whole-entry dirty (`mark_dirty("providers.models.openai.default")`) fails closed
    // on `vault:-bad` and persists/resolves valid `vault:OPENAI_OFFICIAL_API_KEY`.
    missing_cfg.providers.models.openai.insert(
        "default".to_string(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("gpt-5.4".to_string()),
                api_key: Some("vault:-bad".to_string()),
                ..Default::default()
            },
        },
    );
    missing_cfg.mark_dirty("providers.models.openai.default");
    let pre_provider_disk = std::fs::read_to_string(&config_path).unwrap();
    assert!(missing_cfg.save_dirty().await.is_err());
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        pre_provider_disk
    );

    missing_cfg
        .providers
        .models
        .openai
        .get_mut("default")
        .unwrap()
        .base
        .api_key = Some("vault:OPENAI_OFFICIAL_API_KEY".to_string());
    missing_cfg.save_dirty().await.unwrap();
    assert_eq!(
        missing_cfg
            .providers
            .models
            .openai
            .get("default")
            .and_then(|p| p.base.api_key.as_deref()),
        Some("synthetic-vault-secret")
    );
    let raw_provider = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_provider.contains("vault:OPENAI_OFFICIAL_API_KEY")
            && !raw_provider.contains("synthetic-vault-secret"),
        "{raw_provider}"
    );

    // 5. Deleting all entries under section dirty `mark_dirty("model_routes")` succeeds on both
    // `save_dirty()` and `save()` and prunes stale external reference snapshots cleanly.
    missing_cfg.model_routes.clear();
    missing_cfg.mark_dirty("model_routes");
    missing_cfg.save_dirty().await.unwrap();
    assert!(
        !missing_cfg
            .onepassword_reference_snapshots
            .contains_key("model_routes.fast.api_key")
    );
    missing_cfg.mark_dirty("model_routes");
    missing_cfg.save().await.unwrap();

    // 6. Env-override isolation under `mark_dirty("model_routes")`: an env-overridden
    // sibling route must keep its pre-override disk value and must NEVER leak or get
    // absorbed into `onepassword_reference_snapshots` when editing another route.
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[[model_routes]]
hint = "env_only"
provider = "openai.default"
model = "gpt-5-nano"
api_key = "disk-inline"

[[model_routes]]
hint = "editable"
provider = "openai.default"
model = "gpt-5-mini"
"#,
    )
    .unwrap();
    let _env_guard = EnvValueGuard::set(
        "ZEROCLAW_model_routes__env_only__api_key",
        "vault:OPENAI_OFFICIAL_API_KEY",
    );
    let mut env_cfg = Config::load_or_init().await.unwrap();
    assert_eq!(
        env_cfg.model_routes[0].api_key.as_deref(),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    assert!(
        !env_cfg
            .onepassword_reference_snapshots
            .contains_key("model_routes.env_only.api_key")
    );
    env_cfg.model_routes[1].api_key = Some("op://vault/item/field".to_string());
    env_cfg.mark_dirty("model_routes");
    env_cfg.save_dirty().await.unwrap();
    assert!(
        !env_cfg
            .onepassword_reference_snapshots
            .contains_key("model_routes.env_only.api_key")
    );
    assert_eq!(
        env_cfg.model_routes[1].api_key.as_deref(),
        Some("synthetic-op-secret")
    );
    let raw_env = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_env.contains("disk-inline")
            && !raw_env.contains("vault:OPENAI_OFFICIAL_API_KEY")
            && raw_env.contains("op://vault/item/field")
            && !raw_env.contains("synthetic-op-secret"),
        "{raw_env}"
    );
}

#[cfg(unix)]
#[test]
async fn tachi_vault_direct_save_fails_closed_on_unmarked_invalid_ref() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    std::fs::write(
        dir.path().join("config.toml"),
        r#"
schema_version = 3

[secrets]
encrypt = false

[[model_routes]]
hint = "fast"
provider = "openai.default"
model = "gpt-5-mini"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
    )
    .unwrap();

    let mut cfg = Config::load_or_init().await.unwrap();
    let before_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    cfg.model_routes[0].api_key = Some("vault:-invalid".to_string());
    assert!(cfg.save().await.is_err());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
        before_disk
    );
}

#[cfg(unix)]
#[test]
async fn tachi_vault_paired_tokens_per_slot_append_and_empty_clear_no_leak() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[gateway]
paired_tokens = ["vault:OPENAI_OFFICIAL_API_KEY"]
"#,
    )
    .unwrap();

    let mut cfg = Config::load_or_init().await.unwrap();
    assert_eq!(cfg.gateway.paired_tokens, vec!["synthetic-vault-secret"]);

    // 1. Unmarked append + save(): slot [1] failing invalid ref fails closed without touching disk.
    let mut bad_unmarked = cfg.clone();
    bad_unmarked
        .gateway
        .paired_tokens
        .push("vault:-invalid".to_string());
    assert!(bad_unmarked.save().await.is_err());
    let disk_after_bad = std::fs::read_to_string(&config_path).unwrap();
    assert!(!disk_after_bad.contains("vault:-invalid"));
    assert!(!disk_after_bad.contains("synthetic-vault-secret"));

    // 2. Unmarked append + save(): slot [0] must keep its vault: snapshot on disk (never leak
    // plaintext), while new slot [1] persists its op:// reference.
    let mut good_unmarked = cfg.clone();
    good_unmarked
        .gateway
        .paired_tokens
        .push("op://vault/item/field".to_string());
    good_unmarked.save().await.unwrap();
    let raw_unmarked = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_unmarked.contains("vault:OPENAI_OFFICIAL_API_KEY")
            && raw_unmarked.contains("op://vault/item/field")
            && !raw_unmarked.contains("synthetic-vault-secret")
            && !raw_unmarked.contains("synthetic-op-secret"),
        "{raw_unmarked}"
    );

    // Reload clean [vault:OPENAI_OFFICIAL_API_KEY] fixture to test section-dirty save_dirty().
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[gateway]
paired_tokens = ["vault:OPENAI_OFFICIAL_API_KEY"]
"#,
    )
    .unwrap();
    cfg = Config::load_or_init().await.unwrap();

    // 3. Section-dirty invalid append fails closed without partial write or memory mutation.
    cfg.gateway.paired_tokens.push("vault:-invalid".to_string());
    cfg.mark_dirty("gateway");
    assert!(cfg.save_dirty().await.is_err());
    let disk_before_good_dirty = std::fs::read_to_string(&config_path).unwrap();
    assert!(!disk_before_good_dirty.contains("vault:-invalid"));
    assert!(!disk_before_good_dirty.contains("synthetic-vault-secret"));

    // 4. Section-dirty valid append resolves [1] live and preserves [0]'s vault: snapshot on disk.
    cfg.gateway.paired_tokens[1] = "op://vault/item/field".to_string();
    cfg.mark_dirty("gateway");
    cfg.save_dirty().await.unwrap();
    assert_eq!(
        cfg.gateway.paired_tokens,
        vec![
            "synthetic-vault-secret".to_string(),
            "synthetic-op-secret".to_string()
        ]
    );
    assert_eq!(
        cfg.onepassword_reference_snapshots
            .get("gateway.paired_tokens[0]")
            .as_deref(),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    assert_eq!(
        cfg.onepassword_reference_snapshots
            .get("gateway.paired_tokens[1]")
            .as_deref(),
        Some("op://vault/item/field")
    );
    let raw_dirty = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_dirty.contains("vault:OPENAI_OFFICIAL_API_KEY")
            && raw_dirty.contains("op://vault/item/field")
            && !raw_dirty.contains("synthetic-vault-secret")
            && !raw_dirty.contains("synthetic-op-secret"),
        "{raw_dirty}"
    );

    // 5. Popping slot [1] under `mark_dirty("gateway")` prunes snapshot [1] while keeping [0].
    cfg.gateway.paired_tokens.pop();
    cfg.mark_dirty("gateway");
    cfg.save_dirty().await.unwrap();
    assert!(
        !cfg.onepassword_reference_snapshots
            .contains_key("gateway.paired_tokens[1]")
    );
    assert_eq!(
        cfg.onepassword_reference_snapshots
            .get("gateway.paired_tokens[0]")
            .as_deref(),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );

    // 6. Clearing slot [0] to empty string under `mark_dirty("gateway")` does not resurrect
    // the old `vault:` snapshot on disk.
    cfg.gateway.paired_tokens[0].clear();
    cfg.mark_dirty("gateway");
    cfg.save_dirty().await.unwrap();
    let raw_cleared_slot = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !raw_cleared_slot.contains("vault:OPENAI_OFFICIAL_API_KEY"),
        "{raw_cleared_slot}"
    );
    assert!(
        !cfg.onepassword_reference_snapshots
            .contains_key("gateway.paired_tokens[0]")
    );
}

#[cfg(unix)]
#[test]
async fn tachi_vault_dotted_mcp_sibling_collision_fails_closed_and_resolves_structurally() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[[mcp.servers]]
name = "a"
command = "npx"

[mcp.servers.env]
"env.TOKEN" = "disk-a"

[[mcp.servers]]
name = "a.env"
command = "uvx"

[mcp.servers.env]
TOKEN = "disk-a-env"
"#,
    )
    .unwrap();

    let mut cfg = Config::load_or_init().await.unwrap();

    // 1. Mutating server "a"'s env["env.TOKEN"] to `vault:-invalid` must fail closed under
    // `mark_dirty("mcp.servers")`, `mark_dirty("mcp.servers.a")`, and `save()`, never letting
    // dotted prop_fields collisions with sibling server "a.env"'s `env["TOKEN"]` bypass checks.
    for dirty_target in [Some("mcp.servers"), Some("mcp.servers.a"), None] {
        let mut attempt = cfg.clone();
        attempt.mcp.servers[0]
            .env
            .insert("env.TOKEN".to_string(), "vault:-invalid".to_string());
        if let Some(path) = dirty_target {
            attempt.mark_dirty(path);
            assert!(
                attempt.save_dirty().await.is_err(),
                "dirty_target={path} should fail closed on vault:-invalid"
            );
        } else {
            assert!(attempt.save().await.is_err());
        }
        let disk = std::fs::read_to_string(&config_path).unwrap();
        assert!(!disk.contains("vault:-invalid"), "{disk}");
    }

    // 2. Setting valid `vault:OPENAI_OFFICIAL_API_KEY` on server "a"'s env["env.TOKEN"] under
    // `mark_dirty("mcp.servers")` must resolve onto server "a" without touching sibling "a.env".
    cfg.mcp.servers[0].env.insert(
        "env.TOKEN".to_string(),
        "vault:OPENAI_OFFICIAL_API_KEY".to_string(),
    );
    cfg.mark_dirty("mcp.servers");
    cfg.save_dirty().await.unwrap();
    assert_eq!(
        cfg.mcp.servers[0].env.get("env.TOKEN").map(String::as_str),
        Some("synthetic-vault-secret")
    );
    assert_eq!(
        cfg.mcp.servers[1].env.get("TOKEN").map(String::as_str),
        Some("disk-a-env")
    );
    assert_eq!(
        cfg.onepassword_reference_snapshots
            .get("mcp.servers.a.env.env%2ETOKEN")
            .as_deref(),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    assert!(
        !cfg.onepassword_reference_snapshots
            .contains_key("mcp.servers.a%2Eenv.env.TOKEN")
    );
    let parsed_after: Config =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        parsed_after.mcp.servers[0]
            .env
            .get("env.TOKEN")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    assert_eq!(
        parsed_after.mcp.servers[1]
            .env
            .get("TOKEN")
            .map(String::as_str),
        Some("disk-a-env")
    );

    // 2b. Replacing server "a"'s env["env.TOKEN"] under whole-entry dirty
    // `mark_dirty("mcp.servers.a")` updates server "a" and keeps sibling "a.env" untouched.
    cfg.mcp.servers[0]
        .env
        .insert("env.TOKEN".to_string(), "op://vault/item/field".to_string());
    cfg.mark_dirty("mcp.servers.a");
    cfg.save_dirty().await.unwrap();
    assert_eq!(
        cfg.mcp.servers[0].env.get("env.TOKEN").map(String::as_str),
        Some("synthetic-op-secret")
    );
    assert_eq!(
        cfg.onepassword_reference_snapshots
            .get("mcp.servers.a.env.env%2ETOKEN")
            .as_deref(),
        Some("op://vault/item/field")
    );
    let parsed_entry_dirty: Config =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(
        parsed_entry_dirty.mcp.servers[0]
            .env
            .get("env.TOKEN")
            .map(String::as_str),
        Some("op://vault/item/field")
    );
    assert_eq!(
        parsed_entry_dirty.mcp.servers[1]
            .env
            .get("TOKEN")
            .map(String::as_str),
        Some("disk-a-env")
    );

    // 3. Clearing server "a"'s env["env.TOKEN"] to "" under `mark_dirty("mcp.servers")` does not
    // resurrect the old `vault:` reference on disk or keep a stale snapshot.
    cfg.mcp.servers[0]
        .env
        .insert("env.TOKEN".to_string(), String::new());
    cfg.mark_dirty("mcp.servers");
    cfg.save_dirty().await.unwrap();
    let parsed_cleared: Config =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_ne!(
        parsed_cleared.mcp.servers[0]
            .env
            .get("env.TOKEN")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );
    assert!(
        !cfg.onepassword_reference_snapshots
            .contains_key("mcp.servers.a.env.env%2ETOKEN")
    );
}

#[cfg(unix)]
#[test]
async fn tachi_vault_full_save_prunes_stale_snapshots_and_creates_nested_mcp_subtable() {
    let _lock = env_override_lock().await;
    let (dir, _guards) = tachi_vault_fixture("vault:OPENAI_OFFICIAL_API_KEY", false);
    let config_path = dir.path().join("config.toml");

    // 1. Standalone `save()` after clearing a resolved `paired_tokens` slot must prune the stale
    // snapshot so a subsequent `mark_dirty("gateway") + save_dirty()` with `new-token` never
    // resurrects the old `vault:` reference on disk.
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[gateway]
paired_tokens = ["vault:OPENAI_OFFICIAL_API_KEY"]
"#,
    )
    .unwrap();
    let mut cfg = Config::load_or_init().await.unwrap();
    assert_eq!(cfg.gateway.paired_tokens, vec!["synthetic-vault-secret"]);
    cfg.gateway.paired_tokens[0].clear();
    cfg.save().await.unwrap();
    assert!(
        !cfg.onepassword_reference_snapshots
            .contains_key("gateway.paired_tokens[0]")
    );
    let raw_after_clear = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !raw_after_clear.contains("vault:OPENAI_OFFICIAL_API_KEY"),
        "{raw_after_clear}"
    );

    cfg.gateway.paired_tokens[0] = "new-token".to_string();
    cfg.mark_dirty("gateway");
    cfg.save_dirty().await.unwrap();
    assert_eq!(cfg.gateway.paired_tokens, vec!["new-token"]);
    let raw_reassigned = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_reassigned.contains("new-token")
            && !raw_reassigned.contains("vault:OPENAI_OFFICIAL_API_KEY"),
        "{raw_reassigned}"
    );

    // 2. Standalone `model_routes.clear(); mark_dirty("model_routes"); save(); clear_dirty(); save()`
    // on a fresh loaded Config (without calling `save_dirty()` first) must prune snapshots on the
    // first `save()` so the subsequent `save()` does not fail or resurrect references.
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[[model_routes]]
hint = "fast"
provider = "openai.default"
model = "gpt-5-mini"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
    )
    .unwrap();
    let mut routes_cfg = Config::load_or_init().await.unwrap();
    assert!(
        routes_cfg
            .onepassword_reference_snapshots
            .contains_key("model_routes.fast.api_key")
    );
    routes_cfg.model_routes.clear();
    routes_cfg.mark_dirty("model_routes");
    routes_cfg.save().await.unwrap();
    assert!(
        !routes_cfg
            .onepassword_reference_snapshots
            .contains_key("model_routes.fast.api_key")
    );
    routes_cfg.clear_dirty();
    routes_cfg.save().await.unwrap();
    let raw_routes_cleared = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !raw_routes_cleared.contains("model_routes")
            && !raw_routes_cleared.contains("vault:OPENAI_OFFICIAL_API_KEY"),
        "{raw_routes_cleared}"
    );

    // 3. Replacing a provider secret via `set_prop_persistent` + standalone `save()` (plus
    // `force_save()` coverage) must prune the superseded snapshot and avoid resurrection.
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[providers.models.openai.default]
model = "gpt-5.4"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#,
    )
    .unwrap();
    let mut prop_cfg = Config::load_or_init().await.unwrap();
    prop_cfg
        .set_prop_persistent(
            "providers.models.openai.default.api_key",
            "replacement-plain",
        )
        .unwrap();
    prop_cfg.save().await.unwrap();
    assert!(
        !prop_cfg
            .onepassword_reference_snapshots
            .contains_key("providers.models.openai.default.api_key")
    );
    prop_cfg.clear_dirty();
    prop_cfg.force_save().await.unwrap();
    let raw_replaced = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_replaced.contains("replacement-plain")
            && !raw_replaced.contains("vault:OPENAI_OFFICIAL_API_KEY"),
        "{raw_replaced}"
    );

    // 4. Adding a new `[mcp.servers.env]` subtable to an existing `[[mcp.servers]]` entry that
    // previously had no `env` table under `mark_dirty("mcp.servers.no_env") + save_dirty()` must
    // emit valid TOML with `[mcp.servers.env]` positioned under its parent entry.
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[secrets]
encrypt = false

[[mcp.servers]]
name = "no_env"
command = "npx"
"#,
    )
    .unwrap();
    let mut mcp_cfg = Config::load_or_init().await.unwrap();
    mcp_cfg.mcp.servers[0].env.insert(
        "API_TOKEN".to_string(),
        "vault:OPENAI_OFFICIAL_API_KEY".to_string(),
    );
    mcp_cfg.mark_dirty("mcp.servers.no_env");
    mcp_cfg.save_dirty().await.unwrap();
    assert_eq!(
        mcp_cfg.mcp.servers[0]
            .env
            .get("API_TOKEN")
            .map(String::as_str),
        Some("synthetic-vault-secret")
    );
    let raw_mcp = std::fs::read_to_string(&config_path).unwrap();
    let parsed_mcp: Config = toml::from_str(&raw_mcp).expect(&raw_mcp);
    assert_eq!(
        parsed_mcp.mcp.servers[0]
            .env
            .get("API_TOKEN")
            .map(String::as_str),
        Some("vault:OPENAI_OFFICIAL_API_KEY")
    );

    // 5. Env-overridden Vault field explicitly replaced via `set_prop_persistent` and persisted
    // with standalone `save()` (before `clear_dirty()` + `force_save()` + `save()`) must sync
    // `pre_override_snapshots` and `env_overridden_paths` alongside `onepassword_reference_snapshots`
    // so subsequent full saves never mask stale plaintext back onto disk (both encrypt=false/true).
    for encrypt in [false, true] {
        std::fs::write(
            &config_path,
            format!(
                r#"
schema_version = 3

[secrets]
encrypt = {encrypt}

[providers.models.openai.default]
model = "gpt-5.4"
api_key = "vault:OPENAI_OFFICIAL_API_KEY"
"#
            ),
        )
        .unwrap();
        let _env_guard = EnvValueGuard::set(
            "ZEROCLAW_providers__models__openai__default__api_key",
            "synthetic-env-override",
        );
        let path = "providers.models.openai.default.api_key";
        let mut env_cfg = Config::load_or_init().await.unwrap();
        assert!(env_cfg.prop_is_env_overridden(path));
        assert!(env_cfg.pre_override_snapshots.contains_key(path));
        assert!(env_cfg.onepassword_reference_snapshots.contains_key(path));

        env_cfg
            .set_prop_persistent(path, "replacement-plain")
            .unwrap();
        env_cfg.save().await.unwrap();
        assert!(!env_cfg.onepassword_reference_snapshots.contains_key(path));
        assert!(!env_cfg.pre_override_snapshots.contains_key(path));
        assert!(!env_cfg.env_overridden_paths.contains(path));
        assert!(!env_cfg.prop_is_env_overridden(path));

        env_cfg.clear_dirty();
        env_cfg.force_save().await.unwrap();
        env_cfg.save().await.unwrap();

        let raw_after = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            !raw_after.contains("synthetic-vault-secret")
                && !raw_after.contains("synthetic-env-override")
                && !raw_after.contains("vault:OPENAI_OFFICIAL_API_KEY"),
            "encrypt={encrypt}: {raw_after}"
        );
        let mut parsed_after: Config = toml::from_str(&raw_after).unwrap();
        parsed_after
            .decrypt_secrets(&crate::secrets::SecretStore::new(dir.path(), encrypt))
            .unwrap();
        assert_eq!(
            crate::env_overrides::raw_value_for_path(&parsed_after, path).as_deref(),
            Some("replacement-plain")
        );
    }
}

#[cfg(unix)]
#[test]
async fn onepassword_reference_survives_load_save_cycle() {
    let _env_guard = env_override_lock().await;
    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write_fake_op(
        &bin_dir,
        r#"#!/bin/sh
if [ "$1" = "read" ] && [ "$2" = "op://zeroclaw/provider/openai-api-key" ]; then
  printf '%s\n' 'sk-proj-from-onepassword'
  exit 0
fi
printf '%s\n' 'unexpected op invocation' >&2
exit 65
"#,
    );
    let path = match std::env::var_os("PATH") {
        Some(existing) if !existing.is_empty() => {
            format!("{}:{}", bin_dir.display(), existing.to_string_lossy())
        }
        _ => bin_dir.display().to_string(),
    };
    let _path_guard = EnvValueGuard::set("PATH", path);
    let _config_guard = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", dir.path());
    let _workspace_guard = EnvValueGuard::remove("ZEROCLAW_WORKSPACE");

    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[providers.models.openai.default]
model = "gpt-5"
api_key = "op://zeroclaw/provider/openai-api-key"
"#,
    )
    .unwrap();

    let config = Config::load_or_init().await.unwrap();
    assert_eq!(
        config
            .providers
            .models
            .openai
            .get("default")
            .and_then(|entry| entry.base.api_key.as_deref()),
        Some("sk-proj-from-onepassword"),
        "runtime config uses resolved 1Password secret"
    );

    config.save().await.unwrap();
    let raw_after = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        raw_after.contains("op://zeroclaw/provider/openai-api-key"),
        "on-disk config must keep the 1Password reference: {raw_after}"
    );
    assert!(
        !raw_after.contains("sk-proj-from-onepassword"),
        "resolved secret must not be written back to disk: {raw_after}"
    );
}

#[cfg(unix)]
#[allow(
    clippy::disallowed_methods,
    reason = "test asserts Tokio worker responsiveness"
)]
#[test(flavor = "multi_thread", worker_threads = 1)]
async fn onepassword_reference_load_does_not_block_runtime_worker() {
    let _env_guard = env_override_lock().await;
    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write_fake_op(
        &bin_dir,
        r#"#!/bin/sh
if [ "$1" = "read" ] && [ "$2" = "op://zeroclaw/provider/openai-api-key" ]; then
  sleep 1
  printf '%s\n' 'sk-proj-from-onepassword'
  exit 0
fi
exit 65
"#,
    );
    let path = match std::env::var_os("PATH") {
        Some(existing) if !existing.is_empty() => {
            format!("{}:{}", bin_dir.display(), existing.to_string_lossy())
        }
        _ => bin_dir.display().to_string(),
    };
    let _path_guard = EnvValueGuard::set("PATH", path);
    let _config_guard = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", dir.path());
    let _workspace_guard = EnvValueGuard::remove("ZEROCLAW_WORKSPACE");

    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[providers.models.openai.default]
model = "gpt-5"
api_key = "op://zeroclaw/provider/openai-api-key"
"#,
    )
    .unwrap();

    let started = std::time::Instant::now();
    let load_task = tokio::spawn(Config::load_or_init());
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "op:// config load should not block the async runtime worker"
    );

    let config = load_task.await.unwrap().unwrap();
    assert_eq!(
        config
            .providers
            .models
            .openai
            .get("default")
            .and_then(|entry| entry.base.api_key.as_deref()),
        Some("sk-proj-from-onepassword")
    );
}

#[cfg(unix)]
#[test]
async fn dirty_onepassword_secret_edit_replaces_reference() {
    let _env_guard = env_override_lock().await;
    let dir = TempDir::new().unwrap();
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write_fake_op(
        &bin_dir,
        r#"#!/bin/sh
printf '%s\n' 'sk-proj-from-onepassword'
"#,
    );
    let path = match std::env::var_os("PATH") {
        Some(existing) if !existing.is_empty() => {
            format!("{}:{}", bin_dir.display(), existing.to_string_lossy())
        }
        _ => bin_dir.display().to_string(),
    };
    let _path_guard = EnvValueGuard::set("PATH", path);
    let _config_guard = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", dir.path());
    let _workspace_guard = EnvValueGuard::remove("ZEROCLAW_WORKSPACE");

    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"
schema_version = 3

[providers.models.openai.default]
model = "gpt-5"
api_key = "op://zeroclaw/provider/openai-api-key"
"#,
    )
    .unwrap();

    let mut config = Config::load_or_init().await.unwrap();
    config
        .set_prop_persistent(
            "providers.models.openai.default.api_key",
            "sk-proj-new-direct-key",
        )
        .unwrap();
    config.save_dirty().await.unwrap();

    let raw_after = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !raw_after.contains("op://zeroclaw/provider/openai-api-key"),
        "dirty secret edits should replace the old 1Password reference: {raw_after}"
    );
    assert!(
        !raw_after.contains("sk-proj-new-direct-key"),
        "direct replacement should still be encrypted at rest: {raw_after}"
    );

    let stored: Config = toml::from_str(&raw_after).unwrap();
    let encrypted = stored
        .providers
        .models
        .openai
        .get("default")
        .and_then(|entry| entry.base.api_key.as_deref())
        .unwrap();
    let store = crate::secrets::SecretStore::new(dir.path(), true);
    assert_eq!(store.decrypt(encrypted).unwrap(), "sk-proj-new-direct-key");
}

#[test]
async fn enum_variants_callback_returns_values() {
    let mx = test_telegram_config();
    let fields = mx.prop_fields();
    let stream_field = fields
        .iter()
        .find(|f| f.name == "channels.telegram.stream_mode")
        .unwrap();
    let variants = (stream_field.enum_variants.unwrap())();
    assert!(variants.contains(&"off".to_string()));
    assert!(variants.contains(&"partial".to_string()));
    assert!(variants.contains(&"multi_message".to_string()));
}

#[test]
async fn map_key_sections_discovers_per_family_provider_slots() {
    // Typed-family split: `providers.models` is a struct of typed
    // family maps, not a single open HashMap. Each family slot
    // (`providers.models.<family>`) is its own Map-kind section; the
    // dashboard's "+ Add alias" affordance hangs off the family path.
    let sections = Config::map_key_sections();
    let anthropic = sections
        .iter()
        .find(|s| s.path == "providers.models.anthropic")
        .expect("providers.models.anthropic must be discoverable as a map-keyed section");
    assert_eq!(anthropic.kind, crate::traits::MapKeyKind::Map);
    assert_eq!(anthropic.value_type, "AnthropicModelProviderConfig");

    // agents is also #[nested] HashMap on root Config.
    assert!(
        sections.iter().any(|s| s.path == "agents"),
        "agents map should be discoverable"
    );

    // mcp.servers is a Vec<McpServerConfig> with #[nested] — should
    // surface as a List-kind section so the dashboard's "+ Add MCP
    // server" affordance picks it up. Without this, dashboard users
    // hit a silent dead-end and have to hand-edit config.toml. Pinned
    // here so a regression that drops the #[nested] annotation or the
    // Configurable derive on McpServerConfig fails CI.
    let mcp_servers = sections
        .iter()
        .find(|s| s.path == "mcp.servers")
        .expect("mcp.servers must be discoverable as a list-shaped section");
    assert_eq!(mcp_servers.kind, crate::traits::MapKeyKind::List);
    assert_eq!(mcp_servers.value_type, "McpServerConfig");
}

#[test]
async fn create_map_key_inserts_default_mcp_server() {
    // Round-trip: `POST /api/config/map-key?path=mcp.servers&key=github`.
    // The new entry's `name` field is initialized to the supplied key
    // by the macro's List-kind insertion logic.
    let mut config = Config::default();
    assert!(config.mcp.servers.is_empty());

    let created = config
        .create_map_key("mcp.servers", "github")
        .expect("mcp.servers should accept new list entries");
    assert!(created, "first add should report created=true");
    assert_eq!(config.mcp.servers.len(), 1);
    assert_eq!(
        config.mcp.servers[0].name, "github",
        "new entry must carry the supplied key as its name field"
    );
}

#[test]
async fn create_map_key_inserts_default_alias_under_typed_family() {
    // Dashboard "+ Add alias" target is the typed family slot,
    // not a free-form provider key under `providers.models`.
    let mut config = Config::default();
    assert!(
        !config
            .providers
            .models
            .contains_model_provider_type("anthropic")
    );

    let created = config
        .create_map_key("providers.models.anthropic", "default")
        .expect("typed family slot should accept a new alias");
    assert!(created, "first add should report created=true");
    assert!(
        config
            .providers
            .models
            .find("anthropic", "default")
            .is_some(),
        "the new alias must show up under the typed family slot",
    );

    // Idempotent: second add returns false, doesn't error.
    let again = config
        .create_map_key("providers.models.anthropic", "default")
        .expect("second add still resolves the section");
    assert!(!again, "duplicate add should report created=false");
}

#[test]
async fn ensure_map_key_for_path_materializes_typed_provider_maps() {
    for (path, value) in [
        ("providers.models.openai.default.model", "gpt-4o"),
        ("providers.tts.openai.default.voice", "alloy"),
        ("providers.transcription.openai.default.model", "whisper-1"),
        ("channels.telegram.default.bot_token", "tok"),
    ] {
        let mut config = Config::default();
        assert!(
            config.set_prop(path, value).is_err(),
            "precondition: {path} is unknown on a fresh config"
        );
        config.ensure_map_key_for_path(path);
        assert!(
            config.set_prop(path, value).is_ok(),
            "{path} must be settable after ensure_map_key_for_path"
        );
    }
}

#[test]
async fn ensure_map_key_for_path_ignores_plain_fields() {
    let mut config = Config::default();
    config.ensure_map_key_for_path("gateway.port");
    config.ensure_map_key_for_path("locale");
    assert!(config.set_prop("gateway.port", "8080").is_ok());
}

// ── nested map-routed set_prop must not mask value errors as
// "Unknown property" ────────────────────────────────────────────────
//
// Once the router/key lookup has confirmed a path belongs to a
// materialized map alias, a failure from the inner `set_prop` call is a
// real value problem (bad type, bad enum variant, ...) and must
// propagate as-is rather than being swallowed into the generic
// "Unknown property" fallback (which downstream consumers, e.g.
// `zeroclaw-gateway`'s `map_prop_error` and `src/main.rs`'s
// `config_patch_map_prop_error`, translate into a 404 PathNotFound
// instead of a 400 ValueTypeMismatch).

#[track_caller]
fn assert_value_error(err: &str) {
    assert!(
        !err.starts_with("Unknown property"),
        "bad value must not be reported as an unknown path: {err}"
    );
    assert!(
        err.contains("bool")
            || err.contains("Invalid")
            || err.contains("invalid")
            || err.contains("expected"),
        "error should describe the value problem: {err}"
    );
}

#[track_caller]
fn assert_unknown_property(err: &str) {
    assert!(
        err.starts_with("Unknown property"),
        "an unknown leaf must still surface as a path problem (404), got: {err}"
    );
}

#[test]
async fn set_prop_single_level_map_rejects_invalid_value_not_unknown_property() {
    let mut config = Config::default();
    config.ensure_map_key_for_path("channels.telegram.default.bot_token");
    let err = config
        .set_prop("channels.telegram.default.enabled", "notabool")
        .unwrap_err()
        .to_string();
    assert_value_error(&err);
}

#[test]
async fn set_prop_single_level_map_accepts_valid_value() {
    let mut config = Config::default();
    config.ensure_map_key_for_path("channels.telegram.default.bot_token");
    config
        .set_prop("channels.telegram.default.enabled", "true")
        .unwrap();
    assert!(
        config
            .channels
            .telegram
            .get("default")
            .expect("alias materialized by ensure_map_key_for_path")
            .enabled
    );
}

#[test]
async fn set_prop_single_level_map_unknown_leaf_still_unknown_property() {
    let mut config = Config::default();
    config.ensure_map_key_for_path("channels.telegram.default.bot_token");
    let err = config
        .set_prop("channels.telegram.default.nonexistent_field", "x")
        .unwrap_err()
        .to_string();
    assert_unknown_property(&err);
}

// The production schema has no `#[nested] HashMap<String, HashMap<String,
// T: Configurable>>` field today (every `providers.models.<type>` slot is
// itself a single-level `HashMap<String, T>` field of a plain nested
// struct, not a hashmap key) — so the two-level routing branch in
// `derive_configurable` (crates/zeroclaw-macros/src/lib.rs, the
// `double_value_ty` arm) can't be exercised through `Config` directly.
// Exercise it directly with a minimal local fixture instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "dm_sub"]
struct DoubleMapSub {
    #[serde(default)]
    pub value: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "dm_leaf"]
struct DoubleMapLeaf {
    #[serde(default)]
    pub flag: bool,
    #[serde(default)]
    #[nested]
    pub sub: DoubleMapSub,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "dm"]
struct DoubleMapOuter {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub types: HashMap<String, HashMap<String, DoubleMapLeaf>>,
}

fn double_map_fixture() -> DoubleMapOuter {
    let mut outer = DoubleMapOuter::default();
    outer
        .types
        .entry("anthropic".to_string())
        .or_default()
        .insert("default".to_string(), DoubleMapLeaf::default());
    // Dotted-outer-key ambiguity: `dm.types.a.x.sub.value` splits as
    // outer="a.x"/inner="sub" (longest outer key wins, tried first)
    // AND as outer="a"/inner="x" — the candidate loop must be able to
    // retry the shorter split when the longest one dead-ends.
    outer
        .types
        .entry("a".to_string())
        .or_default()
        .insert("x".to_string(), DoubleMapLeaf::default());
    outer
        .types
        .entry("a.x".to_string())
        .or_default()
        .insert("sub".to_string(), DoubleMapLeaf::default());
    outer
}

#[test]
async fn set_prop_double_level_map_rejects_invalid_value_not_unknown_property() {
    let mut outer = double_map_fixture();
    let err = outer
        .set_prop("dm.types.anthropic.default.flag", "notabool")
        .unwrap_err()
        .to_string();
    assert_value_error(&err);
}

#[test]
async fn set_prop_double_level_map_accepts_valid_value() {
    let mut outer = double_map_fixture();
    outer
        .set_prop("dm.types.anthropic.default.flag", "true")
        .unwrap();
    assert!(outer.types["anthropic"]["default"].flag);
}

#[test]
async fn set_prop_double_level_map_unknown_leaf_still_unknown_property() {
    let mut outer = double_map_fixture();
    let err = outer
        .set_prop("dm.types.anthropic.default.nonexistent_field", "x")
        .unwrap_err()
        .to_string();
    assert_unknown_property(&err);
}

#[test]
async fn set_prop_double_level_map_dotted_outer_key_retries_next_candidate() {
    // The longest candidate split (outer="a.x"/inner="sub") is tried
    // first and its leaf lookup yields "Unknown property" —
    // `dm_leaf.value` is not a direct DoubleMapLeaf field. The loop
    // must fall through to outer="a"/inner="x", whose nested
    // `sub.value` resolves — keeping set_prop in agreement with
    // get_prop's retry semantics on the same path.
    let mut outer = double_map_fixture();
    outer.set_prop("dm.types.a.x.sub.value", "true").unwrap();
    assert!(outer.types["a"]["x"].sub.value);
    assert_eq!(outer.get_prop("dm.types.a.x.sub.value").unwrap(), "true");
}

// ── regression anchor: repro through the serde(flatten)
// delegation site (`OpenAIModelProviderConfig { #[serde(flatten)] base }`).
// A bad-typed value on a flattened base field of a live alias must
// propagate the value error, not degrade into "Unknown property".

#[test]
async fn set_prop_flatten_alias_leaf_rejects_invalid_value_not_unknown_property() {
    let mut config = Config::default();
    config
        .create_map_key("providers.models.openai", "k8")
        .expect("typed family slot accepts a new alias");

    // Happy path first: valid value round-trips.
    config
        .set_prop("providers.models.openai.k8.temperature", "0.5")
        .unwrap();
    assert_eq!(
        config
            .get_prop("providers.models.openai.k8.temperature")
            .unwrap(),
        "0.5"
    );

    // The issue's repro: non-numeric value on the same confirmed path.
    let err = config
        .set_prop("providers.models.openai.k8.temperature", "abc")
        .unwrap_err()
        .to_string();
    assert_value_error(&err);
}

#[test]
async fn set_prop_flatten_alias_unknown_leaf_still_unknown_property() {
    let mut config = Config::default();
    config
        .create_map_key("providers.models.openai", "k8")
        .expect("typed family slot accepts a new alias");
    let err = config
        .set_prop("providers.models.openai.k8.nonexistent_field", "x")
        .unwrap_err()
        .to_string();
    assert_unknown_property(&err);
}

#[test]
async fn set_prop_flatten_own_field_still_resolves_after_base_unknown_property() {
    // GeminiModelProviderConfig has BOTH a flattened base and its own
    // direct fields (oauth_project / oauth_client_id). Setting an
    // own-field goes through the flatten site first, which returns
    // "Unknown property" — that must keep falling through so the own
    // field still resolves (the no-over-propagation guarantee).
    let mut config = Config::default();
    config
        .create_map_key("providers.models.gemini", "k8")
        .expect("typed family slot accepts a new alias");
    config
        .set_prop("providers.models.gemini.k8.oauth_project", "myres")
        .unwrap();
    assert_eq!(
        config
            .providers
            .models
            .gemini
            .get("k8")
            .expect("alias created above")
            .oauth_project
            .as_deref(),
        Some("myres")
    );
}

#[test]
async fn set_prop_flatten_base_field_via_gemini_alias_propagates_value_error() {
    let mut config = Config::default();
    config
        .create_map_key("providers.models.gemini", "k8")
        .expect("typed family slot accepts a new alias");
    let err = config
        .set_prop("providers.models.gemini.k8.temperature", "abc")
        .unwrap_err()
        .to_string();
    assert_value_error(&err);
}

// ── regression: a genuine value error whose own message starts with
// "Unknown property" must still propagate, not be reclassified as the
// generated fall-through marker and swallowed as a retry.
// `is_unknown_property_error` used to match on that prefix alone, so a
// validator error crafted to begin the same way would have been
// misread as "not mine" at the `Option<T>` delegation gate, silently
// discarded, and reported as an unknown-leaf error instead of the real
// value problem it is. (The struct-level TOML round-trip that runs the
// field's custom deserializer wraps the raw message with its own
// "TOML parse error at line ..." position preamble, so the *final*
// propagated error contains rather than starts with the crafted text —
// the point under test is that the crafted sentence survives at all
// instead of being replaced by the generic `Unknown property '<name>'`
// fallback a misclassification would produce.)

fn reject_poison_string_value<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value == "poison" {
        return Err(serde::de::Error::custom(
            "Unknown property value rejected by a custom field validator",
        ));
    }
    Ok(value)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "unk_collision"]
struct UnknownPropertyCollisionInner {
    #[serde(default, deserialize_with = "reject_poison_string_value")]
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "unk_collision_outer"]
struct UnknownPropertyCollisionOuter {
    #[serde(default)]
    #[nested]
    pub inner: Option<UnknownPropertyCollisionInner>,
}

#[test]
async fn set_prop_option_nested_value_error_starting_with_unknown_property_prefix_still_propagates()
{
    let mut outer = UnknownPropertyCollisionOuter {
        inner: Some(UnknownPropertyCollisionInner::default()),
    };
    let err = outer
        .set_prop("unk_collision.label", "poison")
        .unwrap_err()
        .to_string();
    // The specific validator message must survive — a
    // misclassification would instead produce the generic
    // `Unknown property '<name>'` fallback, which doesn't contain this
    // text.
    assert!(
        err.contains("Unknown property value rejected by a custom field validator"),
        "a genuine value error must propagate, not be masked as an unknown property: {err}"
    );
    // Sanity: confirm this isn't the generic fallback shape (which
    // would also technically satisfy a looser check).
    assert!(
        !err.contains("Unknown property 'unk_collision.label'"),
        "must not have degraded into the generic unknown-property fallback: {err}"
    );
}

#[test]
async fn ensure_map_key_for_path_ignores_resource_keyed_rate_sections() {
    let mut config = Config::default();
    config.ensure_map_key_for_path("cost.rates.providers.models.openai.gpt-4.1.input_per_mtok");
    // The return value proves nothing here: it is `true` only for the
    // reserved-agent refusal, so it is already `false` without the
    // `resource_key` filter. The emptiness check is the regression signal.
    assert!(
        config.cost.rates.providers.models.openai.is_empty(),
        "a leaf write must not auto-create a resource-keyed rate row"
    );
}

#[test]
async fn ensure_map_key_for_path_leaves_dotted_resource_ids_alone() {
    let mut config = Config::default();
    config
        .create_map_key("cost.rates.providers.models.openai", "gpt-4.1")
        .expect("resource-keyed sections accept dotted model ids");
    let path = "cost.rates.providers.models.openai.gpt-4.1.input_per_mtok";
    config.ensure_map_key_for_path(path);
    assert_eq!(
        config
            .get_map_keys("cost.rates.providers.models.openai")
            .expect("known section"),
        vec!["gpt-4.1".to_string()],
        "the first-dot split must not plant a phantom `gpt-4` sibling"
    );
    assert!(
        config.set_prop(path, "1.5").is_ok(),
        "editing a real dotted resource id must still work"
    );
}

#[test]
async fn ensure_map_key_for_path_ignores_cost_rate_tools() {
    let mut config = Config::default();
    config.ensure_map_key_for_path("cost.rates.tools.web_search.per_call");
    assert!(
        config.cost.rates.tools.is_empty(),
        "`cost.rates.tools` is resource-keyed by the tool's registered name"
    );
}

#[test]
async fn create_map_key_rejects_unknown_section() {
    let mut config = Config::default();
    let err = config
        .create_map_key("not.a.real.section", "anything")
        .expect_err("unknown section path should error");
    assert!(err.contains("not.a.real.section"));
}

#[test]
async fn provider_slot_names_match_struct_fields() {
    // TtsProviders/TranscriptionProviders::slot_names are inline lists
    // (their slot macros carry a rate-type param); pin them against the
    // actual serialized field names so adding a family without updating
    // slot_names fails here.
    let tts = toml::Value::try_from(crate::providers::TtsProviders {
        openai: std::iter::once(("a".to_string(), Default::default())).collect(),
        elevenlabs: std::iter::once(("a".to_string(), Default::default())).collect(),
        google: std::iter::once(("a".to_string(), Default::default())).collect(),
        edge: std::iter::once(("a".to_string(), Default::default())).collect(),
        piper: std::iter::once(("a".to_string(), Default::default())).collect(),
    })
    .unwrap();
    let mut tts_fields: Vec<&str> = tts.as_table().unwrap().keys().map(String::as_str).collect();
    tts_fields.sort_unstable();
    let mut tts_slots = crate::providers::TtsProviders::slot_names().to_vec();
    tts_slots.sort_unstable();
    assert_eq!(tts_fields, tts_slots);

    let tr = toml::Value::try_from(crate::providers::TranscriptionProviders {
        groq: std::iter::once(("a".to_string(), Default::default())).collect(),
        openai: std::iter::once(("a".to_string(), Default::default())).collect(),
        deepgram: std::iter::once(("a".to_string(), Default::default())).collect(),
        assemblyai: std::iter::once(("a".to_string(), Default::default())).collect(),
        google: std::iter::once(("a".to_string(), Default::default())).collect(),
        local_whisper: std::iter::once(("a".to_string(), Default::default())).collect(),
    })
    .unwrap();
    let mut tr_fields: Vec<&str> = tr.as_table().unwrap().keys().map(String::as_str).collect();
    tr_fields.sort_unstable();
    let mut tr_slots = crate::providers::TranscriptionProviders::slot_names().to_vec();
    tr_slots.sort_unstable();
    assert_eq!(tr_fields, tr_slots);
}

#[test]
async fn unknown_provider_families_flags_silent_serde_drop() {
    // serde ignores unknown keys under providers.models, so a typo'd
    // family parses cleanly and its aliases vanish on reload. The
    // detector must flag it; known families must pass.
    let raw = r#"
schema_version = 3

[providers.models.antropic.main]
model = "claude-sonnet-4-6"

[providers.models.openai.work]
model = "gpt-4o"
"#;
    let parsed: Config = toml::from_str(raw).expect("unknown family must not fail parse");
    assert!(
        parsed.providers.models.find("antropic", "main").is_none(),
        "precondition: serde silently drops the unknown family"
    );
    assert_eq!(
        Config::unknown_provider_families(raw),
        vec!["models.antropic".to_string()]
    );
    assert_eq!(
        Config::unknown_provider_families(
            "schema_version = 3\n[providers.tts.bogustts.x]\nenabled = true\n",
        ),
        vec!["tts.bogustts".to_string()]
    );
    assert!(Config::unknown_provider_families("not even toml {{{").is_empty());
    // Hostile shapes: scalar providers node, scalar kind node,
    // array-of-tables family. as_table() filters all of them; the
    // detector must stay silent rather than panic or false-positive.
    assert!(Config::unknown_provider_families("providers = 3\n").is_empty());
    assert!(Config::unknown_provider_families("[providers]\nmodels = 3\n").is_empty());
    assert_eq!(
        Config::unknown_provider_families("[[providers.models.weird]]\nx = 1\n"),
        vec!["models.weird".to_string()],
        "array-of-tables under an unknown family is still an unknown family"
    );
}

#[test]
async fn extra_nested_model_provider_tables_flags_dropped_provider_fields() {
    let finding = |family: &str, alias: &str, nested: &str| ExtraNestedModelProviderTable {
        family: family.to_string(),
        alias: alias.to_string(),
        nested: nested.to_string(),
    };

    // serde accepts `[providers.models.zai.default.default]` by treating
    // the first `default` as the provider alias and erasing the second
    // child table. The typed Config therefore looks like an empty
    // `zai.default` provider and `validate()` accepts it as an
    // in-progress quickstart entry. The raw-TOML detector must preserve
    // that diagnostic signal before deserialization erases the shape.
    let raw = r#"
schema_version = 3

[providers.models.zai.default.default]
model = "glm-5.1"
api_key = "sk-test"
endpoint = "global"

[risk_profiles.default]
level = "supervised"

[agents.default]
enabled = true
model_provider = "zai.default"
risk_profile = "default"
"#;
    let parsed: Config = toml::from_str(raw).expect("extra nesting must not fail parse");
    let provider = parsed
        .providers
        .models
        .find("zai", "default")
        .expect("outer alias is still present");
    assert!(
        provider.model.is_none() && provider.api_key.is_none(),
        "precondition: serde silently drops the nested model/api_key"
    );
    parsed
        .validate()
        .expect("typed validation cannot see the raw extra nesting");
    assert_eq!(
        Config::extra_nested_model_provider_tables(raw),
        vec![finding("zai", "default", "default")]
    );

    let valid = r#"
schema_version = 3

[providers.models.zai.default]
model = "glm-5.1"
api_key = "sk-test"
endpoint = "global"
"#;
    assert!(
        Config::extra_nested_model_provider_tables(valid).is_empty(),
        "valid V3 alias tables must not be flagged"
    );

    let valid_table_fields = r#"
schema_version = 3

[providers.models.openai.default]
model = "gpt-4o"

[providers.models.openai.default.extra_headers]
model = "header-value"
api_key = "header-value"

[providers.models.openai.default.provider_extra]
model = "router-model"
api_key = "provider-extra-value"

[providers.models.openai.default.chat_template_kwargs]
model = "template-model"
api_key = "template-value"

[providers.models.openai.default.pricing]
model = 1.0
api_key = 2.0
"#;
    assert!(
        Config::extra_nested_model_provider_tables(valid_table_fields).is_empty(),
        "valid table-valued provider fields must not be flagged even when child keys collide"
    );

    let valid_pricing = r#"
schema_version = 3

[providers.models.openai.default]
model = "gpt-4o"
pricing = { "gpt-4o.input" = 5.0, "gpt-4o.output" = 15.0 }
"#;
    assert!(
        Config::extra_nested_model_provider_tables(valid_pricing).is_empty(),
        "valid nested provider fields such as pricing must not be flagged"
    );

    let typed_provider_fields = r#"
schema_version = 3

[providers.models.gemini.default.default]
oauth_project = "example-project"

[providers.models.ollama.local.default]
model = "llama3.1"
"#;
    assert_eq!(
        Config::extra_nested_model_provider_tables(typed_provider_fields),
        vec![
            finding("gemini", "default", "default"),
            finding("ollama", "local", "default")
        ],
        "typed provider fields must still be flagged when extra-nested"
    );

    let dotted_alias = r#"
schema_version = 3

[providers.models.openai."prod.v2".default]
model = "gpt-4o"
"#;
    assert_eq!(
        Config::extra_nested_model_provider_tables(dotted_alias),
        vec![finding("openai", "prod.v2", "default")],
        "aliases containing dots must stay intact in diagnostics"
    );

    assert!(
        Config::extra_nested_model_provider_tables(
            "schema_version = 3\n[providers.models.zia.default.default]\nmodel = \"x\"\n",
        )
        .is_empty(),
        "unknown families are handled by unknown_provider_families"
    );
    assert!(Config::extra_nested_model_provider_tables("not toml {{{").is_empty());
    assert!(Config::extra_nested_model_provider_tables("providers = 3\n").is_empty());
}

#[test]
async fn map_key_create_survives_incremental_save() {
    // Repro for the zerocode "providers vanish after restart" report:
    // the RPC config/map-key-create path is create_map_key + mark_dirty
    // + save_dirty. The new alias must reach config.toml, otherwise it
    // exists only in-memory and a daemon restart silently drops it
    // (and any agents.*.model_provider referencing it dangles).
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Seed a non-empty on-disk file so the incremental path runs, not
    // the new-file fallback to full save().
    std::fs::write(
        &config_path,
        "schema_version = 9\n\n[observability]\nbackend = \"none\"\n",
    )
    .unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    let created = config
        .create_map_key("providers.models.openai", "myalias")
        .expect("typed family slot accepts a new alias");
    assert!(created);
    assert_eq!(
        config
            .providers
            .models
            .find("openai", "myalias")
            .and_then(|e| e.wire_api),
        Some(WireApi::Responses),
        "new OpenAI provider slots default to wire_api = responses"
    );
    config.mark_dirty("providers.models.openai.myalias");
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    let reloaded: Config = toml::from_str(&written)
        .unwrap_or_else(|e| panic!("rewritten config must reparse: {e}\n---\n{written}"));
    assert!(
        reloaded
            .providers
            .models
            .find("openai", "myalias")
            .is_some(),
        "created alias must survive save_dirty + reload; got:\n{written}"
    );
    assert_eq!(
        reloaded
            .providers
            .models
            .find("openai", "myalias")
            .and_then(|e| e.wire_api),
        Some(WireApi::Responses),
        "default wire_api must survive save_dirty + reload; got:\n{written}"
    );
}

#[test]
async fn telegram_alias_create_survives_incremental_save() {
    // Regression test: create_map_key seeds TelegramConfig::default()
    // (bot_token = ""), save_dirty's prune_empty_leaves then strips the
    // empty bot_token from the written TOML, and on reload the alias
    // must still deserialize (bot_token now has #[serde(default)])
    // instead of being silently salvage-dropped.
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Seed a non-empty on-disk file so the incremental path runs, not
    // the new-file fallback to full save().
    std::fs::write(
        &config_path,
        "schema_version = 9\n\n[observability]\nbackend = \"none\"\n",
    )
    .unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    let created = config
        .create_map_key("channels.telegram", "myalias")
        .expect("map-keyed section accepts a new alias");
    assert!(created);
    config.mark_dirty("channels.telegram.myalias");
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    let reloaded: Config = toml::from_str(&written)
        .unwrap_or_else(|e| panic!("rewritten config must reparse: {e}\n---\n{written}"));
    assert!(
        reloaded.channels.telegram.contains_key("myalias"),
        "created telegram alias must survive save_dirty + reload; got:\n{written}"
    );
}

#[test]
async fn discord_alias_create_survives_incremental_save() {
    // Discord twin of telegram_alias_create_survives_incremental_save:
    // DiscordConfig.bot_token has the same serde default.
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Seed a non-empty on-disk file so the incremental path runs, not
    // the new-file fallback to full save().
    std::fs::write(
        &config_path,
        "schema_version = 9\n\n[observability]\nbackend = \"none\"\n",
    )
    .unwrap();

    let mut config = Config {
        config_path: config_path.clone(),
        ..Default::default()
    };
    let created = config
        .create_map_key("channels.discord", "myalias")
        .expect("map-keyed section accepts a new alias");
    assert!(created);
    config.mark_dirty("channels.discord.myalias");
    config.save_dirty().await.unwrap();

    let written = std::fs::read_to_string(&config_path).unwrap();
    let reloaded: Config = toml::from_str(&written)
        .unwrap_or_else(|e| panic!("rewritten config must reparse: {e}\n---\n{written}"));
    assert!(
        reloaded.channels.discord.contains_key("myalias"),
        "created discord alias must survive save_dirty + reload; got:\n{written}"
    );
}

#[test]
async fn init_defaults_instantiates_none_sections() {
    let mut config = Config::default();
    assert!(config.channels.telegram.is_empty());

    // Channels are HashMaps — init_defaults cannot insert a default key
    // (there is no meaningful default alias). Callers use create_map_key.
    config
        .create_map_key("channels.telegram", "default")
        .expect("create_map_key should insert a default telegram entry");
    assert!(
        config.channels.telegram.contains_key("default"),
        "create_map_key must add the 'default' alias"
    );

    // init_defaults on an already-populated map section is a no-op.
    let initialized = config.init_defaults(Some("channels.telegram"));
    assert!(
        !initialized.contains(&"channels.telegram"),
        "init_defaults should not report channels.telegram when entry already exists"
    );
}

#[test]
async fn init_defaults_then_set_prop_round_trips_vec_string() {
    // Regression for Channels picker → form → save:
    // 1. create_map_key inserts channels.telegram["default"] = TelegramConfig::default()
    // 2. set_prop on channels.telegram.default.excluded_tools must accept a JSON-array
    //    string (the shape coerce_for_set_prop emits for Vec<String>).
    // 3. get_prop reads it back.
    let mut config = Config::default();
    config
        .create_map_key("channels.telegram", "default")
        .expect("create_map_key should insert a default telegram entry");
    assert!(config.channels.telegram.contains_key("default"));

    // prop_fields must surface the kebab path so the form can render it.
    let has_field = config
        .prop_fields()
        .iter()
        .any(|f| f.name == "channels.telegram.default.excluded_tools");
    assert!(
        has_field,
        "channels.telegram.default.excluded_tools must appear in prop_fields after init"
    );

    // set_prop with the JSON-array string the gateway PATCH path produces.
    config
        .set_prop(
            "channels.telegram.default.excluded_tools",
            r#"["alice","bob"]"#,
        )
        .expect("set_prop should accept JSON-array string for Vec<String>");
    assert_eq!(
        config
            .channels
            .telegram
            .get("default")
            .unwrap()
            .excluded_tools,
        vec!["alice".to_string(), "bob".to_string()],
    );
}

#[test]
async fn mcp_servers_addable_via_create_map_key_and_per_entry_props() {
    // `mcp.servers` is a `Vec<McpServerConfig>` with `#[nested]`, so the
    // `Configurable` derive surfaces it as a List section (not an
    // ObjectArray prop) — operators add servers via
    // `POST /api/config/map-key?path=mcp.servers&key=<name>` and edit
    // each server's fields via per-prop GET/PUT.
    //
    // This replaces the prior model where the entire Vec round-tripped
    // through set_prop("mcp.servers", "<json-array>"). The List model
    // matches the rest of the schema (`providers.models`, `agents`,
    // etc.) and gives the dashboard a per-field editor instead of a
    // monolithic JSON blob.
    let mut config = Config::default();

    // The List section is discoverable.
    let sections = Config::map_key_sections();
    assert!(
        sections
            .iter()
            .any(|s| s.path == "mcp.servers" && s.kind == crate::traits::MapKeyKind::List),
        "mcp.servers should surface as a List section in map_key_sections()"
    );

    // create_map_key inserts a default-valued entry and seeds its
    // `name` field from the supplied key.
    config
        .create_map_key("mcp.servers", "fs")
        .expect("mcp.servers should accept new list entries via create_map_key");
    assert_eq!(config.mcp.servers.len(), 1);
    assert_eq!(config.mcp.servers[0].name, "fs");

    // Per-entry fields are mutated via standard set_prop on the inner
    // path; routing goes through the `#[natural_key = "name"]` arm
    // on `McpConfig::servers` (see `route_vec_path` and the
    // `Configurable` derive's natural-key arm for the wiring).
    config
        .set_prop("mcp.servers.fs.command", "/usr/bin/mcp-fs")
        .expect("set_prop on mcp.servers.fs.command should route through natural-key arm");
    assert_eq!(config.mcp.servers[0].command, "/usr/bin/mcp-fs");

    // Round-trip via get_prop.
    let got = config
        .get_prop("mcp.servers.fs.command")
        .expect("get_prop on mcp.servers.fs.command should resolve through the natural-key arm");
    assert_eq!(got, "/usr/bin/mcp-fs");

    // Enum-typed fields (transport) parse from their wire form.
    config
        .set_prop("mcp.servers.fs.transport", "http")
        .expect("transport should accept its enum variants as strings");
    assert_eq!(
        config.mcp.servers[0].transport,
        crate::schema::McpTransport::Http
    );

    // The natural-key field itself is read-only via set_prop — the
    // routing arm returns an explicit error pointing at
    // config_map_key_rename rather than mutating `name` in place
    // (which would silently re-key the entry and strand any
    // in-flight references to the old key).
    let err = config
        .set_prop("mcp.servers.fs.name", "filesystem")
        .expect_err("set_prop on the natural-key field must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("natural key")
            && msg.contains("read-only")
            && msg.contains("config_map_key_rename"),
        "unexpected error message for read-only natural-key set: {msg}"
    );

    // Rename via the dedicated path. The element's `name` field
    // changes in place; subsequent prop access uses the new key.
    let renamed = config
        .rename_map_key("mcp.servers", "fs", "filesystem")
        .expect("rename should succeed when the new key is free");
    assert!(renamed, "rename_map_key should report Ok(true) on success");
    assert_eq!(config.mcp.servers[0].name, "filesystem");
    assert_eq!(
        config.get_prop("mcp.servers.filesystem.command").unwrap(),
        "/usr/bin/mcp-fs"
    );

    // The old key no longer resolves — confirming the rename was
    // not just an alias add.
    assert!(
        config.get_prop("mcp.servers.fs.command").is_err(),
        "old natural key should stop resolving after rename"
    );

    // prop_fields enumerates the per-element fields under the
    // current natural key, and filters out the natural-key field
    // itself (no editable `name` row in the TUI).
    let paths: Vec<String> = config
        .prop_fields()
        .into_iter()
        .map(|f| f.name)
        .filter(|n| n.starts_with("mcp.servers."))
        .collect();
    assert!(
        paths.iter().any(|n| n == "mcp.servers.filesystem.command"),
        "prop_fields should surface per-element child props; got: {paths:?}"
    );
    assert!(
        !paths.iter().any(|n| n == "mcp.servers.filesystem.name"),
        "prop_fields must hide the natural-key field to keep it read-only; got: {paths:?}"
    );

    // delete_map_key by natural key removes the matching element.
    let deleted = config
        .delete_map_key("mcp.servers", "filesystem")
        .expect("delete by natural key should resolve");
    assert!(deleted);
    assert!(config.mcp.servers.is_empty());
}

#[test]
async fn mcp_servers_create_map_key_is_idempotent_on_existing_natural_key() {
    // Regression for the per-field editor contract: the rest of the
    // natural-key surface (`get_prop` / `set_prop` / `rename_map_key`)
    // treats duplicate natural keys as `VecRoute::Ambiguous` and
    // refuses to mutate (see `mcp_servers_routing_is_ambiguous_on_
    // duplicate_names`). If `create_map_key` always appended, a UI
    // retry — or any caller that re-issued the same add after an
    // uncertain RPC response — would drop `mcp.servers` into that
    // invalid state and leave `mcp.servers.<name>.command` no longer
    // routing until the operator hand-repaired the duplicate.
    //
    // The contract is the same as the `HashMap<String, T>` arm:
    // re-adding an existing key is `Ok(false)` (idempotent no-op),
    // not "append a second element that happens to share the key".
    let mut config = Config::default();

    let first = config
        .create_map_key("mcp.servers", "fs")
        .expect("first add should succeed");
    assert!(first, "first add should report created=true");
    assert_eq!(config.mcp.servers.len(), 1);
    assert_eq!(config.mcp.servers[0].name, "fs");

    // Seed an inner field so we can prove the existing entry's
    // state is preserved across the no-op second add (rather than,
    // say, the second call clobbering it with a default).
    config
        .set_prop("mcp.servers.fs.command", "/usr/bin/mcp-fs")
        .expect("set_prop on the freshly-added entry should route");

    // Repeat add for the same natural key. Must report Ok(false)
    // and must not push a second element.
    let second = config
        .create_map_key("mcp.servers", "fs")
        .expect("repeat add for an existing natural key must not error");
    assert!(
        !second,
        "repeat add for an existing natural key should report created=false (idempotent)"
    );
    assert_eq!(
        config.mcp.servers.len(),
        1,
        "repeat add must not append a duplicate; got {} entries",
        config.mcp.servers.len()
    );
    assert_eq!(config.mcp.servers[0].name, "fs");

    // The natural-key surface still routes — the duplicate was
    // never created, so `set_prop` / `get_prop` are not in the
    // `VecRoute::Ambiguous` state. The previously-set command
    // round-trips and a new edit lands on the original entry.
    assert_eq!(
        config.get_prop("mcp.servers.fs.command").unwrap(),
        "/usr/bin/mcp-fs",
        "existing entry's state must survive the no-op second add"
    );
    config
        .set_prop("mcp.servers.fs.command", "/usr/local/bin/mcp-fs")
        .expect("set_prop must keep routing after the repeat add");
    assert_eq!(config.mcp.servers[0].command, "/usr/local/bin/mcp-fs");

    // A genuinely new key is still added: idempotency is per-key,
    // not "first add wins everything".
    let third = config
        .create_map_key("mcp.servers", "github")
        .expect("a distinct natural key should still be addable");
    assert!(third, "distinct-key add should report created=true");
    assert_eq!(config.mcp.servers.len(), 2);
}

#[test]
async fn mcp_servers_routing_is_ambiguous_on_duplicate_names() {
    // `validate_mcp_config` rejects duplicate `name` at save time,
    // but until the operator repairs the config the in-flight
    // routing must refuse to silently mutate one of the duplicates.
    // This is the schema-side anchor for that contract; the helper
    // function's behaviour is unit-tested directly in
    // `helpers::tests::route_vec_path_reports_ambiguous_duplicates`.
    let mut config = Config::default();
    config.mcp.servers.push(McpServerConfig {
        name: "dupe".into(),
        transport: McpTransport::Stdio,
        command: "/a".into(),
        ..Default::default()
    });
    config.mcp.servers.push(McpServerConfig {
        name: "dupe".into(),
        transport: McpTransport::Stdio,
        command: "/b".into(),
        ..Default::default()
    });

    let set_err = config
        .set_prop("mcp.servers.dupe.command", "/c")
        .expect_err("set_prop on a duplicated natural key must refuse");
    assert!(
        set_err.to_string().contains("ambiguous"),
        "expected ambiguity error, got: {set_err}"
    );

    let get_err = config
        .get_prop("mcp.servers.dupe.command")
        .expect_err("get_prop on a duplicated natural key must refuse");
    assert!(
        get_err.to_string().contains("ambiguous"),
        "expected ambiguity error, got: {get_err}"
    );

    // Neither side mutated the underlying state.
    assert_eq!(config.mcp.servers[0].command, "/a");
    assert_eq!(config.mcp.servers[1].command, "/b");

    // rename_map_key likewise refuses, with an actionable message.
    let rename_err = config
        .rename_map_key("mcp.servers", "dupe", "ok")
        .expect_err("rename of a duplicated natural key must refuse");
    assert!(
        rename_err.contains("ambiguous"),
        "expected ambiguity error from rename, got: {rename_err}"
    );
}

#[test]
async fn mcp_servers_rename_refuses_when_new_key_is_taken() {
    let mut config = Config::default();
    config.create_map_key("mcp.servers", "fs").unwrap();
    config.create_map_key("mcp.servers", "github").unwrap();

    let err = config
        .rename_map_key("mcp.servers", "fs", "github")
        .expect_err("rename should refuse when the target key already exists");
    assert!(
        err.contains("already exists"),
        "expected target-collision error, got: {err}"
    );

    // State untouched.
    assert_eq!(config.mcp.servers.len(), 2);
    assert_eq!(config.mcp.servers[0].name, "fs");
    assert_eq!(config.mcp.servers[1].name, "github");
}

#[test]
async fn mcp_servers_get_map_keys_lists_natural_keys_in_insertion_order() {
    let mut config = Config::default();
    config.create_map_key("mcp.servers", "a").unwrap();
    config.create_map_key("mcp.servers", "b").unwrap();
    config.create_map_key("mcp.servers", "c").unwrap();

    let keys = config
        .get_map_keys("mcp.servers")
        .expect("mcp.servers must surface its natural keys via get_map_keys");
    assert_eq!(
        keys,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
}

#[test]
async fn init_defaults_skips_already_set() {
    let mut config = Config::default();
    config
        .channels
        .telegram
        .insert("default".to_string(), test_telegram_config());

    let initialized = config.init_defaults(Some("channels.telegram"));
    // Already set — should not re-initialize
    assert!(!initialized.contains(&"channels.telegram"));
    // Original value preserved
    assert_eq!(
        config
            .channels
            .telegram
            .get("default")
            .unwrap()
            .api_base_url,
        "https://m.org"
    );
}

#[test]
async fn nested_get_set_prop_traverses_config_tree() {
    let mut config = Config::default();
    config
        .channels
        .telegram
        .insert("default".to_string(), test_telegram_config());

    // get_prop traverses Config → ChannelsConfig → channels.telegram["default"] → TelegramConfig
    assert_eq!(
        config
            .get_prop("channels.telegram.default.api_base_url")
            .unwrap(),
        "https://m.org"
    );

    // set_prop traverses the same path
    config
        .set_prop("channels.telegram.default.api_base_url", "https://new.org")
        .unwrap();
    assert_eq!(
        config
            .channels
            .telegram
            .get("default")
            .unwrap()
            .api_base_url,
        "https://new.org"
    );
}

#[test]
async fn hashmap_nested_encrypt_decrypt_traverses_values() {
    let dir = TempDir::new().unwrap();
    let store = crate::secrets::SecretStore::new(dir.path(), true);

    let mut config = Config::default();
    config.providers.models.openrouter.insert(
        "test".into(),
        crate::schema::OpenRouterModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("secret-key".into()),
                ..Default::default()
            },
        },
    );

    config.encrypt_secrets(&store).unwrap();
    let encrypted_key = config
        .providers
        .models
        .find("openrouter", "test")
        .expect("entry exists")
        .api_key
        .as_ref()
        .unwrap();
    assert!(crate::secrets::SecretStore::is_encrypted(encrypted_key));

    config.decrypt_secrets(&store).unwrap();
    assert_eq!(
        config
            .providers
            .models
            .find("openrouter", "test")
            .expect("entry exists")
            .api_key
            .as_deref(),
        Some("secret-key")
    );
}

#[test]
async fn vec_secret_encrypt_decrypt_traverses_elements() {
    let dir = TempDir::new().unwrap();
    let store = crate::secrets::SecretStore::new(dir.path(), true);

    let mut config = Config::default();
    config.gateway.paired_tokens = vec!["token-a".into(), "token-b".into()];

    config.encrypt_secrets(&store).unwrap();
    for token in &config.gateway.paired_tokens {
        assert!(crate::secrets::SecretStore::is_encrypted(token));
    }

    config.decrypt_secrets(&store).unwrap();
    assert_eq!(config.gateway.paired_tokens, vec!["token-a", "token-b"]);
}

/// Walk every property on a default Config: get_prop must succeed,
/// and set_prop must round-trip for non-secret, non-enum scalar fields.
#[test]
async fn every_prop_is_gettable_and_settable() {
    let mut config = Config::default();
    // Initialize all Option<T> sections so their fields are reachable
    config.init_defaults(None);

    let fields = config.prop_fields();
    assert!(
        fields.len() > 50,
        "Expected 50+ props, got {} — macro may be skipping fields",
        fields.len()
    );

    for field in &fields {
        // get_prop must not panic or error
        let get_result = config.get_prop(&field.name);
        assert!(
            get_result.is_ok(),
            "get_prop failed for '{}': {}",
            field.name,
            get_result.unwrap_err()
        );

        // set_prop: round-trip the display value back through set_prop.
        // Skip secrets (masked), enums (need valid variant), and <unset> Options.
        if field.is_secret || field.is_enum() || field.display_value == crate::traits::UNSET_DISPLAY
        {
            continue;
        }

        let set_result = config.set_prop(&field.name, &field.display_value);
        assert!(
            set_result.is_ok(),
            "set_prop failed for '{}' with value '{}': {}",
            field.name,
            field.display_value,
            set_result.unwrap_err()
        );

        // Value should survive the round-trip
        let after = config.get_prop(&field.name).unwrap();
        assert_eq!(
            after, field.display_value,
            "round-trip mismatch for '{}': set '{}', got '{}'",
            field.name, field.display_value, after
        );
    }
}

/// Audit gate: every path emitted by `prop_fields()` must round-trip
/// through `get_prop`. The CLI (`zeroclaw config get/set`), the TUI
/// Quickstart prompts (`prompt_field`), the gateway list endpoint
/// (`/api/config/list`), and the dashboard form all derive from
/// `prop_fields()`; if a path appears here but `get_prop` rejects
/// it, that field is unreachable on every surface.
///
/// `init_defaults(None)` populates Option-shaped subsections (memory
/// backend specifics, tunnel provider details, etc.) so the walk
/// also exercises fields that only materialize once a backend is
/// chosen.
#[test]
async fn every_prop_field_path_is_reachable_via_get_prop() {
    let mut config = Config::default();
    config.init_defaults(None);
    for field in config.prop_fields() {
        let result = config.get_prop(&field.name);
        assert!(
            result.is_ok(),
            "get_prop('{}') failed: {} \u{2014} prop_fields() advertises a path \
             that the CLI / gateway / TUI all expect to be readable. \
             Either the macro emits the path but routing is missing, \
             or the field shouldn't be in prop_fields().",
            field.name,
            result.unwrap_err()
        );
    }
}

/// The dashboard's `/config/channels` global-settings tab filters the
/// canonical `prop_fields()` list down to direct `[channels]` fields.
/// Keep root settings such as `show_tool_calls` discoverable there without
/// mixing per-alias fields into the same editor.
#[test]
async fn channels_root_settings_stay_on_direct_prop_surface() {
    let mut config = Config::default();
    config.init_defaults(None);
    config
        .channels
        .telegram
        .insert("default".into(), TelegramConfig::default());

    let paths: Vec<_> = config
        .prop_fields()
        .into_iter()
        .map(|field| field.name)
        .collect();
    let direct_channel_paths: Vec<_> = paths
        .iter()
        .filter_map(|path| {
            path.strip_prefix("channels.")
                .filter(|rest| !rest.contains('.'))
                .map(|_| path.as_str())
        })
        .collect();

    assert!(
        direct_channel_paths.contains(&"channels.show_tool_calls"),
        "root [channels] settings should include show_tool_calls: {direct_channel_paths:?}"
    );
    assert!(
        direct_channel_paths.contains(&"channels.ack_reactions"),
        "root [channels] settings should include other global channel controls"
    );
    assert!(
        paths
            .iter()
            .any(|path| path == "channels.telegram.default.enabled"),
        "fixture should include a nested channel alias field"
    );
    assert!(
        !direct_channel_paths.contains(&"channels.telegram.default.enabled"),
        "global channel settings must not include per-alias fields"
    );
}

/// Audit gate for RFC Phase 0: any credential-shaped property path
/// that reaches the CLI/gateway/TUI property surface must have an explicit
/// classification. This catches future config additions whose names imply
/// credential handling before they silently land without a security call.
#[test]
async fn credential_shaped_prop_fields_have_explicit_classification() {
    let mut config = Config::default();
    config.init_defaults(None);
    config
        .providers
        .models
        .anthropic
        .insert("default".into(), AnthropicModelProviderConfig::default());
    config
        .providers
        .tts
        .openai
        .insert("default".into(), OpenAITtsProviderConfig::default());
    config.providers.transcription.openai.insert(
        "default".into(),
        OpenAiTranscriptionProviderConfig::default(),
    );
    config.providers.transcription.local_whisper.insert(
        "default".into(),
        LocalWhisperTranscriptionProviderConfig::default(),
    );
    config
        .channels
        .telegram
        .insert("default".into(), TelegramConfig::default());

    let fields = config.prop_fields();
    let missing: Vec<_> = fields
        .iter()
        .filter(|field| credential_shaped_prop_path(&field.name))
        .filter(|field| field.credential_class.is_none())
        .map(|field| field.name.clone())
        .collect();

    assert!(
        missing.is_empty(),
        "credential-shaped config fields need explicit classification: {missing:?}"
    );

    let unmarked_secrets: Vec<_> = fields
        .iter()
        .filter(|field| {
            field.credential_class == Some(crate::config::CredentialSurfaceClass::EncryptedSecret)
        })
        .filter(|field| !field.is_secret && !Config::prop_is_secret(&field.name))
        .map(|field| field.name.clone())
        .collect();

    assert!(
        unmarked_secrets.is_empty(),
        "EncryptedSecret classifications must route through #[secret]: {unmarked_secrets:?}"
    );
}

#[test]
async fn prop_fields_carry_credential_classification_from_schema_fields() {
    let mut config = Config::default();
    config.init_defaults(None);
    config.providers.models.openai.insert(
        "codex".into(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                requires_openai_auth: true,
                ..ModelProviderConfig::default()
            },
        },
    );
    config
        .providers
        .tts
        .openai
        .insert("default".into(), OpenAITtsProviderConfig::default());
    config.providers.transcription.local_whisper.insert(
        "default".into(),
        LocalWhisperTranscriptionProviderConfig::default(),
    );
    config
        .channels
        .telegram
        .insert("default".into(), TelegramConfig::default());

    let fields = config.prop_fields();
    let class_for = |name: &str| {
        fields
            .iter()
            .find(|field| field.name == name)
            .and_then(|field| field.credential_class)
    };

    assert_eq!(
        class_for("providers.models.openai.codex.requires_openai_auth"),
        Some(crate::config::CredentialSurfaceClass::ExternalAuthStore)
    );
    assert_eq!(
        class_for("providers.tts.openai.default.api_key"),
        Some(crate::config::CredentialSurfaceClass::EncryptedSecret)
    );
    assert_eq!(
        class_for("providers.transcription.local_whisper.default.bearer_token"),
        Some(crate::config::CredentialSurfaceClass::EncryptedSecret)
    );
    assert_eq!(
        class_for("channels.telegram.default.bot_token"),
        Some(crate::config::CredentialSurfaceClass::EncryptedSecret)
    );
    // model_routes and embedding_routes are now #[nested] Vec fields —
    // they are surfaced via map_key_sections(), not as flat prop_fields.
    // After adding a route entry, its api_key sub-field appears in
    // prop_fields with EncryptedSecret classification (from #[secret]).
    config.model_routes.push(ModelRouteConfig {
        hint: "reasoning".into(),
        model_provider: "openai.default".into(),
        model: "gpt-4".into(),
        api_key: None,
    });
    config.embedding_routes.push(EmbeddingRouteConfig {
        hint: "semantic".into(),
        model_provider: "openai.embeddings".into(),
        model: "text-embedding-3-small".into(),
        dimensions: None,
        api_key: None,
    });
    let nested_fields = config.prop_fields();
    let nested_class_for = |name: &str| {
        nested_fields
            .iter()
            .find(|field| field.name == name)
            .and_then(|field| field.credential_class)
    };
    assert_eq!(
        nested_class_for("model_routes.reasoning.api_key"),
        Some(crate::config::CredentialSurfaceClass::EncryptedSecret)
    );
    assert_eq!(
        nested_class_for("embedding_routes.semantic.api_key"),
        Some(crate::config::CredentialSurfaceClass::EncryptedSecret)
    );
    assert!(Config::prop_is_secret(
        "providers.tts.openai.default.api_key"
    ));
    assert!(Config::prop_is_secret(
        "providers.transcription.local_whisper.default.bearer_token"
    ));
    assert!(Config::prop_is_secret(
        "channels.telegram.default.bot_token"
    ));
}

fn credential_shaped_prop_path(path: &str) -> bool {
    path.split('.').any(|part| {
        let normalized = part.replace('_', "-");
        let has_term = |needle| normalized.split('-').any(|term| term == needle);
        normalized.contains("api-key")
            || normalized.contains("api-token")
            || normalized.contains("auth-file")
            || normalized.contains("auth-header")
            || normalized.contains("auth-token")
            || normalized.contains("bearer-token")
            || normalized.contains("bot-token")
            || normalized.contains("access-token")
            || normalized.contains("refresh-token")
            || normalized.contains("verification-token")
            || normalized.contains("paired-tokens")
            || part == "token"
            || has_term("credential")
            || has_term("env")
            || has_term("header")
            || has_term("headers")
            || has_term("password")
            || has_term("secret")
    })
}

#[test]
async fn object_array_prop_display_redacts_nested_secret_fields() {
    let fixture = ObjectArraySecretFixture {
        entries: vec![
            ObjectArraySecretEntry {
                name: "primary".to_string(),
                token: Some("nested-token-credential".to_string()),
                headers: HashMap::from([
                    (
                        "Authorization".to_string(),
                        "Bearer nested-header-credential".to_string(),
                    ),
                    ("X-Tenant".to_string(), "tenant-credential".to_string()),
                ]),
            },
            ObjectArraySecretEntry {
                name: "unset-secret".to_string(),
                token: None,
                headers: HashMap::new(),
            },
        ],
    };

    let display_value = fixture
        .prop_fields()
        .into_iter()
        .find(|field| field.name == "test.object_array.entries")
        .expect("object-array field should be surfaced")
        .display_value;
    let readback = fixture
        .get_prop("test.object_array.entries")
        .expect("object-array field should be readable");

    for rendered in [&display_value, &readback] {
        assert!(
            !rendered.contains("nested-token-credential"),
            "object-array display/readback must redact scalar nested secrets: {rendered}"
        );
        assert!(
            !rendered.contains("Bearer nested-header-credential"),
            "object-array display/readback must redact nested secret map values: {rendered}"
        );
        assert!(
            !rendered.contains("tenant-credential"),
            "object-array display/readback must redact every value in nested secret maps: {rendered}"
        );
        assert!(
            rendered.contains("primary"),
            "non-secret object-array fields should remain visible: {rendered}"
        );
        assert!(
            rendered.contains("unset-secret"),
            "non-secret fields on entries with unset secrets should remain visible: {rendered}"
        );
        assert!(
            rendered.contains("****"),
            "redacted object-array output should show masked placeholders: {rendered}"
        );
    }

    assert!(
        display_value.contains(r#""token":null"#),
        "JSON display should preserve unset optional secrets as null, not a populated mask: {display_value}"
    );
}

#[test]
async fn onboard_state_prop_path_uses_top_level_kebab_field_name() {
    let mut config = Config::default();

    config
        .set_prop("onboard_state.completed_sections", "agents")
        .expect("onboard state marker path should be writable");
    assert_eq!(
        config
            .get_prop("onboard_state.completed_sections")
            .expect("onboard state marker path should be readable"),
        "[\"agents\"]"
    );
}

/// `onboard_state.quickstart_completed` is the flag the Quickstart
/// flips when it lands a `BuilderSubmission`. Defaults to `false`
/// so first launches auto-open the Quickstart; round-trips through
/// `set_prop` / `get_prop` like any other top-level config field.
#[test]
async fn onboard_state_quickstart_completed_round_trips() {
    let mut config = Config::default();

    assert_eq!(
        config
            .get_prop("onboard_state.quickstart_completed")
            .expect("default quickstart-completed should be readable"),
        "false",
        "fresh configs default to quickstart-completed=false so the \
         Quickstart auto-opens on first launch",
    );

    config
        .set_prop("onboard_state.quickstart_completed", "true")
        .expect("quickstart-completed should be writable via prop path");
    assert_eq!(
        config
            .get_prop("onboard_state.quickstart_completed")
            .expect("quickstart-completed should be readable after set"),
        "true"
    );
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_warns_on_retired_cost_selector_and_keeps_complexity_hints() {
    let _env_guard = env_override_lock().await;
    let home = TempDir::new().unwrap();
    let workspace = home.path().join("profile-a");
    fs::create_dir_all(&workspace).await.unwrap();
    let config_path = workspace.join("config.toml");
    let contents = r#"schema_version = 3

[providers.models.ollama.local]
model = "qwen3"
pricing = { "qwen3.input" = 0.1, "qwen3.output" = 0.2 }

[risk_profiles.shared]
[runtime_profiles.fast.auto_classify]
simple_hint = "cost-optimized"
standard_hint = "regular"
complex_hint = "cheapest"
cost_optimized_hint = "legacy-cost-selector"

[agents.assistant]
model_provider = "ollama.local"
risk_profile = "shared"
runtime_profile = "fast"
"#;
    fs::write(&config_path, contents).await.unwrap();
    let _home = EnvValueGuard::set("HOME", home.path());
    let _workspace = EnvValueGuard::set("ZEROCLAW_WORKSPACE", &workspace);
    let _config_dir = EnvValueGuard::remove("ZEROCLAW_CONFIG_DIR");

    let mut config = Box::pin(Config::load_or_init()).await.unwrap();
    assert!(config.degraded_sections.is_empty());
    assert!(config.degraded_security.is_empty());
    let agent = config.resolved_agent_config("assistant").unwrap();
    let hints = agent.resolved.auto_classify.unwrap();
    assert_eq!(hints.simple_hint.as_deref(), Some("cost-optimized"));
    assert_eq!(hints.standard_hint.as_deref(), Some("regular"));
    assert_eq!(hints.complex_hint.as_deref(), Some("cheapest"));
    let pricing = &config.providers.models.ollama["local"].base.pricing;
    assert_eq!(pricing.get("qwen3.input"), Some(&0.1));
    assert_eq!(pricing.get("qwen3.output"), Some(&0.2));

    let warnings = config.collect_warnings();
    let retired: Vec<_> = warnings
        .iter()
        .filter(|warning| warning.code == "cost_optimized_routing_removed")
        .collect();
    assert_eq!(retired.len(), 1);
    assert_eq!(
        retired[0].path,
        "runtime_profiles.*.auto_classify.cost_optimized_hint"
    );
    let retired_path = "runtime_profiles.fast.auto_classify.cost_optimized_hint";
    assert!(
        !config
            .prop_fields()
            .iter()
            .any(|field| field.name == retired_path)
    );
    assert!(config.set_prop(retired_path, "cheapest").is_err());
    let live_path = "runtime_profiles.fast.auto_classify.simple_hint";
    assert!(
        config
            .prop_fields()
            .iter()
            .any(|field| field.name == live_path)
    );
    assert_eq!(config.get_prop(live_path).unwrap(), "cost-optimized");
    config.set_prop(live_path, "fast").unwrap();
    assert_eq!(config.get_prop(live_path).unwrap(), "fast");
    assert!(
        serde_json::to_value(&hints)
            .unwrap()
            .get("cost_optimized_hint")
            .is_none()
    );
    assert_eq!(fs::read_to_string(config_path).await.unwrap(), contents);
}

#[cfg(feature = "schema-export")]
#[test]
async fn auto_classify_schema_keeps_only_the_three_complexity_hints() {
    use crate::scattered_types::AutoClassifyConfig;

    let schema = serde_json::to_value(schemars::schema_for!(AutoClassifyConfig)).unwrap();
    let properties = schema["properties"].as_object().unwrap();
    for key in ["simple_hint", "standard_hint", "complex_hint"] {
        assert!(properties.contains_key(key), "missing live field {key}");
    }
    assert!(!properties.contains_key("cost_optimized_hint"));
}

#[test]
async fn per_agent_nested_prop_fields_use_agent_alias_paths() {
    let mut config = Config::default();
    config
        .agents
        .insert("bob".to_string(), AliasedAgentConfig::default());
    config.runtime_profiles.insert(
        "fast".to_string(),
        crate::schema::RuntimeProfileConfig::default(),
    );

    let fields = config.prop_fields();
    assert!(
        fields
            .iter()
            .any(|field| field.name == "runtime_profiles.fast.history_pruning.enabled"),
        "history-pruning is a runtime-profile field, emitted under the profile alias"
    );
    assert!(
        !fields
            .iter()
            .any(|field| field.name.starts_with("agents.bob.history_pruning")),
        "history-pruning must no longer be settable on the agent"
    );

    config
        .set_prop("runtime_profiles.fast.history_pruning.enabled", "true")
        .expect("set_prop should accept the runtime-profile nested path");
    assert_eq!(
        config
            .get_prop("runtime_profiles.fast.history_pruning.enabled")
            .expect("get_prop should accept the runtime-profile nested path"),
        "true"
    );
}

/// Audit gate: every non-secret scalar prop round-trips through
/// `set_prop(get_prop(p))`. The CLI's `zeroclaw config set` and the
/// dashboard's PATCH op both rely on this being true so an operator
/// can read a value, edit it locally, and write it back. Vec /
/// object-array fields are skipped — they pass through serde-JSON
/// rather than scalar string parsing.
#[test]
async fn every_scalar_prop_round_trips_through_set_prop() {
    let mut config = Config::default();
    config.init_defaults(None);
    let fields = config.prop_fields();
    for field in &fields {
        if field.is_secret
            || matches!(
                field.kind,
                crate::config::PropKind::StringArray | crate::config::PropKind::ObjectArray
            )
        {
            continue;
        }
        let value = match config.get_prop(&field.name) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Sentinel for unset Option fields — no round-trip applies.
        if value == crate::traits::UNSET_DISPLAY {
            continue;
        }
        let result = config.set_prop(&field.name, &value);
        assert!(
            result.is_ok(),
            "round-trip set_prop('{}', '{}') failed: {}",
            field.name,
            value,
            result.unwrap_err()
        );
    }
}

/// Every enum field must have a working enum_variants callback, and
/// set_prop must accept each variant it advertises.
#[test]
async fn every_enum_variant_is_settable() {
    let mut config = Config::default();
    config.init_defaults(None);

    for field in config.prop_fields() {
        if !field.is_enum() {
            continue;
        }
        let get_variants = field
            .enum_variants
            .unwrap_or_else(|| panic!("enum field '{}' has no enum_variants callback", field.name));
        let variants = get_variants();
        assert!(
            !variants.is_empty(),
            "enum field '{}' returned no variants",
            field.name
        );

        for variant in &variants {
            let result = config.set_prop(&field.name, variant);
            assert!(
                result.is_ok(),
                "set_prop('{}', '{}') failed: {}",
                field.name,
                variant,
                result.unwrap_err()
            );
        }
    }
}

#[test]
async fn channel_approval_timeout_secs_defaults_to_300() {
    let discord: DiscordConfig = serde_json::from_str(r#"{"bot_token":"tok"}"#).unwrap();
    assert_eq!(discord.approval_timeout_secs, 300);
}

#[test]
async fn channel_approval_timeout_secs_explicit_override() {
    let discord: DiscordConfig =
        serde_json::from_str(r#"{"bot_token":"tok","approval_timeout_secs":60}"#).unwrap();
    assert_eq!(discord.approval_timeout_secs, 60);
}

// ── Multi-agent cross-reference validators ─────────────────────

/// Build a minimal valid Config with one agent on a configured
/// channel + risk profile + model provider. Each test mutates a
/// single field to provoke a validator.
fn multi_agent_test_config() -> Config {
    use crate::providers::ChannelRef;

    let mut config = Config::default();

    // Risk profile (mandatory for enabled agents).
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());

    // Anthropic model provider (mandatory for the agent).
    config.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig::default(),
    );

    // A configured Telegram channel the agent can reference. Just
    // having the entry in the map is enough for the dotted-alias
    // validator; we are not exercising channel-level behavior here.
    config
        .channels
        .telegram
        .insert("draft".to_string(), TelegramConfig::default());

    // Agent that targets the model provider, risk profile, and
    // channel. Default workspace is jailed.
    let agent = AliasedAgentConfig {
        channels: vec![ChannelRef::new("telegram.draft")],
        model_provider: crate::providers::ModelProviderRef::new("anthropic.default"),
        risk_profile: "default".into(),
        ..AliasedAgentConfig::default()
    };
    config.agents.insert("alpha".to_string(), agent);

    config
}

#[test]
async fn validate_rejects_workspace_access_self_reference() {
    let mut config = multi_agent_test_config();
    let alpha = config.agents.get_mut("alpha").unwrap();
    alpha.workspace.access.insert(
        crate::multi_agent::AgentAlias::new("alpha"),
        crate::multi_agent::AccessMode::Read,
    );
    let err = config
        .validate()
        .expect_err("self-reference must fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("agents.alpha.workspace.access.alpha"),
        "expected field path in error, got: {msg}"
    );
    assert!(
        msg.contains("self-references"),
        "expected self-reference explanation, got: {msg}"
    );
}

#[test]
async fn validate_rejects_workspace_access_dangling_target() {
    let mut config = multi_agent_test_config();
    let alpha = config.agents.get_mut("alpha").unwrap();
    alpha.workspace.access.insert(
        crate::multi_agent::AgentAlias::new("ghost"),
        crate::multi_agent::AccessMode::ReadWrite,
    );
    let err = config
        .validate()
        .expect_err("dangling target must fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("agents.ghost is not configured"),
        "expected dangling-ref explanation, got: {msg}"
    );
}

#[test]
async fn validate_rejects_read_memory_from_self_reference() {
    let mut config = multi_agent_test_config();
    let alpha = config.agents.get_mut("alpha").unwrap();
    alpha
        .workspace
        .read_memory_from
        .push(crate::multi_agent::AgentAlias::new("alpha"));
    let err = config
        .validate()
        .expect_err("self-reference must fail validation");
    assert!(
        err.to_string().contains("read_memory_from[0]"),
        "expected indexed field path, got: {err}"
    );
}

#[test]
async fn validate_rejects_read_memory_from_cross_backend() {
    let mut config = multi_agent_test_config();

    // Add a second agent on Markdown.
    let beta = AliasedAgentConfig {
        channels: vec![crate::providers::ChannelRef::new("telegram.draft")],
        model_provider: crate::providers::ModelProviderRef::new("anthropic.default"),
        risk_profile: "default".into(),
        memory: crate::multi_agent::AgentMemoryConfig {
            backend: crate::multi_agent::MemoryBackendKind::Markdown,
        },
        ..AliasedAgentConfig::default()
    };
    config.agents.insert("beta".to_string(), beta);

    // Alpha (Sqlite default) tries to read from beta (Markdown).
    let alpha = config.agents.get_mut("alpha").unwrap();
    alpha
        .workspace
        .read_memory_from
        .push(crate::multi_agent::AgentAlias::new("beta"));

    let err = config
        .validate()
        .expect_err("cross-backend allowlist must fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("same-backend siblings only"),
        "expected cross-backend explanation, got: {msg}"
    );
}

#[test]
async fn validate_rejects_typed_memory_flags_on_non_sqlite_global_backend() {
    let mut config = multi_agent_test_config();
    config.memory.types.enabled = true;
    config.memory.backend = "markdown.work".to_string();

    let err = config
        .validate()
        .expect_err("typed memory on a non-sqlite global backend must fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("memory.types.enabled") && msg.contains("SQLite-only"),
        "expected SQLite-only explanation naming the flag, got: {msg}"
    );
}

#[test]
async fn validate_rejects_typed_memory_flags_on_non_sqlite_agent_backend() {
    let mut config = multi_agent_test_config();
    config.memory.consolidation_extract_facts = true;

    let beta = AliasedAgentConfig {
        channels: vec![crate::providers::ChannelRef::new("telegram.draft")],
        model_provider: crate::providers::ModelProviderRef::new("anthropic.default"),
        risk_profile: "default".into(),
        memory: crate::multi_agent::AgentMemoryConfig {
            backend: crate::multi_agent::MemoryBackendKind::Markdown,
        },
        ..AliasedAgentConfig::default()
    };
    config.agents.insert("beta".to_string(), beta);

    let err = config
        .validate()
        .expect_err("typed memory with a non-sqlite agent backend must fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("memory.consolidation_extract_facts")
            && msg.contains("agents.beta.memory.backend")
            && msg.contains("Markdown"),
        "expected SQLite-only explanation naming the agent, got: {msg}"
    );
}

#[test]
async fn validate_accepts_typed_memory_flags_on_sqlite() {
    let mut config = multi_agent_test_config();
    config.memory.types.enabled = true;
    config.memory.consolidation_extract_facts = true;
    config.memory.backend = "sqlite".to_string();

    config
        .validate()
        .expect("typed memory on sqlite everywhere must pass validation");
}

#[test]
async fn validate_accepts_typed_memory_flags_on_mixed_case_sqlite() {
    // The runtime classifies backends case-insensitively
    // (`backend_kind_from_dotted` lowercases), so the SQLite-only gate
    // must accept every spelling the runtime resolves to sqlite.
    let mut config = multi_agent_test_config();
    config.memory.types.enabled = true;
    config.memory.backend = " SQLite.default ".to_string();

    config
        .validate()
        .expect("mixed-case sqlite spellings the runtime accepts must pass the typed gate");
}

#[test]
async fn validate_accepts_typed_memory_flags_on_tachi() {
    let mut config = multi_agent_test_config();
    config.memory.types.enabled = true;
    config.memory.consolidation_extract_facts = true;
    config.memory.backend = "tachi".to_string();
    let alpha = config.agents.get_mut("alpha").unwrap();
    alpha.memory.backend = crate::multi_agent::MemoryBackendKind::Tachi;

    config
        .validate()
        .expect("typed memory on tachi everywhere must pass validation");
}

#[test]
async fn validate_allows_non_sqlite_backend_when_typed_memory_flags_off() {
    let mut config = multi_agent_test_config();
    config.memory.backend = "markdown.work".to_string();
    let alpha = config.agents.get_mut("alpha").unwrap();
    alpha.memory.backend = crate::multi_agent::MemoryBackendKind::Markdown;

    config
        .validate()
        .expect("flags-off configs keep every backend choice valid");
}

#[test]
async fn validate_rejects_peer_group_dangling_member() {
    let mut config = multi_agent_test_config();
    let group = crate::multi_agent::PeerGroupConfig {
        channel: "telegram".into(),
        agents: vec![
            crate::multi_agent::AgentAlias::new("alpha"),
            crate::multi_agent::AgentAlias::new("ghost"),
        ],
        ..crate::multi_agent::PeerGroupConfig::default()
    };
    config.peer_groups.insert("team_chat".to_string(), group);
    let err = config
        .validate()
        .expect_err("dangling group member must fail validation");
    assert!(
        err.to_string().contains("peer_groups.team_chat.agents[1]"),
        "expected indexed field path, got: {err}"
    );
}

#[test]
async fn validate_rejects_peer_group_member_without_channel() {
    let mut config = multi_agent_test_config();

    // Add a discord channel and a beta agent that ONLY uses discord.
    config
        .channels
        .discord
        .insert("ops".to_string(), DiscordConfig::default());
    let beta = AliasedAgentConfig {
        channels: vec![crate::providers::ChannelRef::new("discord.ops")],
        model_provider: crate::providers::ModelProviderRef::new("anthropic.default"),
        risk_profile: "default".into(),
        ..AliasedAgentConfig::default()
    };
    config.agents.insert("beta".to_string(), beta);

    // Group on telegram.draft includes beta (who only has discord).
    let group = crate::multi_agent::PeerGroupConfig {
        channel: "telegram".into(),
        agents: vec![
            crate::multi_agent::AgentAlias::new("alpha"),
            crate::multi_agent::AgentAlias::new("beta"),
        ],
        ..crate::multi_agent::PeerGroupConfig::default()
    };
    config.peer_groups.insert("team_chat".to_string(), group);

    let err = config
        .validate()
        .expect_err("channel-mismatch group member must fail validation");
    let msg = err.to_string();
    assert!(
        msg.contains("agents.beta.channels has no entry of type"),
        "expected channel-mismatch explanation, got: {msg}"
    );
}

#[test]
async fn validate_accepts_valid_peer_group_with_two_compatible_members() {
    let mut config = multi_agent_test_config();

    // Beta on the same telegram channel.
    let beta = AliasedAgentConfig {
        channels: vec![crate::providers::ChannelRef::new("telegram.draft")],
        model_provider: crate::providers::ModelProviderRef::new("anthropic.default"),
        risk_profile: "default".into(),
        ..AliasedAgentConfig::default()
    };
    config.agents.insert("beta".to_string(), beta);

    // Group on telegram.draft includes both members.
    let group = crate::multi_agent::PeerGroupConfig {
        channel: "telegram".into(),
        agents: vec![
            crate::multi_agent::AgentAlias::new("alpha"),
            crate::multi_agent::AgentAlias::new("beta"),
        ],
        ..crate::multi_agent::PeerGroupConfig::default()
    };
    config.peer_groups.insert("team_chat".to_string(), group);

    config
        .validate()
        .expect("two-member same-channel peer group must validate cleanly");
}

#[test]
async fn config_validate_rejects_classifier_provider_pointing_at_missing_alias() {
    // Use the SHARED `typed_provider_refs` validation loop — same error
    // surface as tts_provider / transcription_provider.
    let toml = r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        risk_profile = "default"
        classifier_provider = "custom.does-not-exist"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let err = cfg
        .validate()
        .expect_err("missing alias must fail validate");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("classifier_provider")
            && msg.contains("does-not-exist")
            && msg.contains("providers.models.custom.does-not-exist is not configured"),
        "expected DanglingReference error mentioning field + alias + section, got: {msg}"
    );
}

// agent-level summary_provider validated like classifier_provider.
#[tokio::test]
async fn config_validate_rejects_agent_summary_provider_missing_alias() {
    let toml = r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        risk_profile = "default"
        summary_provider = "custom.does-not-exist"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let msg = format!("{:#}", cfg.validate().expect_err("missing alias must fail"));
    assert!(
        msg.contains("summary_provider")
            && msg.contains("providers.models.custom.does-not-exist is not configured"),
        "expected DanglingReference for agent summary_provider, got: {msg}"
    );
}

// ── Advisor (#405 phase 1) ─────────────────────────────────────────

fn advisor_config(agent_extra: &str) -> String {
    format!(
        r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [providers.models.custom.big]
        api_key = "k"
        model = "big-model"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        risk_profile = "default"
        {agent_extra}
    "#
    )
}

#[test]
async fn advisor_model_target_parses_validates_and_round_trips() {
    let toml = advisor_config(
        r#"advisor = "model:custom.big"
        advisor_max_calls_per_turn = 3"#,
    );
    let cfg: Config = toml::from_str(&toml).unwrap();
    cfg.validate()
        .expect("advisor naming a configured alias validates");
    let agent = &cfg.agents["default"];
    assert_eq!(
        agent.advisor,
        Some(crate::advisor::AdvisorTarget::Model("custom.big".into()))
    );
    assert_eq!(agent.advisor_max_calls_per_turn, Some(3));

    // Serialized back as the same prefixed string, and re-parses equal.
    let rendered = toml::to_string(agent).unwrap();
    assert!(
        rendered.contains(r#"advisor = "model:custom.big""#),
        "{rendered}"
    );
    let reparsed: AliasedAgentConfig = toml::from_str(&rendered).unwrap();
    assert_eq!(reparsed.advisor, agent.advisor);
    assert_eq!(reparsed.advisor_max_calls_per_turn, Some(3));

    // Unset stays unset and is not serialized.
    let plain: AliasedAgentConfig = toml::from_str("").unwrap();
    assert!(plain.advisor.is_none());
    assert!(!toml::to_string(&plain).unwrap().contains("advisor"));
}

#[test]
async fn advisor_untyped_target_fails_to_parse() {
    let toml = advisor_config(r#"advisor = "custom.big""#);
    let err = toml::from_str::<Config>(&toml).expect_err("untyped advisor must not parse");
    assert!(err.to_string().contains("model:<type>.<alias>"), "{err}");
}

#[test]
async fn advisor_dangling_model_ref_fails_validation() {
    let toml = advisor_config(r#"advisor = "model:custom.does-not-exist""#);
    let cfg: Config = toml::from_str(&toml).unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate().expect_err("dangling advisor must fail")
    );
    assert!(
        msg.contains("agents.default.advisor")
            && msg.contains("providers.models.custom.does-not-exist is not configured"),
        "expected DanglingReference for advisor, got: {msg}"
    );
}

#[test]
async fn advisor_harness_target_parses_but_is_refused() {
    let toml = advisor_config(r#"advisor = "harness:codex""#);
    let cfg: Config = toml::from_str(&toml).expect("harness target parses");
    assert_eq!(
        cfg.agents["default"].advisor,
        Some(crate::advisor::AdvisorTarget::Harness("codex".into()))
    );
    let msg = format!(
        "{:#}",
        cfg.validate().expect_err("harness advisor is refused")
    );
    assert!(
        msg.contains("agents.default.advisor")
            && msg.contains("harness advisors are not supported yet"),
        "{msg}"
    );
}

#[test]
async fn advisor_zero_calls_per_turn_is_refused() {
    let toml = advisor_config(
        r#"advisor = "model:custom.big"
        advisor_max_calls_per_turn = 0"#,
    );
    let cfg: Config = toml::from_str(&toml).unwrap();
    let msg = format!("{:#}", cfg.validate().expect_err("zero cap is refused"));
    assert!(msg.contains("advisor_max_calls_per_turn"), "{msg}");
}

#[test]
async fn advisor_is_settable_through_the_prop_surface() {
    let toml = advisor_config("");
    let mut cfg: Config = toml::from_str(&toml).unwrap();
    assert_eq!(cfg.get_prop("agents.default.advisor").unwrap(), "<unset>");

    cfg.set_prop("agents.default.advisor", "model:custom.big")
        .unwrap();
    assert_eq!(
        cfg.get_prop("agents.default.advisor").unwrap(),
        "model:custom.big"
    );
    cfg.validate().unwrap();

    cfg.set_prop("agents.default.advisor_max_calls_per_turn", "4")
        .unwrap();
    assert_eq!(cfg.agents["default"].advisor_max_calls_per_turn, Some(4));

    assert!(
        cfg.set_prop("agents.default.advisor", "custom.big")
            .is_err(),
        "an untyped target must be rejected at the prop surface"
    );

    cfg.set_prop("agents.default.advisor", "").unwrap();
    assert!(cfg.agents["default"].advisor.is_none());
}

// ── Cards ────────────────────────────────────────────────────────

fn card_config(agent_body: &str, extra: &str) -> String {
    format!(
        r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [personas.terse]
        directness = "xhigh"

        [cards.analyst]
        persona = "terse"
        risk_profile = "default"

        [cards.analyst.grants]
        tools = [{{ tool = "memory_recall", class = "local_read" }}]

        {extra}

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        {agent_body}
    "#
    )
}

#[tokio::test]
async fn config_validate_accepts_an_agent_defined_solely_by_a_card() {
    let cfg: Config = toml::from_str(&card_config(r#"card = "analyst""#, "")).unwrap();
    cfg.validate()
        .expect("a card alone is a complete definition");
}

#[tokio::test]
async fn config_validate_rejects_carded_agent_with_nonexistent_risk_profile() {
    let toml = card_config(
        r#"card = "orphan""#,
        r#"
        [cards.orphan]
        persona = "terse"
        risk_profile = "ghost"

        [cards.orphan.grants]
        tools = [{ tool = "memory_recall", class = "local_read" }]
        "#,
    );
    let cfg: Config = toml::from_str(&toml).unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate()
            .expect_err("carded agent with dangling risk_profile must fail")
    );
    assert!(
        msg.contains("orphan") && msg.contains("ghost"),
        "the error must name the card and missing risk profile: {msg}"
    );
}

#[tokio::test]
async fn config_validate_accepts_carded_agent_with_existing_risk_profile() {
    let toml = card_config(
        r#"card = "auditor""#,
        r#"
        [risk_profiles.auditor]
        level = "supervised"

        [cards.auditor]
        persona = "terse"
        risk_profile = "auditor"

        [cards.auditor.grants]
        tools = [{ tool = "memory_recall", class = "local_read" }]
        "#,
    );
    let cfg: Config = toml::from_str(&toml).unwrap();
    cfg.validate()
        .expect("carded agent with a configured risk profile must validate");
}

#[tokio::test]
async fn config_validate_rejects_a_card_pointing_nowhere() {
    let cfg: Config = toml::from_str(&card_config(r#"card = "ghost""#, "")).unwrap();
    let msg = format!("{:#}", cfg.validate().expect_err("dangling card must fail"));
    assert!(
        msg.contains("cards.ghost"),
        "the error must name the missing card: {msg}"
    );
}

/// The ambiguity this refuses: with both set, an agent's real authority
/// depends on a precedence rule, and a forgotten precedence rule means an
/// agent reaching further than its author believed.
#[tokio::test]
async fn config_validate_rejects_an_agent_setting_both_card_and_risk_profile() {
    let cfg: Config = toml::from_str(&card_config(
        r#"card = "analyst"
        risk_profile = "default""#,
        "",
    ))
    .unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate()
            .expect_err("card plus risk_profile is ambiguous")
    );
    assert!(
        msg.contains("risk_profile") && msg.contains("card"),
        "the error must name both halves of the conflict: {msg}"
    );
}

#[tokio::test]
async fn config_validate_rejects_an_agent_setting_both_card_and_persona() {
    let cfg: Config = toml::from_str(&card_config(
        r#"card = "analyst"
        persona = "terse""#,
        "",
    ))
    .unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate().expect_err("card plus persona is ambiguous")
    );
    assert!(msg.contains("persona"), "{msg}");
}

/// `mcp_bundles` joins the card mutual-exclusion check (#21): a card's
/// `grants.mcp_bundles` REPLACES the raw field (`resolved_agent_config`),
/// so setting both leaves the raw field silently ignored — the same
/// forgotten-precedence hazard `persona`/`risk_profile` are already
/// guarded against above.
#[tokio::test]
async fn config_validate_rejects_an_agent_setting_both_card_and_mcp_bundles() {
    let cfg: Config = toml::from_str(&card_config(
        r#"card = "analyst"
        mcp_bundles = ["b1"]"#,
        "",
    ))
    .unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate()
            .expect_err("card plus mcp_bundles is ambiguous")
    );
    assert!(
        msg.contains("mcp_bundles") && msg.contains("card"),
        "the error must name both halves of the conflict: {msg}"
    );
}

#[tokio::test]
async fn config_validate_accepts_a_card_only_agent_with_no_raw_mcp_bundles() {
    let cfg: Config = toml::from_str(&card_config(r#"card = "analyst""#, "")).unwrap();
    cfg.validate()
        .expect("a card-only agent with no raw mcp_bundles must validate");
}

#[tokio::test]
async fn config_validate_accepts_an_mcp_bundles_only_agent_without_a_card() {
    // The bundle must actually exist: the pre-existing dangling-reference
    // check on `agents.<alias>.mcp_bundles` fires otherwise, and this
    // test is about the card-exclusion arm, not that one.
    let toml = card_config(
        r#"risk_profile = "default"
        mcp_bundles = ["b1"]"#,
        r#"
        [mcp_bundles.b1]
        servers = []
        "#,
    );
    let cfg: Config = toml::from_str(&toml).unwrap();
    cfg.validate()
        .expect("an uncarded agent's own mcp_bundles must validate on their own");
}

/// A malformed grant list must fail at load, not at the first dispatch.
#[tokio::test]
async fn config_validate_rejects_a_card_granting_one_tool_twice() {
    let toml = card_config(
        r#"card = "dupe""#,
        r#"
        [cards.dupe]
        risk_profile = "default"

        [cards.dupe.grants]
        tools = [
          { tool = "shell", class = "local_act" },
          { tool = "shell", class = "local_read" },
        ]
        "#,
    );
    let cfg: Config = toml::from_str(&toml).unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate().expect_err("a duplicate grant must fail")
    );
    assert!(msg.contains("twice"), "{msg}");
}

/// The invariant a card must not be able to dodge: every enabled agent
/// has exactly one risk profile. A card may supply it, but a card without
/// one has to fail exactly as a bare agent without one does — otherwise
/// "define it with a card" becomes a way to run ungated.
#[tokio::test]
async fn a_card_without_a_risk_profile_cannot_gate_an_agent() {
    let toml = card_config(
        r#"card = "ungated""#,
        r#"
        [cards.ungated]
        persona = "terse"

        [cards.ungated.grants]
        tools = [{ tool = "shell", class = "local_act" }]
        "#,
    );
    let cfg: Config = toml::from_str(&toml).unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate()
            .expect_err("a card carrying no profile must not gate an agent")
    );
    assert!(
        msg.contains("risk_profile"),
        "the error must point at the missing profile: {msg}"
    );
}

/// Agents that never opted into cards must be untouched by any of this.
#[tokio::test]
async fn config_validate_leaves_uncarded_agents_alone() {
    let cfg: Config = toml::from_str(&card_config(r#"risk_profile = "default""#, "")).unwrap();
    cfg.validate()
        .expect("an agent without a card keeps working exactly as before");
}

// ── persona_for_agent ───────────────────────────────────────────

#[tokio::test]
async fn persona_for_agent_follows_the_card() {
    let cfg: Config = toml::from_str(&card_config(r#"card = "analyst""#, "")).unwrap();
    let persona = cfg
        .persona_for_agent("default")
        .expect("a carded agent whose card names a persona resolves it");
    assert_eq!(persona.directness, crate::persona::PersonaLevel::Xhigh);
    let (_, alias, card) = cfg.persona_selection_for_agent("default").unwrap();
    assert_eq!(alias, "terse");
    assert_eq!(card, Some("analyst"));
}

#[tokio::test]
async fn persona_for_agent_resolves_a_direct_persona_on_an_uncarded_agent() {
    let cfg: Config = toml::from_str(&card_config(r#"persona = "terse""#, "")).unwrap();
    let persona = cfg
        .persona_for_agent("default")
        .expect("an uncarded agent's direct persona field resolves");
    assert_eq!(persona.directness, crate::persona::PersonaLevel::Xhigh);
    let (_, alias, card) = cfg.persona_selection_for_agent("default").unwrap();
    assert_eq!(alias, "terse");
    assert_eq!(card, None);
}

#[tokio::test]
async fn persona_for_agent_is_none_for_an_agent_with_neither() {
    let cfg: Config = toml::from_str(&card_config(r#"risk_profile = "default""#, "")).unwrap();
    assert!(
        cfg.persona_for_agent("default").is_none(),
        "an agent with no card and no direct persona has no dials"
    );
}

// ── resolved_agent_config: card mcp_bundles ─────────────────────

#[tokio::test]
async fn resolved_agent_config_mcp_bundles_come_from_the_card() {
    // The agent also names a stale `mcp_bundles` entry of its own — the
    // pointer the card supersedes. It must be ignored, not unioned in:
    // reading the card alone must tell you the agent's whole MCP reach.
    let toml = r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [personas.terse]
        directness = "xhigh"

        [[mcp.servers]]
        name = "hyperion"
        transport = "stdio"
        command = "/usr/bin/hyperion-mcp"

        [[mcp.servers]]
        name = "stale"
        transport = "stdio"
        command = "/usr/bin/stale-mcp"

        [mcp_bundles.hyperion_read]
        servers = ["hyperion"]

        [mcp_bundles.stale_bundle]
        servers = ["stale"]

        [cards.analyst]
        persona = "terse"
        risk_profile = "default"

        [cards.analyst.grants]
        tools = [{ tool = "memory_recall", class = "local_read" }]
        mcp_bundles = ["hyperion_read"]

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        card = "analyst"
        mcp_bundles = ["stale_bundle"]
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();

    let granted: Vec<String> = cfg
        .mcp_servers_for_agent("default")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        granted,
        vec!["hyperion".to_string()],
        "a carded agent's MCP reach comes from the card's grants, and equals them exactly \
         (replaces the agent's own stale mcp_bundles rather than unioning with it)"
    );
}

#[tokio::test]
async fn resolved_agent_config_carded_agent_with_no_card_bundles_gets_no_servers() {
    let toml = r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [[mcp.servers]]
        name = "hyperion"
        transport = "stdio"
        command = "/usr/bin/hyperion-mcp"

        [mcp_bundles.hyperion_read]
        servers = ["hyperion"]

        [cards.bare]
        risk_profile = "default"

        [cards.bare.grants]
        tools = [{ tool = "memory_recall", class = "local_read" }]

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        card = "bare"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!(
        cfg.mcp_servers_for_agent("default").is_empty(),
        "a carded agent whose card grants no bundles gets no servers"
    );
}

// profile-level summary_provider validated by the new profile loop.
#[tokio::test]
async fn config_validate_rejects_profile_summary_provider_missing_alias() {
    let toml = r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.fast.context_compression]
        summary_provider = "custom.nope"

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        risk_profile = "default"
        runtime_profile = "fast"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let msg = format!(
        "{:#}",
        cfg.validate().expect_err("missing profile alias must fail")
    );
    assert!(
        msg.contains("runtime_profiles.fast.context_compression.summary_provider")
            && msg.contains("providers.models.custom.nope is not configured"),
        "expected DanglingReference for profile summary_provider, got: {msg}"
    );
}

// effective_summary_provider precedence — agent → profile → None.
#[tokio::test]
async fn effective_summary_provider_precedence() {
    let toml = r#"
        [providers.models.custom.main]
        api_key = "k"
        model = "m-main"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"
        [providers.models.custom.cheap]
        api_key = "k"
        model = "m-cheap"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"
        [providers.models.custom.profilesum]
        api_key = "k"
        model = "m-profile"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.fast.context_compression]
        summary_provider = "custom.profilesum"

        [agents.a]
        enabled = true
        model_provider = "custom.main"
        risk_profile = "default"
        runtime_profile = "fast"
        summary_provider = "custom.cheap"

        [agents.b]
        enabled = true
        model_provider = "custom.main"
        risk_profile = "default"
        runtime_profile = "fast"

        [agents.c]
        enabled = true
        model_provider = "custom.main"
        risk_profile = "default"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    // agent override wins over the profile
    assert_eq!(
        cfg.effective_summary_provider("a").as_deref(),
        Some("custom.cheap")
    );
    // agent empty → profile value
    assert_eq!(
        cfg.effective_summary_provider("b").as_deref(),
        Some("custom.profilesum")
    );
    // no agent override + no runtime profile → None (caller uses agent's own)
    assert_eq!(cfg.effective_summary_provider("c"), None);
}

// config-time diagnostic for the legacy cross-provider summary_model
// shape. A profile sets the deprecated bare summary_model and is shared by
// two agents on DIFFERENT providers with no summary_provider override -> the
// diagnostic fires and names the profile + the affected agents + providers.
#[tokio::test]
async fn collect_warnings_flags_cross_provider_summary_model() {
    let toml = r#"
        [providers.models.custom.p1]
        api_key = "k"
        model = "m1"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"
        [providers.models.custom.p2]
        api_key = "k"
        model = "m2"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.shared.context_compression]
        summary_model = "haiku"

        [agents.alpha]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"

        [agents.beta]
        enabled = true
        model_provider = "custom.p2"
        risk_profile = "default"
        runtime_profile = "shared"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    let w = warnings
        .iter()
        .find(|w| w.code == "cross_provider_summary_model")
        .expect("expected cross_provider_summary_model warning");
    assert_eq!(
        w.path,
        "runtime_profiles.shared.context_compression.summary_model"
    );
    assert!(
        w.message.contains("haiku"),
        "message names the model: {}",
        w.message
    );
    assert!(
        w.message.contains("alpha -> custom.p1"),
        "message names alpha + provider: {}",
        w.message
    );
    assert!(
        w.message.contains("beta -> custom.p2"),
        "message names beta + provider: {}",
        w.message
    );
}

// The `cross_provider_summary_model` diagnostic must report the setting
// as unsupported/inert like every other `context_compression` knob, not
// as something that is actively dispatched onto per-agent providers and
// fails at runtime — there is no runtime consumer left to dispatch
// anything. The cross-provider detail (which agents, which providers)
// must still be present since it is useful context for the fix, but the
// message must not claim any runtime behavior.
#[tokio::test]
async fn collect_warnings_cross_provider_summary_model_reports_inert_not_dispatch() {
    let toml = r#"
        [providers.models.custom.p1]
        api_key = "k"
        model = "m1"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"
        [providers.models.custom.p2]
        api_key = "k"
        model = "m2"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.shared.context_compression]
        summary_model = "haiku"

        [agents.alpha]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"

        [agents.beta]
        enabled = true
        model_provider = "custom.p2"
        risk_profile = "default"
        runtime_profile = "shared"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    let w = warnings
        .iter()
        .find(|w| w.code == "cross_provider_summary_model")
        .expect("expected cross_provider_summary_model warning");
    assert!(
        w.message.contains("not currently implemented") && w.message.contains("no effect"),
        "message must truthfully report the setting as unsupported/inert: {}",
        w.message
    );
    assert!(
        !w.message.contains("silently fails"),
        "message must not claim the setting silently fails at runtime: {}",
        w.message
    );
    assert!(
        !w.message.contains("dispatched"),
        "message must not claim the setting is dispatched to a provider at runtime: {}",
        w.message
    );
    // Cross-provider specificity must survive the rewrite — it is still
    // useful detail even though the setting is inert.
    assert!(
        w.message.contains("alpha -> custom.p1") && w.message.contains("beta -> custom.p2"),
        "message must keep naming the affected agents and providers: {}",
        w.message
    );
    // The remediation must NOT send the operator to another inert
    // context_compression field: this PR's per-field pass classifies a
    // non-default `summary_provider` as unsupported/inert too, so
    // "migrate to context_compression.summary_provider" would just produce
    // another no-effect setting and another warning.
    assert!(
        !w.message
            .contains("Migrate to context_compression.summary_provider"),
        "remediation must not recommend migrating to the inert summary_provider: {}",
        w.message
    );
    assert!(
        w.message
            .contains("Remove the unsupported context_compression setting"),
        "remediation should tell the operator to remove the inert setting: {}",
        w.message
    );
}

// The runtime context compressor was removed; nothing reads
// `context_compression` at runtime anymore, so an explicit
// `enabled = true` on a named runtime profile is inert and must be
// flagged.
#[tokio::test]
async fn collect_warnings_flags_context_compression_enabled_on_runtime_profile() {
    let toml = r#"
        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.fast.context_compression]
        enabled = true

        [agents.alpha]
        enabled = true
        risk_profile = "default"
        runtime_profile = "fast"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    let w = warnings
        .iter()
        .find(|w| w.code == "context_compression_unsupported")
        .expect("expected context_compression_unsupported warning");
    assert_eq!(w.path, "runtime_profiles.fast.context_compression.enabled");
    assert!(
        w.message.contains("not currently implemented"),
        "message explains the flag is inert: {}",
        w.message
    );
}

// The legacy pre-V3 `[agent.context_compression]` top-level table is
// folded into `[runtime_profiles.default]` by the V1/V2→V3 migration
// (see `schema/v2.rs`), so it must surface the same diagnostic once
// migrated — this is the historical form of the surface commonly
// called "agent-level" configuration.
#[::core::prelude::v1::test]
fn collect_warnings_flags_context_compression_enabled_via_legacy_agent_table() {
    let raw = r#"
        default_temperature = 0.7

        [agent.context_compression]
        enabled = true
    "#;
    let parsed = crate::migration::migrate_to_current(raw).expect("migration succeeds");
    let warnings = parsed.collect_warnings();
    let w = warnings
        .iter()
        .find(|w| w.code == "context_compression_unsupported")
        .expect("expected context_compression_unsupported warning after migration");
    assert_eq!(
        w.path,
        "runtime_profiles.default.context_compression.enabled"
    );
}

// A default config (no explicit `context_compression.enabled`) must stay
// silent — the flag now defaults to `false`, matching the runtime, which
// does not consult it at all.
#[tokio::test]
async fn collect_warnings_silent_for_context_compression_default() {
    let toml = r#"
        [risk_profiles.default]
        level = "supervised"

        [agents.alpha]
        enabled = true
        risk_profile = "default"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    assert!(
        !warnings
            .iter()
            .any(|w| w.code == "context_compression_unsupported"),
        "default config must not flag context_compression_unsupported: {warnings:?}"
    );
}

// Every `context_compression` knob is inert, not just `enabled` — tuning
// fields set to non-default values must each surface their own warning
// with a per-field path, even with `enabled` left off, since the whole
// struct is covered.
#[tokio::test]
async fn collect_warnings_flags_context_compression_tuning_fields() {
    let toml = r#"
        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.fast.context_compression]
        threshold_ratio = 0.9
        protect_first_n = 500

        [agents.alpha]
        enabled = true
        risk_profile = "default"
        runtime_profile = "fast"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    let paths: Vec<&str> = warnings
        .iter()
        .filter(|w| w.code == "context_compression_unsupported")
        .map(|w| w.path.as_str())
        .collect();
    assert!(
        paths.contains(&"runtime_profiles.fast.context_compression.threshold_ratio"),
        "threshold_ratio must be flagged: {paths:?}"
    );
    assert!(
        paths.contains(&"runtime_profiles.fast.context_compression.protect_first_n"),
        "protect_first_n must be flagged: {paths:?}"
    );
    // `enabled` was not set (defaults to false) — no warning for it.
    assert!(
        !paths.contains(&"runtime_profiles.fast.context_compression.enabled"),
        "unset enabled must not be flagged: {paths:?}"
    );
    let w = warnings
        .iter()
        .find(|w| w.path == "runtime_profiles.fast.context_compression.threshold_ratio")
        .expect("threshold_ratio warning present");
    assert!(
        w.message.contains("non-default value"),
        "message says the value is non-default: {}",
        w.message
    );
}

// A knob explicitly written at its default value is indistinguishable
// from an omitted one post-deserialization and must stay silent — the
// same accepted limitation as `validate_memory_semantics`.
#[tokio::test]
async fn collect_warnings_silent_for_context_compression_default_values_written_explicitly() {
    let toml = r#"
        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.fast.context_compression]
        enabled = false
        threshold_ratio = 0.50
        protect_first_n = 3
        tool_result_retrim_chars = 2000

        [agents.alpha]
        enabled = true
        risk_profile = "default"
        runtime_profile = "fast"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    assert!(
        !warnings
            .iter()
            .any(|w| w.code == "context_compression_unsupported"),
        "explicit default values must not flag context_compression_unsupported: {warnings:?}"
    );
}

// Specific-warning-wins dedup: a bare cross-provider `summary_model`
// already draws the more specific `cross_provider_summary_model`
// diagnostic, which itself reports the setting as inert (same fact as
// `context_compression_unsupported`) plus the cross-provider detail, so
// the generic inert warning must NOT also fire for the identical path —
// doctor/gateway print both with no dedup, and it would just be the same
// statement twice.
#[tokio::test]
async fn collect_warnings_context_compression_defers_to_cross_provider_summary_model() {
    let toml = r#"
        [providers.models.custom.p1]
        api_key = "k"
        model = "m1"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"
        [providers.models.custom.p2]
        api_key = "k"
        model = "m2"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.shared.context_compression]
        summary_model = "haiku"

        [agents.alpha]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"

        [agents.beta]
        enabled = true
        model_provider = "custom.p2"
        risk_profile = "default"
        runtime_profile = "shared"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    let summary_model_warnings: Vec<_> = warnings
        .iter()
        .filter(|w| w.path == "runtime_profiles.shared.context_compression.summary_model")
        .collect();
    assert_eq!(
        summary_model_warnings.len(),
        1,
        "exactly one warning for the summary_model path: {summary_model_warnings:?}"
    );
    assert_eq!(
        summary_model_warnings[0].code, "cross_provider_summary_model",
        "the specific cross-provider diagnostic wins for the shared path"
    );
}

// Same-provider control: without a cross-provider diagnostic covering
// the path, the inert warning must still fire for `summary_model` — no
// other diagnostic covers the single-provider shape.
#[tokio::test]
async fn collect_warnings_context_compression_flags_same_provider_summary_model() {
    let toml = r#"
        [providers.models.custom.p1]
        api_key = "k"
        model = "m1"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.shared.context_compression]
        summary_model = "haiku"

        [agents.alpha]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"

        [agents.beta]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    let warnings = cfg.collect_warnings();
    let w = warnings
        .iter()
        .find(|w| w.path == "runtime_profiles.shared.context_compression.summary_model")
        .expect("expected a warning for the summary_model path");
    assert_eq!(
        w.code, "context_compression_unsupported",
        "single-provider summary_model gets the inert warning"
    );
}

// exposed_skills set with no skill_bundles -> the agent card resolves no
// skills (skills: []) silently; the diagnostic fires and names the agent.

// exposed_skills set alongside at least one declared skill_bundle -> no
// structural diagnostic (disk resolution governs whether ids actually
// resolve; that is out of scope for this offline check).

// Control: same profile + summary_model but both agents on the SAME provider
// -> no diagnostic (deprecated-but-correct; runtime WARN still nudges).
#[tokio::test]
async fn collect_warnings_silent_for_same_provider_summary_model() {
    let toml = r#"
        [providers.models.custom.p1]
        api_key = "k"
        model = "m1"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.shared.context_compression]
        summary_model = "haiku"

        [agents.alpha]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"

        [agents.beta]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!(
        !cfg.collect_warnings()
            .iter()
            .any(|w| w.code == "cross_provider_summary_model"),
        "same-provider use must not warn"
    );
}

// Control: cross-provider agents but each sets an agent-level
// summary_provider override -> the override supersedes the bare id, so no
// diagnostic.
#[tokio::test]
async fn collect_warnings_silent_when_summary_provider_override_present() {
    let toml = r#"
        [providers.models.custom.p1]
        api_key = "k"
        model = "m1"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"
        [providers.models.custom.p2]
        api_key = "k"
        model = "m2"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"
        [providers.models.custom.sum]
        api_key = "k"
        model = "ms"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [runtime_profiles.shared.context_compression]
        summary_model = "haiku"

        [agents.alpha]
        enabled = true
        model_provider = "custom.p1"
        risk_profile = "default"
        runtime_profile = "shared"
        summary_provider = "custom.sum"

        [agents.beta]
        enabled = true
        model_provider = "custom.p2"
        risk_profile = "default"
        runtime_profile = "shared"
        summary_provider = "custom.sum"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!(
        !cfg.collect_warnings()
            .iter()
            .any(|w| w.code == "cross_provider_summary_model"),
        "agent-level summary_provider override must suppress the warning"
    );
}

const SEMANTIC_MEMORY_WARNING: &str = "memory_semantic_search_without_embedder";

fn warnings_with_code(
    config: &Config,
    code: &str,
) -> Vec<crate::validation_warnings::ValidationWarning> {
    config
        .collect_warnings()
        .into_iter()
        .filter(|warning| warning.code == code)
        .collect()
}

fn suppress_semantic_memory_warning(config: &mut Config) {
    config.memory.search_mode = SearchMode::Bm25;
}

#[test]
async fn collect_warnings_flags_sqlite_hybrid_without_embedder() {
    let config = Config::default();

    let warnings = warnings_with_code(&config, SEMANTIC_MEMORY_WARNING);
    assert_eq!(warnings.len(), 1);
    let warning = &warnings[0];
    assert_eq!(warning.path, "memory.search_mode");
    assert!(
        warning.message.contains("sqlite"),
        "warning should name sqlite memory: {}",
        warning.message
    );
    assert!(
        warning.message.contains("keyword-only"),
        "warning should describe the runtime degradation: {}",
        warning.message
    );
}

#[test]
async fn collect_warnings_flags_sqlite_embedding_without_embedder() {
    let mut config = Config::default();
    config.memory.search_mode = SearchMode::Embedding;

    let warnings = warnings_with_code(&config, SEMANTIC_MEMORY_WARNING);
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].message.contains("\"embedding\""),
        "warning should name embedding search mode: {}",
        warnings[0].message
    );
}

#[test]
async fn collect_warnings_silent_for_valid_hint_embedding_route() {
    let mut config = Config::default();
    config.memory.embedding_model = "hint:semantic".to_string();
    config.providers.models.openai.insert(
        "default".to_string(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                api_key: Some("k".to_string()),
                model: Some("gpt-4o".to_string()),
                ..Default::default()
            },
        },
    );
    config.embedding_routes.push(EmbeddingRouteConfig {
        hint: "semantic".to_string(),
        model_provider: "openai.default".to_string(),
        model: "text-embedding-3-small".to_string(),
        dimensions: Some(1536),
        api_key: None,
    });
    config.validate().expect("hint route should validate");

    assert!(
        warnings_with_code(&config, SEMANTIC_MEMORY_WARNING).is_empty(),
        "valid hint route must count as an effective embedder"
    );
}

#[test]
async fn collect_warnings_flags_missing_hint_embedding_route() {
    let mut config = Config::default();
    config.memory.embedding_model = "hint:semantic".to_string();

    let warnings = warnings_with_code(&config, SEMANTIC_MEMORY_WARNING);
    assert_eq!(warnings.len(), 1);
}

#[test]
async fn collect_warnings_flags_invalid_hint_embedding_route() {
    let mut config = Config::default();
    config.memory.embedding_model = "hint:semantic".to_string();
    config.embedding_routes.push(EmbeddingRouteConfig {
        hint: "semantic".to_string(),
        model_provider: "none".to_string(),
        model: "text-embedding-3-small".to_string(),
        dimensions: Some(1536),
        api_key: None,
    });

    let warnings = warnings_with_code(&config, SEMANTIC_MEMORY_WARNING);
    assert_eq!(warnings.len(), 1);
}

#[test]
async fn collect_warnings_silent_for_bm25_without_embedder() {
    let mut config = Config::default();
    config.memory.search_mode = SearchMode::Bm25;

    assert!(warnings_with_code(&config, SEMANTIC_MEMORY_WARNING).is_empty());
}

#[test]
async fn collect_warnings_silent_for_non_sqlite_without_embedder() {
    let mut config = Config::default();
    config.memory.backend = "markdown.default".to_string();

    assert!(warnings_with_code(&config, SEMANTIC_MEMORY_WARNING).is_empty());
}

const INERT_MEMORY_KNOB_WARNING: &str = "memory_config_knob_inert";

fn inert_knob_paths(config: &Config) -> Vec<String> {
    warnings_with_code(config, INERT_MEMORY_KNOB_WARNING)
        .into_iter()
        .map(|warning| warning.path)
        .collect()
}

#[test]
async fn validate_memory_semantics_silent_at_defaults() {
    let config = Config::default();

    assert!(inert_knob_paths(&config).is_empty());
}

#[test]
async fn validate_memory_semantics_warns_for_non_default_retrieval_stages() {
    let mut config = Config::default();
    config.memory.retrieval_stages = vec!["fts".into()];

    assert_eq!(inert_knob_paths(&config), vec!["memory.retrieval_stages"]);
}

#[test]
async fn validate_memory_semantics_warns_for_non_default_fts_early_return_score() {
    let mut config = Config::default();
    config.memory.fts_early_return_score = 0.5;

    assert_eq!(
        inert_knob_paths(&config),
        vec!["memory.fts_early_return_score"]
    );
}

#[test]
async fn validate_memory_semantics_warns_for_rerank_enabled() {
    let mut config = Config::default();
    config.memory.rerank_enabled = true;

    let warnings = warnings_with_code(&config, INERT_MEMORY_KNOB_WARNING);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].path, "memory.rerank_enabled");
    assert!(
        warnings[0].message.contains("currently has no effect"),
        "warning should state the knob has no effect: {}",
        warnings[0].message
    );
}

#[test]
async fn validate_memory_semantics_warns_for_non_default_rerank_threshold() {
    let mut config = Config::default();
    config.memory.rerank_threshold = 10;

    assert_eq!(inert_knob_paths(&config), vec!["memory.rerank_threshold"]);
}

#[test]
async fn validate_memory_semantics_reports_each_set_knob() {
    let mut config = Config::default();
    config.memory.rerank_enabled = true;
    config.memory.rerank_threshold = 10;

    assert_eq!(
        inert_knob_paths(&config),
        vec!["memory.rerank_enabled", "memory.rerank_threshold"]
    );
}

#[test]
async fn config_validate_accepts_classifier_provider_pointing_at_existing_alias() {
    let toml = r#"
        [providers.models.custom.default]
        api_key = "k1"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [providers.models.custom.kimi-k2-5]
        api_key = "k2"
        model = "kimi-k2.5"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        risk_profile = "default"
        classifier_provider = "custom.kimi-k2-5"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    cfg.validate()
        .expect("validate must succeed for resolvable ref");
    assert_eq!(
        cfg.agents
            .get("default")
            .unwrap()
            .classifier_provider
            .as_str(),
        "custom.kimi-k2-5"
    );
}

#[test]
async fn config_validate_accepts_empty_classifier_provider_as_inheritance_signal() {
    // No classifier_provider field at all → must validate, must remain
    // the empty default. This pins backward compatibility.
    let toml = r#"
        [providers.models.custom.default]
        api_key = "k"
        model = "qwen3.6-plus"
        uri = "https://example.com/v1"
        wire_api = "chat_completions"

        [risk_profiles.default]
        level = "supervised"

        [agents.default]
        enabled = true
        model_provider = "custom.default"
        risk_profile = "default"
    "#;
    let cfg: Config = toml::from_str(toml).unwrap();
    cfg.validate()
        .expect("missing classifier_provider must validate");
    assert!(
        cfg.agents
            .get("default")
            .unwrap()
            .classifier_provider
            .is_empty()
    );
}

fn provider_entry_with_fallback(fallback: &[&str]) -> OpenAIModelProviderConfig {
    OpenAIModelProviderConfig {
        base: ModelProviderConfig {
            model: Some("gpt-4o".to_string()),
            fallback: fallback
                .iter()
                .map(|s| crate::providers::ModelProviderRef::new(*s))
                .collect(),
            ..Default::default()
        },
    }
}

#[test]
async fn fallback_warns_on_dangling_ref() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    config.providers.models.openai.insert(
        "primary".to_string(),
        provider_entry_with_fallback(&["openai.ghost"]),
    );

    let warnings = config.collect_warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "dangling_fallback_ref");
    assert_eq!(
        warnings[0].path,
        "providers.models.openai.primary.fallback[0]"
    );
}

#[test]
async fn fallback_no_warning_when_ref_resolves() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    config.providers.models.openai.insert(
        "primary".to_string(),
        provider_entry_with_fallback(&["openai.backup"]),
    );
    config
        .providers
        .models
        .openai
        .insert("backup".to_string(), provider_entry_with_fallback(&[]));

    assert!(config.collect_warnings().is_empty());
}

#[test]
async fn fallback_warns_on_two_node_cycle() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    config
        .providers
        .models
        .openai
        .insert("a".to_string(), provider_entry_with_fallback(&["openai.b"]));
    config
        .providers
        .models
        .openai
        .insert("b".to_string(), provider_entry_with_fallback(&["openai.a"]));

    let cycle_warnings: Vec<_> = config
        .collect_warnings()
        .into_iter()
        .filter(|w| w.code == "fallback_cycle")
        .collect();
    assert!(
        !cycle_warnings.is_empty(),
        "a->b->a must surface at least one fallback_cycle warning"
    );
}

#[test]
async fn fallback_self_reference_is_a_cycle() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    config.providers.models.openai.insert(
        "loop".to_string(),
        provider_entry_with_fallback(&["openai.loop"]),
    );

    let warnings = config.collect_warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "fallback_cycle");
}

#[test]
async fn fallback_empty_ref_is_skipped() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    config
        .providers
        .models
        .openai
        .insert("primary".to_string(), provider_entry_with_fallback(&[""]));

    assert!(config.collect_warnings().is_empty());
}

#[test]
async fn fallback_warns_when_chain_exceeds_max_depth() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    let n = crate::providers::MAX_FALLBACK_DEPTH + 2;
    for i in 0..n {
        let next = if i + 1 < n {
            vec![format!("openai.a{}", i + 1)]
        } else {
            vec![]
        };
        let refs: Vec<&str> = next.iter().map(String::as_str).collect();
        config
            .providers
            .models
            .openai
            .insert(format!("a{i}"), provider_entry_with_fallback(&refs));
    }

    let depth_warnings: Vec<_> = config
        .collect_warnings()
        .into_iter()
        .filter(|w| w.code == "max_fallback_depth_exceeded")
        .collect();
    assert!(
        !depth_warnings.is_empty(),
        "a chain deeper than MAX_FALLBACK_DEPTH must surface a max_fallback_depth_exceeded warning"
    );
}

#[test]
async fn fallback_models_warns_on_empty_entry() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    let mut entry = provider_entry_with_fallback(&[]);
    entry.base.fallback_models = vec!["".to_string()];
    config
        .providers
        .models
        .openai
        .insert("primary".to_string(), entry);

    let warnings = config.collect_warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "empty_fallback_model");
}

#[test]
async fn fallback_models_warns_on_duplicate_of_primary() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    let mut entry = provider_entry_with_fallback(&[]);
    entry.base.fallback_models = vec!["gpt-4o".to_string()];
    config
        .providers
        .models
        .openai
        .insert("primary".to_string(), entry);

    let warnings = config.collect_warnings();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "fallback_model_duplicates_primary");
}

#[test]
async fn fallback_models_distinct_entries_do_not_warn() {
    let mut config = Config::default();
    suppress_semantic_memory_warning(&mut config);
    let mut entry = provider_entry_with_fallback(&[]);
    entry.base.fallback_models = vec!["gpt-4o-mini".to_string()];
    config
        .providers
        .models
        .openai
        .insert("primary".to_string(), entry);

    assert!(config.collect_warnings().is_empty());
}

fn insert_card(cfg: &mut Config, card_alias: &str, risk_profile: &str) {
    cfg.cards.insert(
        card_alias.to_string(),
        crate::card::AgentCard {
            risk_profile: risk_profile.into(),
            ..crate::card::AgentCard::default()
        },
    );
}

/// Strip an agent's direct `risk_profile` and point it at a card
/// instead — the shape validation requires (never both set).
fn carded(cfg: &mut Config, agent_alias: &str, card_alias: &str) {
    let agent = cfg.agents.get_mut(agent_alias).unwrap();
    agent.risk_profile = String::new().into();
    agent.card = card_alias.into();
}

#[test]
async fn channel_presence_names_are_unique_and_undeliverable_set_is_fixed() {
    let presence = ChannelsConfig::default().channel_presence();
    let mut seen = std::collections::HashSet::new();
    for (name, _, _, _) in presence {
        assert!(seen.insert(name), "duplicate channel_presence name: {name}");
    }
    // Non-deliverable = retired transports + the input-only voice_duplex.
    let mut undeliverable: Vec<&str> = presence
        .iter()
        .filter(|(_, _, deliverable, _)| !*deliverable)
        .map(|(name, _, _, _)| *name)
        .collect();
    undeliverable.sort_unstable();
    let mut expected: Vec<&str> = crate::schema::RETIRED_TRANSPORT_KINDS
        .to_vec()
        .into_iter()
        .filter(|kind| !SCHEMA_DELETED.contains(kind))
        .collect();
    expected.extend_from_slice(&["voice_duplex"]);
    expected.sort_unstable();
    assert_eq!(
        undeliverable, expected,
        "only retired transports and input-only transports may be non-deliverable; \
         update channel_presence and is_channel_deliverable together"
    );
}

const SCHEMA_DELETED: &[&str] = &[
    "slack",
    "mattermost",
    "imessage",
    "matrix",
    "signal",
    "irc",
    "twitch",
    "lark",
    "line",
    "dingtalk",
    "wecom",
    "wecom_ws",
    "wechat",
    "qq",
    "twitter",
    "mochat",
    "nostr",
    "clawdtalk",
    "reddit",
    "bluesky",
    "git",
    "voice_call",
    "voice_wake",
    "amqp",
    "feishu",
    "whatsapp",
];

#[test]
async fn retired_transport_kinds_complement_the_live_surfaces() {
    let presence = ChannelsConfig::default().channel_presence();
    let mut live: Vec<&str> = presence
        .iter()
        .filter(|(_, _, _, retired)| !*retired)
        .map(|(name, _, _, _)| *name)
        .collect();
    live.sort_unstable();
    assert_eq!(
        live,
        ["discord", "email", "telegram", "voice_duplex", "webhook"],
        "the non-retired registry must be exactly the retained surfaces: \
         telegram bridge, webhook ingress, email/discord tool config, and \
         the voice-duplex Node path (#584)"
    );

    // Every RETIRED_TRANSPORT_KINDS entry still resolves through the
    // predicate, but Slice 3-deleted schemas have no registry row at all:
    // a leftover section cannot even be configured, so bind/heartbeat
    // rejection plus the load-time tombstone are their only surfaces.
    for kind in crate::schema::RETIRED_TRANSPORT_KINDS {
        assert!(crate::schema::is_retired_transport(kind), "{kind}");
        if SCHEMA_DELETED.contains(kind) {
            assert!(
                !presence.iter().any(|(name, _, _, _)| name == kind),
                "schema-deleted kind {kind} must not have a channel_presence row"
            );
        } else {
            assert!(
                presence.iter().any(|(name, _, _, _)| name == kind),
                "RETIRED_TRANSPORT_KINDS entry {kind} has no channel_presence row"
            );
        }
    }
    for info in ChannelsConfig::default().channels() {
        assert_eq!(
            info.retired,
            crate::schema::is_retired_transport(info.kind),
            "channels() retired flag drifted for {}",
            info.kind
        );
    }
    // The kebab-case display kinds channels() emits for two-word sections
    // normalize back to their section keys.
    assert!(crate::schema::is_retired_transport("whatsapp-web"));
    assert!(crate::schema::is_retired_transport("wecom-ws"));
    assert!(crate::schema::is_retired_transport("voice-wake"));
    assert!(!crate::schema::is_retired_transport("telegram"));
    let cfg = ChannelsConfig::default();
    assert!(!cfg.is_known_channel("Slack"));
    assert!(!cfg.is_known_channel("linq"));
}

// ── Serde-default vs struct-Default drift guards ──────────────
//
// The save path prunes fields whose value equals serde's default.
// If `#[serde(default)]` and `impl Default` disagree, a save →
// load round-trip silently flips the field to the serde default,
// which is. These tests catch that drift: an empty TOML
// table (the extreme case of pruning — all fields pruned away)
// must deserialize to the same value as the struct's `Default`.

// ── Schema-walked reload round-trip smoke battery ─────────────
//
// Reload re-reads config.toml and rebuilds the in-memory Config; any
// scalar field that does not survive a serialize -> deserialize cycle
// is silently lost on reload (the class). This walks every
// scalar prop the derive exposes, mutates it off-default, round-trips
// the whole Config through TOML, and asserts the mutated value comes
// back. Driven entirely off prop_fields() so it tracks the schema.

fn values_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => (x - y).abs() < f64::EPSILON,
        _ => false,
    }
}

fn off_default_value_for(field: &crate::traits::PropFieldInfo, current: &str) -> Option<String> {
    match field.kind {
        PropKind::Bool => Some(if current == "true" { "false" } else { "true" }.to_string()),
        PropKind::Integer => {
            let n: i128 = current.parse().unwrap_or(0);
            Some((n.wrapping_add(7)).to_string())
        }
        PropKind::Float => {
            let n: f64 = current.parse().unwrap_or(0.0);
            Some(format!("{:.3}", n + 1.5))
        }
        PropKind::String => {
            let probe = "zc_reload_probe";
            if current == probe {
                Some("zc_reload_probe_alt".to_string())
            } else {
                Some(probe.to_string())
            }
        }
        PropKind::Enum => field.enum_variants.and_then(|variants| {
            variants()
                .into_iter()
                .find(|v| v != current)
                .or_else(|| variants().into_iter().next())
        }),
        PropKind::AliasRef | PropKind::StringArray | PropKind::ObjectArray | PropKind::Object => {
            None
        }
    }
}

#[test]
async fn every_scalar_field_survives_toml_reload_round_trip() {
    let mut config = Config::default();
    let fields = config.prop_fields();

    let mut mutated: Vec<(String, String)> = Vec::new();
    let mut skipped_non_scalar = 0usize;
    let mut skipped_unsettable = 0usize;

    for field in &fields {
        if field.is_secret {
            continue;
        }
        let Ok(current) = config.get_prop(&field.name) else {
            skipped_unsettable += 1;
            continue;
        };
        let Some(target) = off_default_value_for(field, &current) else {
            skipped_non_scalar += 1;
            continue;
        };
        if config.set_prop(&field.name, &target).is_err() {
            skipped_unsettable += 1;
            continue;
        }
        mutated.push((field.name.clone(), target));
    }

    let serialized = toml::to_string(&config).expect("mutated config must serialize");
    let reloaded: Config = toml::from_str(&serialized).expect("serialized config must deserialize");

    let mut lost: Vec<String> = Vec::new();
    for (name, expected) in &mutated {
        match reloaded.get_prop(name) {
            Ok(got) if values_match(&got, expected) => {}
            Ok(got) => lost.push(format!("{name}: set {expected:?}, reloaded {got:?}")),
            Err(e) => lost.push(format!("{name}: set {expected:?}, reload read failed: {e}")),
        }
    }

    assert!(
        lost.is_empty(),
        "{} scalar field(s) did not survive a TOML reload round-trip ({} mutated, {} non-scalar skipped, {} unsettable skipped):\n{}",
        lost.len(),
        mutated.len(),
        skipped_non_scalar,
        skipped_unsettable,
        lost.join("\n")
    );
    assert!(
        mutated.len() > 100,
        "smoke battery covered only {} scalar fields; schema walk likely regressed",
        mutated.len()
    );
}

#[test]
async fn empty_table_round_trips_to_section_defaults() {
    // An empty TOML table must deserialize to the same values as the
    // hand-written `Default` impl for each section.
    let http: HttpRequestConfig = toml::from_str("").unwrap();
    let http_default = HttpRequestConfig::default();
    assert_eq!(http.enabled, http_default.enabled);
    assert_eq!(http.allowed_domains, http_default.allowed_domains);
    assert_eq!(http.max_response_size, http_default.max_response_size);

    let web_fetch: WebFetchConfig = toml::from_str("").unwrap();
    assert_eq!(web_fetch.enabled, WebFetchConfig::default().enabled);
    let web_search: WebSearchConfig = toml::from_str("").unwrap();
    assert_eq!(web_search.enabled, WebSearchConfig::default().enabled);
    let memory: MemoryConfig = toml::from_str("").unwrap();
    assert_eq!(memory.backend, MemoryConfig::default().backend);
    let tunnel: TunnelConfig = toml::from_str("").unwrap();
    assert_eq!(
        tunnel.tunnel_provider,
        TunnelConfig::default().tunnel_provider
    );
    let hooks: HooksConfig = toml::from_str("").unwrap();
    assert_eq!(hooks.enabled, HooksConfig::default().enabled);
    let builtin: BuiltinHooksConfig = toml::from_str("").unwrap();
    assert_eq!(
        builtin.command_logger,
        BuiltinHooksConfig::default().command_logger
    );
}

#[test]
async fn whitespace_only_model_provider_is_not_dispatchable() {
    let cfg = config_with_dispatchable_agent(AliasedAgentConfig {
        model_provider: "   ".into(),
        ..fully_dispatchable_agent()
    });
    assert!(
        !cfg.agent_is_dispatchable("a"),
        "whitespace-only model_provider should not be dispatchable"
    );

    let cfg = config_with_dispatchable_agent(fully_dispatchable_agent());
    assert!(
        cfg.agent_is_dispatchable("a"),
        "non-empty model_provider should be dispatchable"
    );
}

/// An uncarded agent missing `risk_profile` still gates — unchanged by
/// #21's fix, which only changes how a *carded* agent's profile
/// resolves.
#[test]
async fn missing_risk_profile_is_not_dispatchable_when_uncarded() {
    let cfg = config_with_dispatchable_agent(AliasedAgentConfig {
        risk_profile: String::new().into(),
        ..fully_dispatchable_agent()
    });
    assert!(!cfg.agent_is_dispatchable("a"));
}

#[test]
async fn missing_runtime_profile_is_not_dispatchable() {
    let cfg = config_with_dispatchable_agent(AliasedAgentConfig {
        runtime_profile: String::new().into(),
        ..fully_dispatchable_agent()
    });
    assert!(!cfg.agent_is_dispatchable("a"));
}

#[test]
async fn disabled_agent_is_not_dispatchable() {
    let cfg = config_with_dispatchable_agent(AliasedAgentConfig {
        enabled: false,
        ..fully_dispatchable_agent()
    });
    assert!(!cfg.agent_is_dispatchable("a"));
}

#[test]
async fn unknown_alias_is_not_dispatchable() {
    let cfg = config_with_dispatchable_agent(fully_dispatchable_agent());
    assert!(!cfg.agent_is_dispatchable("does-not-exist"));
}

/// #21, sixth instance — the discriminator. A valid, enabled carded
/// agent must pass `Config::agent_is_dispatchable`, even though its raw
/// `risk_profile` field is forced empty by construction (the card
/// carries the profile instead; see `carded()`). The deleted
/// `AliasedAgentConfig::is_dispatchable` read that raw field directly,
/// so it would have rejected this exact agent — reproduce that old
/// expression inline (the method no longer exists to call) to prove
/// the bug it fixed. Reverting `agent_is_dispatchable` to read
/// `agent.risk_profile` directly instead of
/// `resolved_risk_profile_alias` turns the first assertion red.
#[test]
async fn carded_agent_is_dispatchable_where_the_old_raw_check_was_not() {
    let mut cfg = config_with_dispatchable_agent(fully_dispatchable_agent());
    insert_card(&mut cfg, "a_card", "shared");
    carded(&mut cfg, "a", "a_card");

    assert!(cfg.agent_is_dispatchable("a"), "carded agent must dispatch");

    let agent = cfg.agent("a").unwrap();
    let old_raw_field_logic = agent.enabled
        && !agent.model_provider.trim().is_empty()
        && !agent.risk_profile.trim().is_empty()
        && !agent.runtime_profile.trim().is_empty();
    assert!(
        !old_raw_field_logic,
        "discriminator broken: the old raw-field check should reject this \
         carded agent (raw risk_profile is empty by construction) — if it \
         no longer does, this fixture stopped exercising the card path"
    );
}

#[test]
async fn carded_agent_with_dangling_card_is_not_dispatchable() {
    let mut cfg = config_with_dispatchable_agent(fully_dispatchable_agent());
    carded(&mut cfg, "a", "no-such-card");
    assert!(
        !cfg.agent_is_dispatchable("a"),
        "a card alias with no [cards.<alias>] entry must not resolve a risk profile"
    );
}

fn fully_dispatchable_agent() -> AliasedAgentConfig {
    AliasedAgentConfig {
        enabled: true,
        risk_profile: "default".into(),
        runtime_profile: "default".into(),
        model_provider: "gpt4".into(),
        ..Default::default()
    }
}

fn config_with_dispatchable_agent(agent: AliasedAgentConfig) -> Config {
    let mut cfg = Config::default();
    cfg.agents.insert("a".to_string(), agent);
    cfg
}

/// Structural guard against a *sixth* raw `.risk_profile` read showing up
/// in this file (#21: `agents.<alias>.risk_profile` is forced empty for
/// every carded agent, so any comparison or gate reading it directly
/// instead of resolving through the card is silently wrong for carded
/// agents). Fixed instances so far: `resolved_risk_profile_alias` /
/// `risk_profile_for_agent` (the resolver itself), the `validate()`
/// dangling-reference and card-exclusivity checks (which must inspect
/// the raw field — that is what they are validating),
/// `reachable_delegate_target_configs`, and the sixth instance,
/// `AliasedAgentConfig::is_dispatchable` — that method read the raw
/// field directly and reported every valid carded agent as
/// undispatchable; it has been deleted (its only callers were the two
/// `acp_server.rs` RPC call sites, both now switched to
/// `Config::agent_is_dispatchable`, which routes through
/// `resolved_risk_profile_alias`).
///
/// This re-reads this file's own production source (everything before
/// `mod tests`, so this doc comment's own mentions of the field name
/// can't trip itself) and fails if a `.risk_profile` dot-access (the
/// singular field, not the `.risk_profiles` map) appears inside any
/// function other than the allowlisted ones below. Reverting
/// `reachable_delegate_target_configs` back to `caller.risk_profile.trim()`
/// / `agent.risk_profile.trim()` turns this red; so does adding a new
/// raw read anywhere else in the file's production code.
///
/// Known limit (cold review of 3a3e2a92f): the matcher is line-local,
/// so a read split across lines (`agent.` newline `risk_profile`)
/// slips past it. This is a text scan standing in for a visibility
/// boundary, not a compiler — it catches the shape people actually
/// write, and issue #21's stronger options (rename the field, or make
/// it private) remain the real fix if evasion ever becomes a concern.
#[test]
async fn no_new_raw_risk_profile_reads_outside_the_resolver_and_validation() {
    const SRC: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/schema.rs"));
    let production_src = SRC.split("mod tests").next().unwrap_or(SRC);

    // Functions that legitimately read `agent.risk_profile` (or, for the
    // resolver, `cards[card].risk_profile`) directly:
    //   - `resolved_risk_profile_alias` / `risk_profile_for_agent`: the
    //     single resolution helper this whole guard exists to funnel
    //     every other caller through.
    //   - `validate`: the dangling-reference and card-exclusivity checks
    //     are validating the raw field itself (e.g. "is it set alongside
    //     a card", "does it point at a real risk_profiles entry"); they
    //     must see the unresolved value, not the card-following one.
    const ALLOWED_FNS: &[&str] = &[
        "resolved_risk_profile_alias",
        "risk_profile_for_agent",
        "validate",
    ];

    let mut current_fn: Option<&str> = None;
    let mut offenders = Vec::new();

    for (i, line) in production_src.lines().enumerate() {
        if let Some(name) = fn_name_declared_on(line) {
            current_fn = Some(name);
        }

        if line.trim_start().starts_with("//") {
            continue; // doc comments and line comments never gate anything
        }

        if line_reads_raw_risk_profile_field(line)
            && !current_fn.is_some_and(|f| ALLOWED_FNS.contains(&f))
        {
            offenders.push(format!(
                "line {} (fn {:?}): {}",
                i + 1,
                current_fn,
                line.trim()
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "found a raw `.risk_profile` field read outside the allowlisted resolver/\
         validation functions {ALLOWED_FNS:?} — carded agents force this field \
         empty, so gating or comparing on it directly is silently wrong for them; \
         route through `Config::resolved_risk_profile_alias` instead:\n{}",
        offenders.join("\n")
    );

    /// The function name a top-level `fn` (or `pub fn`, `async fn`,
    /// `pub(crate) fn`, `pub async fn`, ...) declaration line introduces,
    /// if this line is one.
    fn fn_name_declared_on(line: &str) -> Option<&str> {
        let trimmed = line.trim_start();
        let after_fn = trimmed
            .strip_prefix("pub(crate) async fn ")
            .or_else(|| trimmed.strip_prefix("pub(crate) fn "))
            .or_else(|| trimmed.strip_prefix("pub async fn "))
            .or_else(|| trimmed.strip_prefix("pub fn "))
            .or_else(|| trimmed.strip_prefix("async fn "))
            .or_else(|| trimmed.strip_prefix("fn "))?;
        let end = after_fn
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(after_fn.len());
        if end == 0 {
            None
        } else {
            Some(&after_fn[..end])
        }
    }

    /// Whether `line` dot-accesses the singular `risk_profile` field
    /// (`.risk_profile` not immediately followed by another identifier
    /// character — which rules out `.risk_profiles` the map and
    /// `.risk_profile_for_agent(...)`/`.risk_profile_declared` etc., were
    /// such names ever added).
    fn line_reads_raw_risk_profile_field(line: &str) -> bool {
        const NEEDLE: &str = ".risk_profile";
        let mut search_from = 0;
        while let Some(idx) = line[search_from..].find(NEEDLE) {
            let abs = search_from + idx;
            let after = abs + NEEDLE.len();
            let boundary = line[after..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            if boundary {
                return true;
            }
            search_from = after;
        }
        false
    }
}

#[test]
async fn restricted_device_bridge_cannot_coexist_with_anonymous_owner_chat() {
    let mut config = Config::default();
    config.gateway.require_pairing = false;
    config.gateway.bridges.insert(
        "device".into(),
        GatewayBridgeConfig {
            token_hash: "0f".repeat(32),
            owner: None,
            device: Some(GatewayDeviceBridgeConfig::default()),
            ..Default::default()
        },
    );
    let err = config.validate().unwrap_err().to_string();
    assert!(err.contains("gateway.require_pairing"), "{err}");
}

#[test]
async fn gateway_unauthenticated_public_exemption_is_explicit_and_roundtrips() {
    let defaults: GatewayConfig = toml::from_str("").unwrap();
    assert!(!defaults.dangerously_allow_unauthenticated_public_bind);
    let container: GatewayConfig =
        toml::from_str("host = '[::]'\nallow_public_bind = true\nrequire_pairing = false").unwrap();
    assert!(!container.dangerously_allow_unauthenticated_public_bind);
    let opted_in: GatewayConfig =
        toml::from_str("dangerously_allow_unauthenticated_public_bind = true").unwrap();
    assert!(opted_in.dangerously_allow_unauthenticated_public_bind);
    let roundtrip: GatewayConfig = toml::from_str(&toml::to_string(&opted_in).unwrap()).unwrap();
    assert!(roundtrip.dangerously_allow_unauthenticated_public_bind);
}

#[::core::prelude::v1::test]
fn http_auth_secret_policy_preserves_missing_empty_and_scoped_hosts() {
    for (policy, expected) in [
        ("", None),
        ("allowed_hosts = []", Some(Vec::<String>::new())),
        (
            "allowed_hosts = [\"api.example.com\"]",
            Some(vec!["api.example.com".to_owned()]),
        ),
    ] {
        let text = format!("[http_request.auth_secret_policies.api_token]\n{policy}\n");
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            config.http_request.auth_secret_policies["api_token"].allowed_hosts,
            expected
        );
        let serialized = toml::to_string(&config).unwrap();
        let roundtrip: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(
            roundtrip.http_request.auth_secret_policies["api_token"].allowed_hosts,
            expected
        );
    }
    let legacy: Config =
        toml::from_str("[http_request.secrets]\napi_token = \"Bearer fixture\"\n").unwrap();
    assert!(legacy.http_request.auth_secret_policies.is_empty());
    assert!(
        toml::from_str::<Config>(
            "[http_request.auth_secret_policies.api_token]\nallowed_host = [\"api.example.com\"]\n"
        )
        .is_err()
    );
}

#[test]
#[allow(clippy::large_futures)]
async fn load_or_init_rejects_retired_model_provider_without_rewriting_file() {
    let _env_guard = env_override_lock().await;
    let home = TempDir::new().unwrap();
    let config_dir = home.path().join("profile");
    fs::create_dir_all(&config_dir).await.unwrap();
    let config_path = config_dir.join("config.toml");
    let _home = EnvValueGuard::set("HOME", home.path());
    let _config_dir = EnvValueGuard::set("ZEROCLAW_CONFIG_DIR", &config_dir);
    let _workspace = EnvValueGuard::remove("ZEROCLAW_WORKSPACE");
    for family in crate::providers::RETIRED_MODEL_PROVIDER_NAMES {
        let raw = format!(
            "schema_version = 3\n[providers.models.{family}.private_alias]\napi_key = \"private-test-key\"\n"
        );
        fs::write(&config_path, &raw).await.unwrap();
        let error = Box::pin(Config::load_or_init())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("is retired"), "{family}: {error}");
        assert!(!error.contains("private-test-key"));
        assert!(!error.contains("private_alias"));
        assert_eq!(fs::read_to_string(&config_path).await.unwrap(), raw);
    }
}
