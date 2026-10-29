//! V0.8.0 env-var override mechanism.

use crate::alias_refs::CreateError;
use crate::schema::Config;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

const PREFIX: &str = "ZEROCLAW_";
const SEP: &str = "__";

/// `[todotracker]` was a daemon schema section in v0.8.3 only; TodoWrite
/// display config now lives in ZeroCode's `zerocode-config.toml`. Removing the
/// schema section would make these previously valid env vars unresolved, and
/// an unresolved path is a hard error — so a working deployment would stop
/// starting after an upgrade. Recognized legacy fields are therefore accepted
/// and ignored (with a migration warning) instead of failing startup.
///
/// This is an exact allowlist: an unknown `ZEROCLAW_todotracker__*` field is
/// still a hard error, so genuine typos keep surfacing.
const LEGACY_TODOTRACKER_FIELDS: [&str; 5] = [
    "enabled",
    "enabled_at_start",
    "location",
    "width",
    "max_height",
];

/// True when `tail` names a recognized legacy `[todotracker]` field that must
/// no longer block daemon startup.
fn is_recognized_legacy_todotracker(tail: &str) -> bool {
    tail.strip_prefix("todotracker")
        .and_then(|rest| rest.strip_prefix(SEP))
        .is_some_and(|field| LEGACY_TODOTRACKER_FIELDS.contains(&field))
}

static NON_OVERRIDABLE_PATHS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["schema_version"]));

/// A new agent the environment named that config load refused to create.
/// None of its `ZEROCLAW_agents__<alias>__*` variables is applied, so the
/// loaded config has no such agent; the daemon still starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithheldAgentOverride {
    pub reason: String,
    /// Every env var that named the alias, in application order.
    pub env_vars: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct AppliedOverrides {
    pub paths: HashSet<String>,
    pub snapshots: HashMap<String, String>,
    /// New agents named by the environment that load withheld, by alias.
    pub withheld_agents: BTreeMap<String, WithheldAgentOverride>,
}

/// Apply every `ZEROCLAW_<lowercase>` env var to `config`. Returns the set of
/// dotted prop-paths that were overridden plus the pre-override raw values
/// for each. Hard-errors on any env var that doesn't resolve to a known
/// schema path or whose alias fails validation.
///
/// A variable that would create a new `agents` entry is admitted first: the
/// reserved `default` agent is never created from the environment. A refusal
/// does not fail the load: the alias and every variable naming it are
/// withheld, reported in [`AppliedOverrides::withheld_agents`] and as a `WARN`
/// with `error_key` `config.env_agent_override_refused`. `admit_env_agent`
/// says why the retired-knowledge rule of the other creation paths does not
/// apply at load.
pub fn apply_env_overrides(config: &mut Config) -> Result<AppliedOverrides> {
    let mut entries: Vec<(String, String, String)> = std::env::vars()
        .filter_map(|(k, v)| {
            let tail = k.strip_prefix(PREFIX)?;
            (!tail.is_empty()
                && tail
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .then(|| (k.clone(), v, tail.to_string()))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut paths: HashSet<String> = HashSet::with_capacity(entries.len());
    let mut snapshots: HashMap<String, String> = HashMap::with_capacity(entries.len());
    let mut withheld_agents: BTreeMap<String, WithheldAgentOverride> = BTreeMap::new();
    for (env_name, value, tail) in entries {
        // Recognized legacy `[todotracker]` vars are accepted and ignored so an
        // upgrade cannot turn a previously working deployment into a daemon
        // that refuses to start. The value has no effect: TodoWrite display is
        // now owned by ZeroCode's `zerocode-config.toml`.
        if is_recognized_legacy_todotracker(&tail) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"env_var": env_name})),
                "ignoring removed [todotracker] env override; move this setting into zerocode-config.toml"
            );
            continue;
        }
        if let Some(alias) = env_agent_alias(&tail)
            && let Some(withheld) = withheld_agents.get_mut(alias)
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_attrs(::serde_json::json!({"env_var": env_name, "alias": alias})),
                "env override withheld with its refused agent"
            );
            withheld.env_vars.push(env_name);
            continue;
        }
        let path = match resolve_path(&tail, config)
            .with_context(|| format!("{env_name} did not resolve to a schema path"))?
        {
            Resolved::Path(path) => path,
            Resolved::WithheldAgent { alias, reason } => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "error_key": "config.env_agent_override_refused",
                            "env_var": env_name,
                            "alias": alias,
                            "reason": reason,
                        })),
                    "env override withheld: it would create an agent config load refuses; the daemon starts without this agent"
                );
                withheld_agents.insert(
                    alias,
                    WithheldAgentOverride {
                        reason,
                        env_vars: vec![env_name],
                    },
                );
                continue;
            }
        };
        if NON_OVERRIDABLE_PATHS.contains(path.as_str()) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"env_var": env_name, "path": path})),
                "env override rejected: field is not overridable"
            );
            anyhow::bail!("{env_name} -> {path}: this field is not overridable via env vars");
        }
        // Snapshot the pre-override raw value via TOML serde walk. Bypasses
        // `Config::get_prop`'s unconditional secret mask: secret fields on
        // `config` carry plaintext (post-`decrypt_secrets`), so the snapshot
        // captures the real value that should be restored at save time.
        let snapshot = raw_value_for_path(config, &path).unwrap_or_default();
        snapshots.insert(path.clone(), snapshot);

        config
            .set_prop(&path, &value)
            .with_context(|| format!("{env_name} → {path}"))?;
        if Config::prop_is_secret(&path) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"path": path, "env_var": env_name})),
                "Secret applied from env override"
            );
        } else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"path": path, "env_var": env_name})),
                "Env override applied"
            );
        }
        paths.insert(path);
    }
    if !paths.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"count": paths.len()})),
            "Applied env-var config overrides"
        );
    }
    Ok(AppliedOverrides {
        paths,
        snapshots,
        withheld_agents,
    })
}

/// The alias an `agents__<alias>...` env tail names, if it names one.
fn env_agent_alias(tail: &str) -> Option<&str> {
    let rest = tail.strip_prefix("agents")?.strip_prefix(SEP)?;
    rest.split(SEP).next().filter(|alias| !alias.is_empty())
}

/// What an env-var tail resolved to.
enum Resolved {
    /// The dotted prop-path to set.
    Path(String),
    /// The tail would create a new agent that config load refuses; nothing
    /// was created.
    WithheldAgent { alias: String, reason: String },
}

/// Admission for an `agents` key the environment would create. `None`
/// admits: the section is not `agents`, the key already exists, or the alias
/// is allowed. `Some(..)` withholds the alias.
///
/// Only the reserved-alias rule applies here. The retired-knowledge rule that
/// explicit creation and property-path auto-creation share
/// (`alias_refs::admit_map_key`) refuses an alias absent from config whose
/// knowledge rows remain, because in those paths absence means an agent is
/// being created or recreated. At load, an agent the environment defines is
/// absent from the file on every boot and legitimately owns the knowledge it
/// captured in earlier runs, and no durable record distinguishes that from a
/// retired alias whose purge failed. Applying the rule here would withhold
/// every env-defined agent after its first run. Retirement evidence durable
/// enough for load-time admission is separate work.
///
/// A malformed alias is left to `create_map_key`, whose validator message the
/// caller propagates as a hard error, as before.
fn admit_env_agent(config: &Config, section: &str, alias: &str) -> Option<Resolved> {
    if section != "agents"
        || !crate::alias_refs::is_reserved_agent_alias(alias)
        || config
            .get_map_keys(section)
            .is_some_and(|keys| keys.iter().any(|key| key == alias))
    {
        return None;
    }
    Some(Resolved::WithheldAgent {
        alias: alias.to_string(),
        reason: CreateError::Reserved(alias.to_string()).to_string(),
    })
}

/// Walk an env-var tail against the schema. Map-keyed positions consume one
/// `__`-delimited alias token (which may contain single `_` per the alias
/// validator); everything else resolves via `prop_fields()` lookup.
fn resolve_path(tail: &str, config: &mut Config) -> Result<Resolved> {
    let mut sections = Config::map_key_sections();
    sections.sort_by_key(|s| std::cmp::Reverse(s.path.len()));
    for section in sections {
        let env_pfx: String = section.path.replace('.', SEP);
        let with_sep = format!("{env_pfx}{SEP}");
        let Some(rest) = tail.strip_prefix(&with_sep) else {
            continue;
        };
        let mut parts = rest.splitn(2, SEP);
        let alias = parts.next().filter(|s| !s.is_empty()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"section": section.path, "tail": tail})),
                "env override path missing alias segment"
            );
            anyhow::Error::msg(format!("missing alias after `{}`", section.path))
        })?;
        let inner = parts.next().unwrap_or("");
        if let Some(withheld) = admit_env_agent(config, section.path, alias) {
            return Ok(withheld);
        }
        // Propagate the alias-validator's specific error so operators see
        // *why* their alias was rejected (leading underscore, uppercase, …)
        // instead of the generic "Unknown property" that would surface from
        // a downstream `set_prop` against a non-existent map key.
        config.create_map_key(section.path, alias).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "section": section.path,
                        "alias": alias,
                        "error": format!("{}", e),
                    })),
                "env override alias rejected by validator"
            );
            anyhow::Error::msg(format!(
                "invalid alias `{alias}` for `{}`: {e}",
                section.path
            ))
        })?;
        let path = if inner.is_empty() {
            format!("{}.{}", section.path, alias)
        } else {
            // Inner segments are `__`-separated snake-case field names — the
            // same casing the prop-path uses, so join them verbatim.
            let inner_path = inner.split(SEP).collect::<Vec<_>>().join(".");
            format!("{}.{}.{}", section.path, alias, inner_path)
        };
        return Ok(Resolved::Path(path));
    }

    // Non-map path: prop_fields() entries are dotted snake-case field
    // names. Convert to env-form (`.` → `__`) and compare.
    config
        .prop_fields()
        .into_iter()
        .find(|f| f.name.replace('.', SEP) == tail)
        .map(|f| Resolved::Path(f.name))
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tail": tail})),
                "env override path does not match any schema field"
            );
            anyhow::Error::msg(format!("no schema field has env-form `{tail}`"))
        })
}

pub(crate) fn raw_value_for_path(source: &Config, path: &str) -> Option<String> {
    let table = toml::Value::try_from(source).ok()?;
    let mut current: &toml::Value = &table;
    for segment in path.split('.') {
        let tbl = current.as_table()?;
        current = match tbl.get(segment) {
            Some(v) => v,
            None => tbl.get(&segment.replace('-', "_"))?,
        };
    }
    Some(match current {
        toml::Value::String(s) => s.clone(),
        // `set_prop` accepts raw strings for scalar fields, but structured
        // fields must be restored in the same JSON grammar used by the
        // override setter. TOML inline tables are not valid input there.
        other => serde_json::to_string(other).ok()?,
    })
}

pub fn mask_env_overrides_for_save(
    config_to_save: &mut Config,
    snapshots: &HashMap<String, String>,
) -> Result<()> {
    for (path, value) in snapshots {
        config_to_save
            .set_prop(path, value)
            .with_context(|| format!("failed to restore env-overridden path {path} before save"))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn env_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multi_agent::MemoryGrant;
    use crate::schema::{AliasedAgentConfig, Config};

    struct EnvVarGuard(&'static str);
    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            // SAFETY: tests serialize on `env_test_lock()`.
            unsafe { std::env::set_var(name, value) };
            Self(name)
        }
    }
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: tests serialize on `env_test_lock()`.
            unsafe { std::env::remove_var(self.0) };
        }
    }

    #[tokio::test]
    async fn walker_resolves_typed_family_alias_default() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set(
            "ZEROCLAW_providers__models__anthropic__default__api_key",
            "sk-ant-fixture",
        );

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(
            applied
                .paths
                .contains("providers.models.anthropic.default.api_key"),
            "kebab-translated path should be recorded: {:?}",
            applied.paths,
        );
        // Secret field round-trips through set_prop into the typed alias.
        assert_eq!(
            config
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-ant-fixture"),
        );
    }

    #[tokio::test]
    async fn walker_resolves_grok_cli_stdout_limit() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set(
            "ZEROCLAW_providers__models__grok_cli__default__max_acp_stdout_bytes",
            "8388608",
        );

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(
            applied
                .paths
                .contains("providers.models.grok_cli.default.max_acp_stdout_bytes"),
        );
        assert_eq!(
            config
                .providers
                .models
                .grok_cli
                .get("default")
                .and_then(|entry| entry.max_acp_stdout_bytes),
            Some(8_388_608)
        );
    }

    #[tokio::test]
    async fn walker_accepts_alias_with_underscore() {
        let _guard = super::env_test_lock().await;
        let _v1 = EnvVarGuard::set(
            "ZEROCLAW_providers__models__openrouter__prod_v2__api_key",
            "sk-or-fixture",
        );
        let _v2 = EnvVarGuard::set(
            "ZEROCLAW_providers__models__openrouter__prod_v2__model",
            "anthropic/claude-sonnet-4-6",
        );

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(
            applied
                .paths
                .contains("providers.models.openrouter.prod_v2.api_key"),
        );
        assert!(
            applied
                .paths
                .contains("providers.models.openrouter.prod_v2.model"),
        );
        let entry = config
            .providers
            .models
            .openrouter
            .get("prod_v2")
            .expect("alias created");
        assert_eq!(entry.base.api_key.as_deref(), Some("sk-or-fixture"));
        assert_eq!(
            entry.base.model.as_deref(),
            Some("anthropic/claude-sonnet-4-6"),
        );
    }

    #[tokio::test]
    async fn walker_preserves_legacy_memory_grant_input_and_mixed_json() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set(
            "ZEROCLAW_agents__alpha__workspace__read_memory_from",
            r#"["beta", {"agent":"gamma", "categories":["facts"]}]"#,
        );

        let mut config = Config::default();
        config
            .agents
            .insert("alpha".into(), AliasedAgentConfig::default());
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(
            applied
                .paths
                .contains("agents.alpha.workspace.read_memory_from")
        );
        let grants = &config.agents["alpha"].workspace.read_memory_from;
        assert_eq!(grants.len(), 2);
        assert!(matches!(grants[0], MemoryGrant::Agent(_)));
        assert_eq!(grants[1].as_str(), "gamma");
        assert_eq!(
            grants[1].categories(),
            Some(["facts".to_string()].as_slice())
        );
    }

    #[tokio::test]
    async fn walker_resolves_non_map_gateway_path() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_gateway__request_timeout_secs", "120");

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(applied.paths.contains("gateway.request_timeout_secs"));
        assert_eq!(config.gateway.request_timeout_secs, 120);
    }

    #[tokio::test]
    async fn walker_rejects_unknown_path() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_no__such__field", "x");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ZEROCLAW_no__such__field") && msg.contains("did not resolve"),
            "error must name the env var and the failure: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_propagates_alias_validator_error() {
        let _guard = super::env_test_lock().await;
        // `_invalid` starts with `_`, which the alias validator rejects.
        // The walker's tail filter accepts `[a-z0-9_]+` so this gets past
        // the prefilter, and the failure must surface as the validator's
        // specific message — not a generic "Unknown property".
        let _v = EnvVarGuard::set(
            "ZEROCLAW_providers__models__anthropic___invalid__api_key",
            "x",
        );

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid alias") && msg.contains("_invalid"),
            "error must surface the alias validator's message: {msg}",
        );
    }

    /// Seed a knowledge store with the `owner_agent` column the admission
    /// probe reads, holding one node per owner in `owners`.
    fn seed_knowledge(path: &std::path::Path, owners: &[&str]) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE nodes (owner_agent TEXT); CREATE TABLE edges (owner_agent TEXT);",
        )
        .unwrap();
        for owner in owners {
            conn.execute("INSERT INTO nodes VALUES (?1)", [owner])
                .unwrap();
        }
        conn
    }

    /// The reserved `default` agent is never created from the environment:
    /// every variable naming it is withheld and reported, the load still
    /// succeeds, and the rest of the environment applies. Once the entry
    /// exists (a migration synthesizes it) the same variables apply to it.
    /// A malformed agent alias still fails the load with the validator's
    /// message.
    #[tokio::test]
    async fn walker_withholds_reserved_default_agent() {
        let _guard = super::env_test_lock().await;
        let _default_enabled = EnvVarGuard::set("ZEROCLAW_agents__default__enabled", "false");
        let _default_grant = EnvVarGuard::set(
            "ZEROCLAW_agents__default__workspace__read_memory_from",
            r#"["alpha"]"#,
        );
        let _alpha_enabled = EnvVarGuard::set("ZEROCLAW_agents__alpha__enabled", "false");
        let _timeout = EnvVarGuard::set("ZEROCLAW_gateway__request_timeout_secs", "120");

        let mut config = Config::default();
        config
            .agents
            .insert("alpha".into(), AliasedAgentConfig::default());
        let applied =
            apply_env_overrides(&mut config).expect("a withheld agent must not fail the load");

        assert!(
            !config.agents.contains_key("default"),
            "the reserved agent must not be created"
        );
        let withheld = applied
            .withheld_agents
            .get("default")
            .expect("the withheld alias is reported");
        assert!(withheld.reason.contains("reserved"), "{}", withheld.reason);
        assert_eq!(
            withheld.env_vars,
            [
                "ZEROCLAW_agents__default__enabled",
                "ZEROCLAW_agents__default__workspace__read_memory_from",
            ]
        );
        assert!(
            !applied
                .paths
                .iter()
                .any(|path| path.starts_with("agents.default.")),
            "{:?}",
            applied.paths
        );
        assert!(
            !applied
                .snapshots
                .keys()
                .any(|path| path.starts_with("agents.default.")),
            "a withheld variable must not be masked at save"
        );
        assert!(applied.paths.contains("agents.alpha.enabled"));
        assert!(!config.agents["alpha"].enabled);
        assert_eq!(config.gateway.request_timeout_secs, 120);

        let mut config = Config::default();
        config
            .agents
            .insert("default".into(), AliasedAgentConfig::default());
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");
        assert!(applied.withheld_agents.is_empty());
        assert!(applied.paths.contains("agents.default.enabled"));
        assert!(!config.agents["default"].enabled);
        assert_eq!(config.agents["default"].workspace.read_memory_from.len(), 1);

        let _bad = EnvVarGuard::set("ZEROCLAW_agents___bad__enabled", "true");
        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid alias") && msg.contains("_bad"),
            "a malformed alias must fail with the validator's message: {msg}",
        );
    }

    /// An agent the environment defines is live. It is absent from the file
    /// on every boot and owns the knowledge it captured in earlier runs, so
    /// load admits it; the retired-knowledge rule of explicit creation would
    /// otherwise withhold every env-defined agent after its first run.
    #[tokio::test]
    async fn walker_admits_env_defined_agent_that_owns_knowledge() {
        let _guard = super::env_test_lock().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let graph = tmp.path().join("knowledge.db");
        let _conn = seed_knowledge(&graph, &["envonly"]);
        let _envonly = EnvVarGuard::set("ZEROCLAW_agents__envonly__enabled", "true");

        let mut config = Config::default();
        config.knowledge.db_path = graph.to_string_lossy().into_owned();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(
            applied.withheld_agents.is_empty(),
            "{:?}",
            applied.withheld_agents
        );
        assert!(config.agents["envonly"].enabled);
        assert!(applied.paths.contains("agents.envonly.enabled"));
    }

    #[tokio::test]
    async fn mask_restores_pre_override_snapshot_for_non_secret() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_gateway__request_timeout_secs", "999");

        let mut config = Config::default();
        let original_timeout = config.gateway.request_timeout_secs;
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");
        assert_eq!(config.gateway.request_timeout_secs, 999);

        let mut to_save = config.clone();
        mask_env_overrides_for_save(&mut to_save, &applied.snapshots).expect("mask succeeds");
        assert_eq!(
            to_save.gateway.request_timeout_secs, original_timeout,
            "non-secret path resets to pre-override snapshot",
        );
        // In-memory config is unchanged — env value still effective for the
        // running process.
        assert_eq!(config.gateway.request_timeout_secs, 999);
    }

    #[tokio::test]
    async fn mask_restores_pre_override_plaintext_for_secret() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set(
            "ZEROCLAW_providers__models__anthropic__default__api_key",
            "sk-ant-from-env",
        );

        // Pre-existing alias with a real plaintext credential (the state
        // after `Config::load_or_init` calls `decrypt_secrets`).
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("anthropic", "default")
            .expect("typed slot")
            .api_key = Some("sk-ant-on-disk".to_string());

        let applied = apply_env_overrides(&mut config).expect("apply succeeds");
        assert!(
            applied
                .paths
                .contains("providers.models.anthropic.default.api_key"),
        );
        // Env value is live in memory.
        assert_eq!(
            config
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-ant-from-env"),
        );

        // Save-bound clone restores the pre-override plaintext, NOT the
        // display mask. This is the regression bar for the data-loss bug
        // identified inreview.
        let mut to_save = config.clone();
        mask_env_overrides_for_save(&mut to_save, &applied.snapshots).expect("mask succeeds");
        assert_eq!(
            to_save
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-ant-on-disk"),
            "secret resets to pre-override plaintext (not the `**** (encrypted)` mask)",
        );
        assert_ne!(
            to_save
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("**** (encrypted)"),
            "must not corrupt the field with the display mask",
        );
    }

    #[tokio::test]
    async fn mask_restores_structured_memory_grant_snapshot() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set(
            "ZEROCLAW_agents__alpha__workspace__read_memory_from",
            "beta",
        );

        let mut config = Config::default();
        for alias in ["alpha", "beta", "gamma"] {
            config
                .agents
                .insert(alias.to_string(), AliasedAgentConfig::default());
        }
        config
            .agents
            .get_mut("alpha")
            .expect("alpha agent")
            .workspace
            .read_memory_from = vec![MemoryGrant::Scoped {
            agent: crate::multi_agent::AgentAlias::new("gamma"),
            categories: Some(vec!["facts".to_string()]),
        }];

        let applied = apply_env_overrides(&mut config).expect("apply succeeds");
        let path = "agents.alpha.workspace.read_memory_from";
        assert!(applied.snapshots[path].starts_with('['));

        let mut to_save = config.clone();
        mask_env_overrides_for_save(&mut to_save, &applied.snapshots)
            .expect("structured snapshot restores");
        assert_eq!(
            to_save.agents["alpha"].workspace.read_memory_from,
            vec![MemoryGrant::Scoped {
                agent: crate::multi_agent::AgentAlias::new("gamma"),
                categories: Some(vec!["facts".to_string()]),
            }]
        );
    }

    #[tokio::test]
    async fn schema_version_override_rejected() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_schema_version", "99");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("schema_version") && msg.contains("not overridable"),
            "error must name the path and the reason: {msg}",
        );
    }
    // ── Legacy `[todotracker]` compatibility ────────────────────────────────
    //
    // The section moved out of the daemon schema into ZeroCode's
    // `zerocode-config.toml`. Recognized legacy env vars must not become
    // unresolved paths, because an unresolved path is fatal and would stop a
    // previously working daemon from starting after an upgrade.

    #[tokio::test]
    async fn recognized_legacy_todotracker_env_does_not_block_startup() {
        let _guard = super::env_test_lock().await;
        let _a = EnvVarGuard::set("ZEROCLAW_todotracker__enabled", "false");
        let _b = EnvVarGuard::set("ZEROCLAW_todotracker__enabled_at_start", "true");
        let _c = EnvVarGuard::set("ZEROCLAW_todotracker__location", "left");
        let _d = EnvVarGuard::set("ZEROCLAW_todotracker__width", "40");
        let _e = EnvVarGuard::set("ZEROCLAW_todotracker__max_height", "8");

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config)
            .expect("recognized legacy todotracker vars must not fail daemon startup");

        // Accepted-and-ignored: no schema path is touched by these vars.
        assert!(
            !applied.paths.iter().any(|p| p.starts_with("todotracker")),
            "legacy vars must not resolve to a schema path: {:?}",
            applied.paths,
        );
    }

    #[tokio::test]
    async fn unknown_legacy_todotracker_field_still_errors() {
        let _guard = super::env_test_lock().await;
        // Not on the exact allowlist: a typo must still surface loudly rather
        // than being silently swallowed by the compatibility shim.
        let _v = EnvVarGuard::set("ZEROCLAW_todotracker__wdith", "40");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("unknown field must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ZEROCLAW_todotracker__wdith") && msg.contains("did not resolve"),
            "error must name the env var and the failure: {msg}",
        );
    }

    #[tokio::test]
    async fn legacy_shim_does_not_shadow_other_sections() {
        let _guard = super::env_test_lock().await;
        // A section that merely *starts with* the legacy name must not be
        // captured by the allowlist prefix check.
        let _v = EnvVarGuard::set("ZEROCLAW_todotracker_extra__enabled", "false");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        assert!(format!("{err:#}").contains("did not resolve"));
    }
}
