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
        // Field renames inside the block: V2 `non_cli_excluded_tools` → V3
        // `excluded_tools` (V3 broadens the field's meaning beyond the
        // non-CLI carve-out — same data shape, new name).
        if let Some(autonomy_value) = autonomy {
            let renamed = rename_table_keys(
                autonomy_value,
                &[("non_cli_excluded_tools", "excluded_tools")],
            );
            let mut risk_profiles = passthrough
                .remove("risk_profiles")
                .and_then(|v| v.try_into::<toml::Table>().ok())
                .unwrap_or_default();
            risk_profiles
                .entry("default".to_string())
                .or_insert(renamed);
            passthrough.insert(
                "risk_profiles".to_string(),
                toml::Value::Table(risk_profiles),
            );
            tracing::info!(target: "migration", "[autonomy] → [risk_profiles.default]");
        }

        // 1a. T13: fold V2 [security.sandbox] and [security.resources] into
        //     risk_profiles.default (V3 RiskProfileConfig absorbed both).
        //     Runs AFTER autonomy fold so the synthesized profile already
        //     exists and we just enrich it with sandbox/resource fields.
        fold_security_into_risk_profile(&mut passthrough);

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

        // 4a. T12: drop V2 reliability fallback fields (provider fallback
        //     was eradicated in V3 per `f8c66f1dd`). Reliability block
        //     itself stays and continues to deserialize as `ReliabilityConfig`.
        if let Some(toml::Value::Table(reliability_table)) = passthrough.get_mut("reliability") {
            let dropped_fb = reliability_table.remove("fallback_providers").is_some();
            let dropped_mf = reliability_table.remove("model_fallbacks").is_some();
            if dropped_fb || dropped_mf {
                tracing::info!(
                    target: "migration",
                    "[reliability] {{fallback_providers, model_fallbacks}} dropped (provider fallback eradicated in V3)"
                );
            }
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

        // 6a. T8: TTS subsystem promotion — V2 `[tts.<type>]` per-provider
        //     blocks → V3 `[providers.tts.<type>.<alias>]`. The bare
        //     `tts.default_provider = "openai"` scalar gets rewritten as
        //     dotted alias `"openai.default"` (V3 uses dotted aliases).
        fold_v2_tts_into_providers(&mut passthrough, &mut new_providers);

        if !new_providers.is_empty() {
            passthrough.insert("providers".to_string(), toml::Value::Table(new_providers));
        }
        if let Some(remaining_cost) = cost_passthrough {
            passthrough.insert("cost".to_string(), remaining_cost);
        }

        // 6b. T9 + T10: storage subsystem promotion. V2 `[memory.qdrant]`,
        //     `[memory.postgres]`, and `[storage.provider.config]` all fold
        //     into V3 `[storage.<backend>.<alias>]` with appropriate field
        //     adapters per backend.
        fold_v2_storage_subsystems(&mut passthrough);

        // 7. channels → alias-wrap each channel type, fold discord_history
        if let Some(channels_value) = channels {
            let new_channels = alias_wrap_channels(channels_value);
            passthrough.insert("channels".to_string(), toml::Value::Table(new_channels));
            tracing::info!(target: "migration", "[channels] sections alias-wrapped, discord_history folded");
        }

        // 8. agents → strip inline brain, synthesize provider aliases.
        //    If there are no [agents] blocks but the user had brain config
        //    folded onto a provider entry, synthesize a default agent so V3
        //    runtime has something concrete to dispatch to (V1/V2 had implicit
        //    "single global agent" semantics; V3 makes agents explicit, so
        //    upgrades need at least one). Also ensure the profile entries
        //    referenced by agents.default exist — V3 validation rejects
        //    dangling profile references.
        let new_agents = if !agents.is_empty() {
            synthesize_agent_brains(agents, &mut passthrough)
        } else {
            let synthesized = synthesize_default_agent_if_needed(&passthrough);
            if !synthesized.is_empty() {
                ensure_profile_entry(&mut passthrough, "risk_profiles", "default");
                ensure_profile_entry(&mut passthrough, "runtime_profiles", "default");
            }
            synthesized
        };
        if !new_agents.is_empty() {
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
    // T11: V3 keys jobs by their HashMap alias; the V2 `id: String` field
    // is redundant under that scheme and was removed in V3 — drop it from
    // each job's table before insertion.
    if let Some(toml::Value::Array(jobs)) = cron_table.remove("jobs") {
        for (i, job) in jobs.into_iter().enumerate() {
            // Pick alias key: name slug → id → fallback `job_N`.
            let key = job
                .get("name")
                .and_then(toml::Value::as_str)
                .map(slugify)
                .or_else(|| {
                    job.get("id")
                        .and_then(toml::Value::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| format!("job_{}", i + 1));
            let key = ensure_unique_key(&new_cron, key);
            // T11 strip: the id field is no longer part of CronJobDecl in V3.
            let stripped = match job {
                toml::Value::Table(mut t) => {
                    t.remove("id");
                    toml::Value::Table(t)
                }
                other => other,
            };
            new_cron.insert(key, stripped);
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
/// `"default"`. Applies, per channel instance:
///
/// - **discord_history fold**: `[channels.discord_history]` → `[channels.discord]`
///   with `archive = true`. Effective `enabled` is the OR of both sides so a
///   user with only `discord_history.enabled = true` still ends up with an
///   enabled merged discord block.
/// - **T3–T6 singular→plural folds** per channel type (`discord.guild_id` →
///   `guild_ids[]`, `mattermost.channel_id` → `channel_ids[]`,
///   `reddit.subreddit` → `subreddits[]`, `signal.group_id` → `group_ids[]`
///   or `dm_only=true` for the `"dm"` sentinel).
/// - **T7 enabled filter**: V3 dropped `enabled: bool` from every channel
///   config. V2 default was `false`. Channels whose V2 `enabled` was not
///   explicitly `true` are dropped from the V3 HashMap entirely; channels
///   that survive have their `enabled` field stripped (V3 has no slot for it).
///   Per-drop INFO log names the channel type and reason.
///
/// `cli: bool` is preserved at the top-level `channels.cli`, not aliased.
fn alias_wrap_channels(channels_value: toml::Value) -> toml::Table {
    let mut channels_table = match channels_value {
        toml::Value::Table(t) => t,
        _ => return toml::Table::new(),
    };
    let mut new_channels = toml::Table::new();

    // CLI is a top-level bool, not aliased.
    if let Some(cli) = channels_table.remove("cli") {
        new_channels.insert("cli".to_string(), cli);
    }

    // Fold discord_history into discord BEFORE the enabled filter so a
    // discord_history-only user with `enabled=true` survives into V3.
    fold_discord_history(&mut channels_table);

    // Per-channel-type processing: T3–T6 folds, T7 enabled filter, alias-wrap.
    for ct in V3_CHANNEL_TYPES {
        let Some(value) = channels_table.remove(*ct) else {
            continue;
        };
        let mut instance = match value {
            toml::Value::Table(t) => t,
            other => {
                // Unexpected shape — wrap raw value under "default" without
                // any of the V3 transforms. This preserves data; V3
                // deserialize will surface the type error.
                let mut wrapped = toml::Table::new();
                wrapped.insert("default".to_string(), other);
                new_channels.insert((*ct).to_string(), toml::Value::Table(wrapped));
                continue;
            }
        };
        apply_v2_to_v3_channel_folds(ct, &mut instance);
        if !drain_enabled_keep(ct, &mut instance) {
            continue;
        }
        let mut wrapped = toml::Table::new();
        wrapped.insert("default".to_string(), toml::Value::Table(instance));
        new_channels.insert((*ct).to_string(), toml::Value::Table(wrapped));
    }

    // Unmodeled channel-section keys: pass through under their original key.
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

/// Fold V2 `[channels.discord_history]` into `[channels.discord]` in place.
/// Sets `archive = true`. Effective `enabled` = `discord.enabled` OR
/// `discord_history.enabled`. Existing discord keys win over history keys
/// for non-`enabled` fields (so a user-set discord.bot_token isn't
/// overwritten by history's bot_token).
fn fold_discord_history(channels: &mut toml::Table) {
    let history_value = match channels.remove("discord_history") {
        Some(v) => v,
        None => return,
    };

    let history_enabled = history_value
        .as_table()
        .and_then(|t| t.get("enabled"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let discord_enabled = channels
        .get("discord")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("enabled"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let effective_enabled = discord_enabled || history_enabled;

    let discord_entry = channels
        .entry("discord".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(discord_table) = discord_entry.as_table_mut() {
        discord_table.insert("archive".to_string(), toml::Value::Boolean(true));
        if let toml::Value::Table(history_table) = history_value {
            for (k, v) in history_table {
                if k == "enabled" {
                    // Handled explicitly via effective_enabled below.
                    continue;
                }
                discord_table.entry(k).or_insert(v);
            }
        }
        discord_table.insert(
            "enabled".to_string(),
            toml::Value::Boolean(effective_enabled),
        );
    }
    tracing::info!(
        target: "migration",
        "[channels.discord_history] folded into [channels.discord] (archive=true, effective enabled={effective_enabled})"
    );
}

/// Apply V2→V3 singular→plural folds to a channel-instance table in place.
///
/// - **T3** discord.guild_id → guild_ids[]
/// - **T4** mattermost.channel_id → channel_ids[]
/// - **T5** reddit.subreddit → subreddits[]
/// - **T6** signal.group_id → group_ids[] (or `dm_only=true` for the
///   legacy `"dm"` sentinel value).
fn apply_v2_to_v3_channel_folds(channel_type: &str, instance: &mut toml::Table) {
    use crate::migration::fold_string_into_array;
    match channel_type {
        "discord" => {
            if fold_string_into_array(instance, "guild_id", "guild_ids") {
                tracing::info!(
                    target: "migration",
                    "channels.discord.guild_id folded into channels.discord.guild_ids[]"
                );
            }
        }
        "mattermost" => {
            if fold_string_into_array(instance, "channel_id", "channel_ids") {
                tracing::info!(
                    target: "migration",
                    "channels.mattermost.channel_id folded into channels.mattermost.channel_ids[]"
                );
            }
        }
        "reddit" => {
            if fold_string_into_array(instance, "subreddit", "subreddits") {
                tracing::info!(
                    target: "migration",
                    "channels.reddit.subreddit folded into channels.reddit.subreddits[]"
                );
            }
        }
        "signal" => {
            // Special: V2 group_id="dm" was a sentinel meaning "DMs only".
            // V3 splits that into a typed dm_only bool. Other group_id
            // values fold into group_ids[] like the simpler renames.
            if let Some(toml::Value::String(group_id)) = instance.remove("group_id")
                && !group_id.is_empty()
            {
                if group_id == "dm" {
                    instance.insert("dm_only".to_string(), toml::Value::Boolean(true));
                    tracing::info!(
                        target: "migration",
                        "channels.signal.group_id=\"dm\" → channels.signal.dm_only=true"
                    );
                } else {
                    let entry = instance
                        .entry("group_ids".to_string())
                        .or_insert_with(|| toml::Value::Array(Vec::new()));
                    if let Some(arr) = entry.as_array_mut() {
                        let already = arr.iter().any(|v| v.as_str() == Some(group_id.as_str()));
                        if !already {
                            arr.push(toml::Value::String(group_id));
                        }
                    }
                    tracing::info!(
                        target: "migration",
                        "channels.signal.group_id folded into channels.signal.group_ids[]"
                    );
                }
            }
        }
        _ => {}
    }
}

/// **T7**: V3 removed `enabled: bool` from every channel config. V2's default
/// was `false`; activation in V3 is implicit by HashMap presence. Strip
/// `enabled` from the instance and decide whether the channel survives:
///
/// - `enabled = true` → keep, no log.
/// - `enabled = false` → drop, log naming the channel type.
/// - `enabled` missing → drop (V2 default false), log.
/// - `enabled` non-bool → keep (treat as "configured"), log the unexpected
///   type rather than dropping data.
fn drain_enabled_keep(channel_type: &str, instance: &mut toml::Table) -> bool {
    match instance.remove("enabled") {
        Some(toml::Value::Boolean(true)) => true,
        Some(toml::Value::Boolean(false)) => {
            tracing::info!(
                target: "migration",
                "channels.{channel_type} dropped (V2 enabled=false; V3 has no off-switch other than absence)"
            );
            false
        }
        Some(other) => {
            tracing::info!(
                target: "migration",
                "channels.{channel_type}.enabled was {other:?} (non-bool); treating as enabled for V3"
            );
            true
        }
        None => {
            tracing::info!(
                target: "migration",
                "channels.{channel_type} dropped (V2 enabled defaulted to false; V3 has no off-switch other than absence)"
            );
            false
        }
    }
}

/// Strip V2-specific fields from each agent and synthesize the V3 alias
/// references / per-agent runtime overrides.
///
/// - **Brain fold** (already covered before T14): for each agent with a
///   `provider` string, synthesize a `providers.models.<provider>.agent_<id>`
///   entry with `{model, api_key, temperature}` and replace the agent's
///   brain with `model_provider = "<provider>.agent_<id>"`.
/// - **T14a max_iterations rename**: V2 `max_iterations: usize` →
///   V3 `max_tool_iterations: usize` (V3 keeps it inline on
///   `DelegateAgentConfig`, just renamed).
/// - **T14b runtime override synthesis**: V2 `agentic`, `allowed_tools`,
///   `timeout_secs`, `agentic_timeout_secs` are removed from
///   `DelegateAgentConfig` in V3 — they belong on `RuntimeProfileConfig`.
///   When any are set, synthesize a per-agent runtime profile at
///   `runtime_profiles.agent_<id>` and point `runtime_profile = "agent_<id>"`.
/// - **T14c risk override synthesis**: V2 `max_depth` → per-agent
///   `risk_profiles.agent_<id>.max_delegation_depth`, with `risk_profile`
///   pointing at `agent_<id>`.
/// - **T14d skills_directory drop**: V3 wants `skill_bundles: Vec<String>`
///   alias references; the V2 path-on-disk has no clean V3 equivalent.
///   Logged and dropped.
/// - **T14e memory_namespace type widening**: V2 `Option<String>` → V3
///   `String`. `None`/missing maps to `""` (V3's "no namespace" sentinel).
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

        // Brain fold: provider/model/api_key/temperature → model-provider alias
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
            agent_table.insert("provider".to_string(), other);
        }

        // T14a: max_iterations → max_tool_iterations (V3 inline rename).
        if let Some(v) = agent_table.remove("max_iterations") {
            agent_table
                .entry("max_tool_iterations".to_string())
                .or_insert(v);
            tracing::info!(
                target: "migration",
                "agents.{alias}.max_iterations → agents.{alias}.max_tool_iterations"
            );
        }

        // T14b: runtime overrides → per-agent runtime_profile.
        let runtime_overrides = extract_runtime_overrides(&mut agent_table);
        if let Some(overrides) = runtime_overrides {
            let profile_alias = format!("agent_{}", alias);
            install_profile_entry(passthrough, "runtime_profiles", &profile_alias, overrides);
            agent_table.insert(
                "runtime_profile".to_string(),
                toml::Value::String(profile_alias.clone()),
            );
            tracing::info!(
                target: "migration",
                "agents.{alias} runtime overrides → runtime_profiles.{profile_alias}"
            );
        }

        // T14c: max_depth → per-agent risk_profile.max_delegation_depth.
        if let Some(max_depth) = agent_table.remove("max_depth") {
            let mut overrides = toml::Table::new();
            overrides.insert("max_delegation_depth".to_string(), max_depth);
            let profile_alias = format!("agent_{}", alias);
            install_profile_entry(passthrough, "risk_profiles", &profile_alias, overrides);
            agent_table
                .entry("risk_profile".to_string())
                .or_insert_with(|| toml::Value::String(profile_alias.clone()));
            tracing::info!(
                target: "migration",
                "agents.{alias}.max_depth → risk_profiles.{profile_alias}.max_delegation_depth"
            );
        }

        // T14d: skills_directory has no V3 alias equivalent. Drop with log.
        if agent_table.remove("skills_directory").is_some() {
            tracing::info!(
                target: "migration",
                "agents.{alias}.skills_directory dropped — V3 uses skill_bundles alias references; \
                 add a [skill_bundles.<alias>] entry pointing at the directory and reference it here"
            );
        }

        // T14f: every V3 agent must reference a configured risk_profile and
        //   runtime_profile (V3 dangling-reference validation rejects unset
        //   or empty alias references). For agents that didn't trigger the
        //   T14b/T14c per-agent profile synthesis, default to "default" and
        //   ensure both entries exist.
        let agent_risk = agent_table
            .get("risk_profile")
            .and_then(toml::Value::as_str)
            .map(ToString::to_string)
            .filter(|s| !s.is_empty());
        let risk_alias = agent_risk.unwrap_or_else(|| "default".to_string());
        ensure_profile_entry(passthrough, "risk_profiles", &risk_alias);
        agent_table.insert("risk_profile".to_string(), toml::Value::String(risk_alias));

        let agent_runtime = agent_table
            .get("runtime_profile")
            .and_then(toml::Value::as_str)
            .map(ToString::to_string)
            .filter(|s| !s.is_empty());
        let runtime_alias = agent_runtime.unwrap_or_else(|| "default".to_string());
        ensure_profile_entry(passthrough, "runtime_profiles", &runtime_alias);
        agent_table.insert(
            "runtime_profile".to_string(),
            toml::Value::String(runtime_alias),
        );

        // T14e: memory_namespace type widening (Option<String> → String).
        // V3 wants a bare string; missing or unset becomes "". Also
        // synthesize an empty memory_namespaces.<ns> entry when an agent
        // references one, since V3 dangling-reference validation rejects
        // unresolved alias references.
        let referenced_ns = match agent_table.get("memory_namespace") {
            Some(toml::Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(toml::Value::String(_)) => None,
            Some(_) => {
                agent_table.insert(
                    "memory_namespace".to_string(),
                    toml::Value::String(String::new()),
                );
                None
            }
            None => None,
        };
        if let Some(ns) = referenced_ns {
            ensure_memory_namespace(passthrough, &ns);
        }

        new_agents.insert(alias, toml::Value::Table(agent_table));
    }
    new_agents
}

/// Ensure `memory_namespaces.<alias>` exists with at least `namespace = "<alias>"`.
/// V3 `MemoryNamespaceConfig` requires a `namespace` field — when an agent
/// references a namespace alias and the user hasn't defined it explicitly,
/// synthesize a minimal entry so V3 dangling-reference validation passes.
fn ensure_memory_namespace(passthrough: &mut toml::Table, alias: &str) {
    let section_value = passthrough
        .entry("memory_namespaces".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(section_table) = section_value.as_table_mut() {
        let entry_value = section_table
            .entry(alias.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(entry_table) = entry_value.as_table_mut() {
            entry_table
                .entry("namespace".to_string())
                .or_insert_with(|| toml::Value::String(alias.to_string()));
        }
    }
}

/// Pull V2 `DelegateAgentConfig` fields that V3 moved onto
/// `RuntimeProfileConfig` out of the agent table. Returns `Some(table)` if
/// any V3 runtime-profile field was set; `None` otherwise.
fn extract_runtime_overrides(agent: &mut toml::Table) -> Option<toml::Table> {
    let mut out = toml::Table::new();
    for (v2_key, v3_key) in [
        ("agentic", "agentic"),
        ("allowed_tools", "allowed_tools"),
        ("timeout_secs", "timeout_secs"),
        ("agentic_timeout_secs", "agentic_timeout_secs"),
    ] {
        if let Some(v) = agent.remove(v2_key) {
            out.insert(v3_key.to_string(), v);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Insert (or merge) a profile entry at `passthrough.<section>.<alias>`.
/// Existing keys win — `fields` only fills in missing slots.
fn install_profile_entry(
    passthrough: &mut toml::Table,
    section: &str,
    alias: &str,
    fields: toml::Table,
) {
    let section_value = passthrough
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(section_table) = section_value.as_table_mut() {
        let alias_value = section_table
            .entry(alias.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(alias_table) = alias_value.as_table_mut() {
            for (k, v) in fields {
                alias_table.entry(k).or_insert(v);
            }
        }
    }
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

/// Ensure `[<section>.<alias>]` exists in `passthrough` as at least an
/// empty table. Used when synthesizing the default agent so the agent's
/// alias references resolve under V3 dangling-reference validation.
fn ensure_profile_entry(passthrough: &mut toml::Table, section: &str, alias: &str) {
    let entry = passthrough
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(section_table) = entry.as_table_mut() {
        section_table
            .entry(alias.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    }
}

/// If no agents were declared in V2 input but the V2→V3 fold synthesized at
/// least one provider model entry, emit a single `agents.default` referencing
/// the first provider-alias. This preserves V1/V2 implicit single-agent
/// semantics: the V1 user with `default_provider = "openai"` and a brain
/// configured globally gets a working V3 default agent automatically.
///
/// `passthrough` is read (not mutated) — the synthesized agent is returned so
/// the caller decides whether to install it under `agents`.
fn synthesize_default_agent_if_needed(passthrough: &toml::Table) -> toml::Table {
    let providers = match passthrough.get("providers").and_then(toml::Value::as_table) {
        Some(t) => t,
        None => return toml::Table::new(),
    };
    let models = match providers.get("models").and_then(toml::Value::as_table) {
        Some(t) => t,
        None => return toml::Table::new(),
    };
    let first_alias = models.iter().find_map(|(provider_type, value)| {
        let inner = value.as_table()?;
        let alias = inner.keys().next()?;
        Some(format!("{provider_type}.{alias}"))
    });
    let alias_ref = match first_alias {
        Some(s) => s,
        None => return toml::Table::new(),
    };

    let mut default_agent = toml::Table::new();
    default_agent.insert("model_provider".to_string(), toml::Value::String(alias_ref));
    default_agent.insert(
        "risk_profile".to_string(),
        toml::Value::String("default".into()),
    );
    default_agent.insert(
        "runtime_profile".to_string(),
        toml::Value::String("default".into()),
    );

    let mut agents = toml::Table::new();
    agents.insert("default".to_string(), toml::Value::Table(default_agent));
    tracing::info!(
        target: "migration",
        "synthesized [agents.default] from V1/V2 implicit single-agent semantics"
    );
    agents
}

/// V3 TTS provider type keys. Matches the V2 `TtsConfig` per-provider
/// option fields.
const V3_TTS_TYPES: &[&str] = &["openai", "elevenlabs", "google", "edge", "piper"];

/// **T8**: V2 `[tts.<type>]` sub-blocks → V3 `[providers.tts.<type>.default]`.
///
/// V2 `TtsConfig` had per-provider `Option<*TtsConfig>` fields (`openai`,
/// `elevenlabs`, `google`, `edge`, `piper`); V3 unifies them under a single
/// `TtsProviderConfig` keyed by `<type>.<alias>` like the model providers.
///
/// `[tts]` top-level scalars (`enabled`, `default_voice`, `default_format`,
/// `max_text_length`) stay on `[tts]`. The `default_provider` scalar gets
/// rewritten from a bare type name (`"openai"`) to a dotted alias
/// (`"openai.default"`) to match the V3 reference format.
fn fold_v2_tts_into_providers(passthrough: &mut toml::Table, new_providers: &mut toml::Table) {
    let Some(toml::Value::Table(tts_table)) = passthrough.get_mut("tts") else {
        return;
    };

    let mut tts_aliased = toml::Table::new();
    for ty in V3_TTS_TYPES {
        if let Some(mut value) = tts_table.remove(*ty) {
            // V2 ElevenLabsTtsConfig.model_id → V3 TtsProviderConfig.model.
            // Other V2 sub-types (OpenAi, Google, Edge, Piper) used field
            // names that survive into V3's unified TtsProviderConfig as-is.
            if *ty == "elevenlabs"
                && let Some(t) = value.as_table_mut()
                && let Some(v) = t.remove("model_id")
            {
                t.entry("model".to_string()).or_insert(v);
                tracing::info!(
                    target: "migration",
                    "tts.elevenlabs.model_id renamed to tts.elevenlabs.model"
                );
            }
            let mut wrapped = toml::Table::new();
            wrapped.insert("default".to_string(), value);
            tts_aliased.insert((*ty).to_string(), toml::Value::Table(wrapped));
        }
    }

    if let Some(toml::Value::String(s)) = tts_table.get_mut("default_provider")
        && !s.is_empty()
        && !s.contains('.')
    {
        *s = format!("{s}.default");
        tracing::info!(
            target: "migration",
            "tts.default_provider rewritten as dotted alias"
        );
    }

    if !tts_aliased.is_empty() {
        new_providers.insert("tts".to_string(), toml::Value::Table(tts_aliased));
        tracing::info!(
            target: "migration",
            "[tts.<type>] sub-blocks promoted to [providers.tts.<type>.default]"
        );
    }
}

/// **T9 + T10**: Fold V2 `[memory.qdrant]`, `[memory.postgres]`, and
/// `[storage.provider.config]` into V3 `[storage.<backend>.<alias>]`.
///
/// V2 had three sources of storage configuration that V3 unifies under a
/// single typed map per backend:
///
/// - `[memory.qdrant]`: V2 `QdrantConfig {url, collection, api_key}` —
///   ships directly to `[storage.qdrant.default]` (V3 `QdrantStorageConfig`
///   takes the same field names).
/// - `[memory.postgres]`: V2 `PostgresMemoryConfig {vector_enabled, vector_dimensions}` —
///   contributes only the vector fields; the remaining `db_url`, `schema`,
///   `table` come from `[storage.provider.config]` if the user set
///   `provider = "postgres"` there.
/// - `[storage.provider.config]`: V2 `StorageProviderConfig {provider, db_url,
///   schema, table, connect_timeout_secs}` — the `provider` field selects the
///   V3 backend; remaining fields are adapted per-backend (sqlite extracts
///   path from a `sqlite://...` URL; qdrant maps `db_url` → `url`; postgres
///   maps the fields directly).
///
/// `[memory.sqlite_open_timeout_secs]` is dropped (V3 moved it onto
/// `SqliteStorageConfig.open_timeout_secs`).
///
/// Existing V3-shaped fields take precedence over the legacy fold (so a
/// user who already wrote `[storage.qdrant.default]` doesn't get clobbered).
fn fold_v2_storage_subsystems(passthrough: &mut toml::Table) {
    let (memory_qdrant, memory_postgres, memory_sqlite_timeout) = match passthrough
        .get_mut("memory")
        .and_then(toml::Value::as_table_mut)
    {
        Some(memory) => (
            memory.remove("qdrant"),
            memory.remove("postgres"),
            memory.remove("sqlite_open_timeout_secs"),
        ),
        None => (None, None, None),
    };

    let storage_provider = match passthrough
        .get_mut("storage")
        .and_then(toml::Value::as_table_mut)
    {
        Some(storage) => storage.remove("provider"),
        None => None,
    };

    if memory_qdrant.is_none()
        && memory_postgres.is_none()
        && memory_sqlite_timeout.is_none()
        && storage_provider.is_none()
    {
        return;
    }

    let storage_entry = passthrough
        .entry("storage".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(storage_table) = storage_entry.as_table_mut() else {
        return;
    };

    if let Some(toml::Value::Table(qdrant_data)) = memory_qdrant {
        merge_storage_default(storage_table, "qdrant", qdrant_data);
        tracing::info!(
            target: "migration",
            "[memory.qdrant] promoted to [storage.qdrant.default]"
        );
    }
    if let Some(timeout_value) = memory_sqlite_timeout {
        let mut sqlite_fields = toml::Table::new();
        sqlite_fields.insert("open_timeout_secs".to_string(), timeout_value);
        merge_storage_default(storage_table, "sqlite", sqlite_fields);
        tracing::info!(
            target: "migration",
            "memory.sqlite_open_timeout_secs → [storage.sqlite.default].open_timeout_secs"
        );
    }
    if let Some(toml::Value::Table(postgres_vector_data)) = memory_postgres {
        merge_storage_default(storage_table, "postgres", postgres_vector_data);
        tracing::info!(
            target: "migration",
            "[memory.postgres] vector fields promoted to [storage.postgres.default]"
        );
    }

    if let Some(provider_section_value) = storage_provider {
        // V2 had two layouts: `[storage.provider.config]` (nested) or
        // `storage.provider = { provider = "...", db_url = "..." }` (inline).
        // Both produce the same parsed structure: a Table with a `config`
        // sub-table. Flatten that here.
        let config_table = match provider_section_value {
            toml::Value::Table(mut section) => {
                if let Some(toml::Value::Table(inner)) = section.remove("config") {
                    inner
                } else {
                    section
                }
            }
            _ => return,
        };
        if config_table.is_empty() {
            return;
        }

        let (provider_type, mut adapted_fields) = adapt_storage_provider_config(config_table);
        if !adapted_fields.is_empty() {
            // sqlite_open_timeout_secs from [memory] (already removed above)
            // wasn't re-injected, but we previously moved memory.qdrant /
            // memory.postgres in here, so fields stay separate per backend.
            merge_storage_default(
                storage_table,
                &provider_type,
                std::mem::take(&mut adapted_fields),
            );
            tracing::info!(
                target: "migration",
                "[storage.provider.config provider={provider_type}] promoted to [storage.{provider_type}.default]"
            );
        }
    }
}

/// Adapt a V2 `StorageProviderConfig` (flat `{provider, db_url, schema,
/// table, connect_timeout_secs}`) to the V3 backend-specific shape. Returns
/// the chosen backend type and the adapted field table.
fn adapt_storage_provider_config(mut config: toml::Table) -> (String, toml::Table) {
    let provider_type = config
        .remove("provider")
        .and_then(|v| match v {
            toml::Value::String(s) if !s.is_empty() => Some(s),
            _ => None,
        })
        .unwrap_or_else(|| "sqlite".to_string());

    match provider_type.as_str() {
        "sqlite" => {
            let mut out = toml::Table::new();
            // V2 db_url for sqlite was typically "sqlite:///path" — extract path.
            if let Some(toml::Value::String(db_url)) = config.remove("db_url") {
                let path = db_url
                    .strip_prefix("sqlite://")
                    .or_else(|| db_url.strip_prefix("sqlite:"))
                    .map(ToString::to_string)
                    .unwrap_or(db_url);
                if !path.is_empty() {
                    out.insert("path".to_string(), toml::Value::String(path));
                }
            }
            // V2 connect_timeout_secs maps to V3 SqliteStorageConfig.open_timeout_secs.
            if let Some(v) = config.remove("connect_timeout_secs") {
                out.insert("open_timeout_secs".to_string(), v);
            }
            // schema/table not applicable to sqlite — drop.
            (provider_type, out)
        }
        "postgres" => {
            // db_url, schema, table, connect_timeout_secs all map directly.
            (provider_type, config)
        }
        "qdrant" => {
            let mut out = toml::Table::new();
            if let Some(v) = config.remove("db_url") {
                out.insert("url".to_string(), v);
            }
            // schema/table not applicable to qdrant — drop.
            (provider_type, out)
        }
        _ => {
            tracing::info!(
                target: "migration",
                "[storage.provider.config] unknown provider type {provider_type:?}; passthrough as-is"
            );
            (provider_type, config)
        }
    }
}

/// Merge `fields` into `storage_table.<backend>.default`, creating the
/// nested tables if missing. Existing keys win — `fields` only fills gaps.
fn merge_storage_default(storage_table: &mut toml::Table, backend_type: &str, fields: toml::Table) {
    let backend_entry = storage_table
        .entry(backend_type.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(backend_table) = backend_entry.as_table_mut() {
        let default_entry = backend_table
            .entry("default".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(default_table) = default_entry.as_table_mut() {
            for (k, v) in fields {
                default_table.entry(k).or_insert(v);
            }
        }
    }
}

/// **T13**: Fold V2 `[security.sandbox]` and `[security.resources]` blocks
/// into the `risk_profiles.default` entry under V3 field names.
///
/// V3 `RiskProfileConfig` absorbed sandbox and resource limits — they used
/// to live at `[security.sandbox]` and `[security.resources]`, but V3 places
/// them on each risk profile so per-agent overrides are possible.
///
/// Field renames during the fold:
/// - `security.sandbox.enabled` → `risk_profiles.default.sandbox_enabled`
/// - `security.sandbox.backend` → `risk_profiles.default.sandbox_backend`
/// - `security.sandbox.firejail_args` → `risk_profiles.default.firejail_args`
/// - `security.resources.max_memory_mb` → `risk_profiles.default.max_memory_mb`
/// - `security.resources.max_cpu_time_seconds` → `risk_profiles.default.max_cpu_time_seconds`
/// - `security.resources.max_subprocesses` → `risk_profiles.default.max_subprocesses`
/// - `security.resources.memory_monitoring` → `risk_profiles.default.memory_monitoring`
///
/// Both V2 source blocks are removed. Existing values on the V3 profile take
/// precedence — globals only fill in missing slots.
fn fold_security_into_risk_profile(passthrough: &mut toml::Table) {
    let (sandbox, resources) = {
        let security_table = match passthrough
            .get_mut("security")
            .and_then(toml::Value::as_table_mut)
        {
            Some(t) => t,
            None => return,
        };
        (
            security_table.remove("sandbox"),
            security_table.remove("resources"),
        )
    };
    if sandbox.is_none() && resources.is_none() {
        return;
    }

    let risk_profiles = passthrough
        .entry("risk_profiles".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(risk_profiles_table) = risk_profiles.as_table_mut() else {
        return;
    };
    let default_entry = risk_profiles_table
        .entry("default".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(default_profile) = default_entry.as_table_mut() else {
        return;
    };

    if let Some(toml::Value::Table(sandbox_table)) = sandbox {
        for (k, v) in sandbox_table {
            let target_key = match k.as_str() {
                "enabled" => "sandbox_enabled",
                "backend" => "sandbox_backend",
                "firejail_args" => "firejail_args",
                _ => continue,
            };
            default_profile.entry(target_key.to_string()).or_insert(v);
        }
        tracing::info!(
            target: "migration",
            "[security.sandbox] folded into [risk_profiles.default]"
        );
    }
    if let Some(toml::Value::Table(resources_table)) = resources {
        for (k, v) in resources_table {
            let target_key = match k.as_str() {
                "max_memory_mb" => "max_memory_mb",
                "max_cpu_time_seconds" => "max_cpu_time_seconds",
                "max_subprocesses" => "max_subprocesses",
                "memory_monitoring" => "memory_monitoring",
                _ => continue,
            };
            default_profile.entry(target_key.to_string()).or_insert(v);
        }
        tracing::info!(
            target: "migration",
            "[security.resources] folded into [risk_profiles.default]"
        );
    }
}

/// Rename top-level keys inside a `toml::Value::Table` according to a list of
/// `(old, new)` pairs. Non-tables are returned unchanged. Existing values at
/// the new key are not overwritten — the rename is best-effort.
fn rename_table_keys(value: toml::Value, renames: &[(&str, &str)]) -> toml::Value {
    let mut table = match value {
        toml::Value::Table(t) => t,
        other => return other,
    };
    for (old, new) in renames {
        if let Some(v) = table.remove(*old)
            && !table.contains_key(*new)
        {
            table.insert((*new).to_string(), v);
        }
    }
    toml::Value::Table(table)
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
