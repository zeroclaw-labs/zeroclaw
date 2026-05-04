//! V2 schema partial typed lens for V2 → V3 migration.
//!
//! Frozen after V3 ships. Explicit fields only for top-level sections that
//! transform between V2 and V3; everything else rides through `passthrough`.
//!
//! V2 → V3 transformation inventory (ground truth: `git show 68a875b5b:crates/zeroclaw-config/src/schema.rs`
//! and current branch HEAD):
//!
//! - **`autonomy` removed** → synthesized into `risk_profiles.default`
//! - **`agent` removed** → synthesized into `runtime_profiles.default`
//! - **`swarms` removed** → dropped (RFC #6271 follow-up)
//! - **`cron` type-changed**: V2 `CronConfig` `{enabled, catch_up_on_startup, max_run_history, jobs}`
//!   → V3 `cron: HashMap<String, CronJobDecl>` (alias-keyed); subsystem knobs move to `[scheduler]`
//! - **`providers.fallback` eradicated**
//! - **`providers.models` flat → aliased**: V2 `HashMap<id, ModelProviderConfig>`
//!   → V3 `HashMap<provider_type, HashMap<alias, ModelProviderConfig>>`
//! - **`cost.prices` removed** → folded into `providers.models.<type>.<alias>.pricing` inline
//! - **`channels.<type>` shape**: V2 `Option<T>` → V3 `HashMap<String, T>` (channel aliasing)
//! - **`channels.discord_history` removed** → folded into `channels.discord.<alias>.archive = true`
//! - **`agents.<id>` inline brain fields** (`provider`, `model`, `temperature`, `api_key`)
//!   → synthesized into `providers.models.<provider>.agent_<id>` and replaced with
//!   `model_provider = "<provider>.agent_<id>"` alias reference

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// V2 partial typed lens. Everything not explicitly named flows through
/// `passthrough` unchanged.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct V2Config {
    #[serde(default = "default_v2_schema_version")]
    pub schema_version: u32,

    /// V3 synthesizes `risk_profiles` from this block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<toml::Value>,

    /// V3 synthesizes `runtime_profiles` from this block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<toml::Value>,

    /// V3 drops swarms (out-of-scope per #6271).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub swarms: HashMap<String, toml::Value>,

    /// V3 restructures cron: `[cron.<alias>] = CronJobDecl`; subsystem knobs
    /// (`enabled`, `catch_up_on_startup`, `max_run_history`) move to `[scheduler]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<toml::Value>,

    /// V3 restructures providers: drops `fallback`, aliases `models`, adds `tts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<toml::Value>,

    /// V3 drops `cost.prices`; pricing moves inline onto each model provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<toml::Value>,

    /// V3 wraps each channel section in `HashMap<String, T>` (alias-keyed) and
    /// folds `discord_history` into `discord.<alias>.archive = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<toml::Value>,

    /// V3 replaces inline brain fields on each agent with model-provider
    /// alias references; brain fields surface as new entries under
    /// `providers.models.<provider>.agent_<id>`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub agents: HashMap<String, toml::Value>,

    /// Everything else passes through unchanged.
    #[serde(flatten)]
    pub passthrough: toml::Table,
}

fn default_v2_schema_version() -> u32 {
    2
}

/// Channel section keys subject to V3 alias-wrapping. Order does not matter
/// for correctness; listed here so missing-from-V2 channel types simply pass
/// through under whatever key the user used (with a debug-log warning).
const V3_CHANNEL_TYPES: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "mattermost",
    "webhook",
    "imessage",
    "matrix",
    "signal",
    "whatsapp",
    "linq",
    "wati",
    "nextcloud_talk",
    "email",
    "gmail_push",
    "irc",
    "lark",
    "line",
    "feishu",
    "dingtalk",
    "wecom",
    "wechat",
    "qq",
    "twitter",
    "mochat",
    "nostr",
    "clawdtalk",
    "reddit",
    "bluesky",
    "voice_call",
];

impl V2Config {
    /// Migrate V2 → V3. Returns a serialized V3-shaped TOML value.
    ///
    /// V3 `Config` is too large to construct field-by-field in Rust without
    /// duplicating its full shape; instead we emit a `toml::Value` that
    /// `migrate_to_current` deserializes into the live `Config` type. The
    /// final deserialize is the runtime gate that catches any structural
    /// mismatch.
    pub fn migrate(self) -> anyhow::Result<toml::Value> {
        let V2Config {
            schema_version: _,
            autonomy,
            agent,
            swarms,
            cron,
            providers,
            cost,
            channels,
            agents,
            mut passthrough,
        } = self;

        // 1. autonomy → risk_profiles.default
        if let Some(autonomy_value) = autonomy {
            let mut risk_profiles = passthrough
                .remove("risk_profiles")
                .and_then(|v| v.try_into::<toml::Table>().ok())
                .unwrap_or_default();
            risk_profiles
                .entry("default".to_string())
                .or_insert(autonomy_value);
            passthrough.insert(
                "risk_profiles".to_string(),
                toml::Value::Table(risk_profiles),
            );
            tracing::info!(target: "migration", "[autonomy] → [risk_profiles.default]");
        }

        // 2. agent → runtime_profiles.default
        if let Some(agent_value) = agent {
            let mut runtime_profiles = passthrough
                .remove("runtime_profiles")
                .and_then(|v| v.try_into::<toml::Table>().ok())
                .unwrap_or_default();
            runtime_profiles
                .entry("default".to_string())
                .or_insert(agent_value);
            passthrough.insert(
                "runtime_profiles".to_string(),
                toml::Value::Table(runtime_profiles),
            );
            tracing::info!(target: "migration", "[agent] → [runtime_profiles.default]");
        }

        // 3. swarms → drop (RFC out-of-scope)
        if !swarms.is_empty() {
            tracing::info!(
                target: "migration",
                "[swarms] dropped ({} entries) — V3 swarm schema follow-up #6271",
                swarms.len()
            );
        }

        // 4. cron → restructure
        if let Some(cron_value) = cron {
            let (new_cron, scheduler_extras) = restructure_cron(cron_value);
            if !new_cron.is_empty() {
                passthrough.insert("cron".to_string(), toml::Value::Table(new_cron));
            }
            if !scheduler_extras.is_empty() {
                merge_into_table(&mut passthrough, "scheduler", scheduler_extras);
            }
            tracing::info!(target: "migration", "[cron] restructured into [cron.<alias>] + [scheduler]");
        }

        // 5. providers → restructure (drop fallback, alias models, fold cost.prices, fold per-agent inline brain)
        let mut new_providers = providers
            .and_then(|v| match v {
                toml::Value::Table(t) => Some(t),
                _ => None,
            })
            .unwrap_or_default();
        if new_providers.remove("fallback").is_some() {
            tracing::info!(target: "migration", "providers.fallback eradicated");
        }
        let mut aliased_models = alias_provider_models(new_providers.remove("models"));

        // 5a. Fold V2 [providers] globals (api_key, default_provider, default_model,
        //     default_temperature, provider_timeout_secs, provider_max_tokens,
        //     extra_headers) onto the V3 per-provider model entry. The V1→V2 step
        //     placed these on `providers` directly; V3 has no equivalent at the
        //     section level — they live inline on each `ModelProviderConfig`.
        fold_providers_globals_into_models(&mut new_providers, &mut aliased_models);

        // 6. cost.prices → providers.models[*].pricing inline
        let cost_passthrough = if let Some(cost_value) = cost {
            let (cost_remaining, prices) = strip_cost_prices(cost_value);
            if !prices.is_empty() {
                fold_pricing_into_models(&mut aliased_models, prices);
                tracing::info!(target: "migration", "[cost.prices] folded into providers.models[*].pricing");
            }
            cost_remaining
        } else {
            None
        };
        if !aliased_models.is_empty() {
            new_providers.insert("models".to_string(), toml::Value::Table(aliased_models));
        }
        if !new_providers.is_empty() {
            passthrough.insert("providers".to_string(), toml::Value::Table(new_providers));
        }
        if let Some(remaining_cost) = cost_passthrough {
            passthrough.insert("cost".to_string(), remaining_cost);
        }

        // 7. channels → alias-wrap each channel type, fold discord_history
        if let Some(channels_value) = channels {
            let new_channels = alias_wrap_channels(channels_value);
            passthrough.insert("channels".to_string(), toml::Value::Table(new_channels));
            tracing::info!(target: "migration", "[channels] sections alias-wrapped, discord_history folded");
        }

        // 8. agents → strip inline brain, synthesize provider aliases
        if !agents.is_empty() {
            let new_agents = synthesize_agent_brains(agents, &mut passthrough);
            passthrough.insert("agents".to_string(), toml::Value::Table(new_agents));
        }

        // 9. set schema_version = 3
        passthrough.insert("schema_version".to_string(), toml::Value::Integer(3));

        Ok(toml::Value::Table(passthrough))
    }
}

/// Split V2 `[cron]` into V3 `[cron.<alias>]` and `[scheduler]` extras.
fn restructure_cron(cron_value: toml::Value) -> (toml::Table, toml::Table) {
    let mut new_cron = toml::Table::new();
    let mut scheduler_extras = toml::Table::new();
    let mut cron_table = match cron_value {
        toml::Value::Table(t) => t,
        _ => return (new_cron, scheduler_extras),
    };

    // V2 had `[[cron.jobs]]` array. Each entry becomes `[cron.<key>]`.
    if let Some(toml::Value::Array(jobs)) = cron_table.remove("jobs") {
        for (i, job) in jobs.into_iter().enumerate() {
            let key = job
                .get("name")
                .and_then(toml::Value::as_str)
                .map(slugify)
                .unwrap_or_else(|| format!("job_{}", i + 1));
            // De-duplicate keys with a numeric suffix.
            let key = ensure_unique_key(&new_cron, key);
            new_cron.insert(key, job);
        }
    }

    // Subsystem knobs move to [scheduler].
    for knob in ["enabled", "catch_up_on_startup", "max_run_history"] {
        if let Some(v) = cron_table.remove(knob) {
            scheduler_extras.insert(knob.to_string(), v);
        }
    }

    // Anything left was unknown to V2 cron; surface but don't drop silently —
    // dropped fields are visible in INFO logs instead.
    if !cron_table.is_empty() {
        tracing::info!(
            target: "migration",
            "[cron] had unmodeled keys: {:?}",
            cron_table.keys().collect::<Vec<_>>()
        );
    }

    (new_cron, scheduler_extras)
}

/// Convert a V2 `providers.models` flat map (`{id => ModelProviderConfig}`)
/// into a V3 aliased map (`{provider_type => {alias => ModelProviderConfig}}`).
///
/// Rules:
/// - `claude-code` standalone → `anthropic.claude-code` (per PR body).
/// - Any other entry `<id>` → `<id>.default` (single alias).
fn alias_provider_models(models: Option<toml::Value>) -> toml::Table {
    let flat = match models {
        Some(toml::Value::Table(t)) => t,
        _ => return toml::Table::new(),
    };
    let mut aliased = toml::Table::new();
    for (provider_id, config) in flat {
        let (provider_type, alias) = if provider_id == "claude-code" {
            ("anthropic".to_string(), "claude-code".to_string())
        } else {
            (provider_id.clone(), "default".to_string())
        };
        let entry = aliased
            .entry(provider_type)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(entry_table) = entry {
            entry_table.insert(alias, config);
        }
    }
    aliased
}

/// Fold V2 `[providers]` global fields (which lived directly on `ProvidersConfig`)
/// onto the V3 per-provider `ModelProviderConfig` entry.
///
/// Field renames applied during the fold:
/// - `api_url` → `base_url` (matches V3 `ModelProviderConfig.base_url`)
/// - `default_model` → `model`
/// - `default_temperature` → `temperature`
/// - `provider_timeout_secs` → `timeout_secs`
/// - `provider_max_tokens` → `max_tokens`
///
/// Target entry resolution:
/// - If `default_provider` is a string and matches a key in `aliased_models`, fold there.
/// - Otherwise, if `aliased_models` already has at least one entry, fold onto its
///   first entry's `default` alias (this matches V1 `[model_providers.<id>]` blocks
///   that had no separate `default_provider` declaration).
/// - Otherwise, synthesize a fresh `<default_provider | "openrouter">.default`
///   entry to hold the globals (matches V1's documented default provider).
///
/// `claude-code` continues to map under `anthropic.claude-code` per the V3 fold.
///
/// Per-provider explicit fields take precedence: globals only fill in missing slots.
fn fold_providers_globals_into_models(
    new_providers: &mut toml::Table,
    aliased_models: &mut toml::Table,
) {
    let g_api_key = new_providers.remove("api_key");
    let g_api_url = new_providers.remove("api_url");
    let g_api_path = new_providers.remove("api_path");
    let g_default_provider = new_providers.remove("default_provider");
    let g_default_model = new_providers.remove("default_model");
    let g_default_temperature = new_providers.remove("default_temperature");
    let g_provider_timeout_secs = new_providers.remove("provider_timeout_secs");
    let g_provider_max_tokens = new_providers.remove("provider_max_tokens");
    let g_extra_headers = new_providers.remove("extra_headers");

    let any_value_globals = g_api_key.is_some()
        || g_api_url.is_some()
        || g_api_path.is_some()
        || g_default_model.is_some()
        || g_default_temperature.is_some()
        || g_provider_timeout_secs.is_some()
        || g_provider_max_tokens.is_some()
        || g_extra_headers.is_some();

    if !any_value_globals && g_default_provider.is_none() {
        return;
    }

    // Determine target (provider_type, alias).
    let (target_type, target_alias) =
        match g_default_provider.as_ref().and_then(toml::Value::as_str) {
            Some("claude-code") => ("anthropic".to_string(), "claude-code".to_string()),
            Some(s) => (s.to_string(), "default".to_string()),
            None => match aliased_models.keys().next() {
                Some(k) => (k.clone(), "default".to_string()),
                None => ("openrouter".to_string(), "default".to_string()),
            },
        };

    let provider_value = aliased_models
        .entry(target_type.clone())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let provider_table = match provider_value.as_table_mut() {
        Some(t) => t,
        None => return,
    };
    let alias_value = provider_table
        .entry(target_alias.clone())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let alias_table = match alias_value.as_table_mut() {
        Some(t) => t,
        None => return,
    };

    // Per-provider entries take precedence: only fill missing slots.
    for (target_key, source) in [
        ("api_key", g_api_key),
        ("base_url", g_api_url),
        ("api_path", g_api_path),
        ("model", g_default_model),
        ("temperature", g_default_temperature),
        ("timeout_secs", g_provider_timeout_secs),
        ("max_tokens", g_provider_max_tokens),
        ("extra_headers", g_extra_headers),
    ] {
        if let Some(value) = source
            && !alias_table.contains_key(target_key)
        {
            alias_table.insert(target_key.to_string(), value);
        }
    }

    if any_value_globals {
        tracing::info!(
            target: "migration",
            "[providers] globals folded onto providers.models.{target_type}.{target_alias}"
        );
    }
}

/// Pull `prices` (a per-model HashMap) out of a V2 `[cost]` block.
/// Returns `(cost_passthrough, prices)`. `prices` keys are model identifiers;
/// values are `ModelPricing` tables.
fn strip_cost_prices(cost_value: toml::Value) -> (Option<toml::Value>, toml::Table) {
    let mut cost_table = match cost_value {
        toml::Value::Table(t) => t,
        other => return (Some(other), toml::Table::new()),
    };
    let prices = match cost_table.remove("prices") {
        Some(toml::Value::Table(p)) => p,
        Some(other) => {
            // Unexpected shape — reinsert and skip the fold.
            cost_table.insert("prices".to_string(), other);
            return (Some(toml::Value::Table(cost_table)), toml::Table::new());
        }
        None => toml::Table::new(),
    };
    let cost_passthrough = if cost_table.is_empty() {
        None
    } else {
        Some(toml::Value::Table(cost_table))
    };
    (cost_passthrough, prices)
}

/// Merge legacy `cost.prices` entries into per-model `pricing` fields on
/// the matching aliased model entries. Best-effort: a price keyed by a
/// model id matches when the model id appears as a `(provider_type, alias)`
/// — typically `(provider_type, "default")`.
///
/// Unmatched entries are logged and dropped, since V3 has no equivalent.
fn fold_pricing_into_models(aliased_models: &mut toml::Table, prices: toml::Table) {
    for (model_id, price) in prices {
        // V2 `cost.prices` was keyed by model id (free-form). V3 `pricing` is
        // a free-form HashMap<String, f64>. We attach the price as
        // `pricing.<model_id>` on the closest matching model entry. If
        // ambiguous, we attach to the first provider whose default alias
        // matches; otherwise we synthesize an `<id>.default` entry.
        let pricing_table = match price {
            toml::Value::Table(t) => t,
            other => {
                tracing::info!(
                    target: "migration",
                    "[cost.prices.{model_id}] unexpected shape ({other:?}); dropped"
                );
                continue;
            }
        };
        let target_provider = aliased_models
            .iter_mut()
            .find_map(|(provider_type, value)| {
                if provider_type == &model_id {
                    Some(value)
                } else {
                    None
                }
            });
        let provider_value = match target_provider {
            Some(v) => v,
            None => aliased_models
                .entry(model_id.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new())),
        };
        let provider_table = match provider_value.as_table_mut() {
            Some(t) => t,
            None => continue,
        };
        let alias_value = provider_table
            .entry("default".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let alias_table = match alias_value.as_table_mut() {
            Some(t) => t,
            None => continue,
        };
        let pricing = alias_table
            .entry("pricing".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(pricing_t) = pricing.as_table_mut() {
            for (k, v) in pricing_table {
                pricing_t.insert(k, v);
            }
        }
    }
}

/// Wrap V2 `Option<T>` channel sections into V3 `HashMap<String, T>` keyed by
/// `"default"`. Folds `discord_history` into `discord.default.archive = true`.
/// Preserves `cli: bool` as-is (V3 keeps it as a bool toggle).
fn alias_wrap_channels(channels_value: toml::Value) -> toml::Table {
    let mut channels_table = match channels_value {
        toml::Value::Table(t) => t,
        _ => return toml::Table::new(),
    };
    let mut new_channels = toml::Table::new();

    // `cli` is a top-level bool, not aliased.
    if let Some(cli) = channels_table.remove("cli") {
        new_channels.insert("cli".to_string(), cli);
    }

    // discord_history → fold into discord.default with archive=true.
    let history = channels_table.remove("discord_history");

    for ct in V3_CHANNEL_TYPES {
        if let Some(value) = channels_table.remove(*ct) {
            let mut wrapped = toml::Table::new();
            wrapped.insert("default".to_string(), value);
            new_channels.insert((*ct).to_string(), toml::Value::Table(wrapped));
        }
    }

    if let Some(history_value) = history {
        let discord_entry = new_channels
            .entry("discord".to_string())
            .or_insert_with(|| {
                let mut wrapped = toml::Table::new();
                wrapped.insert(
                    "default".to_string(),
                    toml::Value::Table(toml::Table::new()),
                );
                toml::Value::Table(wrapped)
            });
        if let Some(discord_table) = discord_entry.as_table_mut() {
            let default_entry = discord_table
                .entry("default".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(default_table) = default_entry.as_table_mut() {
                default_table.insert("archive".to_string(), toml::Value::Boolean(true));
                if let toml::Value::Table(history_table) = history_value {
                    for (k, v) in history_table {
                        // Don't overwrite any explicit discord.default key.
                        default_table.entry(k).or_insert(v);
                    }
                }
            }
        }
        tracing::info!(
            target: "migration",
            "[channels.discord_history] folded into [channels.discord.default] (archive=true)"
        );
    }

    // Any unmodeled channel-section keys: pass through under their original
    // top-level key (no alias-wrap, since V3 may reject them, but better than
    // silently dropping data).
    if !channels_table.is_empty() {
        let leftover_keys: Vec<String> = channels_table.keys().cloned().collect();
        tracing::info!(
            target: "migration",
            "[channels] passthrough for unmodeled keys: {:?}",
            leftover_keys
        );
        for (k, v) in channels_table {
            new_channels.insert(k, v);
        }
    }

    new_channels
}

/// Strip inline brain fields (`provider`, `model`, `temperature`, `api_key`)
/// from each agent. For each agent that had a `provider`, synthesize a new
/// entry under `providers.models.<provider>.agent_<id>` with the brain
/// fields, and replace the agent's brain with `model_provider = "<provider>.agent_<id>"`.
fn synthesize_agent_brains(
    agents: HashMap<String, toml::Value>,
    passthrough: &mut toml::Table,
) -> toml::Table {
    let mut new_agents = toml::Table::new();
    for (alias, agent_value) in agents {
        let mut agent_table = match agent_value {
            toml::Value::Table(t) => t,
            other => {
                new_agents.insert(alias, other);
                continue;
            }
        };
        let provider = agent_table.remove("provider");
        let model = agent_table.remove("model");
        let api_key = agent_table.remove("api_key");
        let temperature = agent_table.remove("temperature");

        if let Some(toml::Value::String(provider_type)) = provider {
            let provider_alias = format!("agent_{}", alias);
            let mut entry = toml::Table::new();
            if let Some(m) = model {
                entry.insert("model".to_string(), m);
            }
            if let Some(k) = api_key {
                entry.insert("api_key".to_string(), k);
            }
            if let Some(t) = temperature {
                entry.insert("temperature".to_string(), t);
            }

            // Insert into providers.models.<provider_type>.<provider_alias>.
            let providers_value = passthrough
                .entry("providers".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(providers_table) = providers_value.as_table_mut() {
                let models_value = providers_table
                    .entry("models".to_string())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let Some(models_table) = models_value.as_table_mut() {
                    let provider_value = models_table
                        .entry(provider_type.clone())
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                    if let Some(provider_table) = provider_value.as_table_mut() {
                        provider_table.insert(provider_alias.clone(), toml::Value::Table(entry));
                    }
                }
            }

            agent_table.insert(
                "model_provider".to_string(),
                toml::Value::String(format!("{provider_type}.{provider_alias}")),
            );
            tracing::info!(
                target: "migration",
                "agents.{alias}: inline brain → providers.models.{provider_type}.{provider_alias}"
            );
        } else if let Some(other) = provider {
            // Non-string provider — preserve as-is.
            agent_table.insert("provider".to_string(), other);
        }

        new_agents.insert(alias, toml::Value::Table(agent_table));
    }
    new_agents
}

/// Insert `(key, value)` pairs from `extras` into a sub-table at `top.<section>`.
/// Creates the sub-table if missing; overwrites individual keys but preserves
/// other existing keys in the section.
fn merge_into_table(top: &mut toml::Table, section: &str, extras: toml::Table) {
    let entry = top
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(section_table) = entry.as_table_mut() {
        for (k, v) in extras {
            section_table.insert(k, v);
        }
    }
}

/// Lowercase, replace non-alphanumeric runs with underscores, trim underscores.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_underscore = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

/// If `key` already exists in `existing`, suffix `_2`, `_3`, … until unique.
fn ensure_unique_key(existing: &toml::Table, key: String) -> String {
    if !existing.contains_key(&key) {
        return key;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{key}_{n}");
        if !existing.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}
