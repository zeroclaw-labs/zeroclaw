use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// V1/V2 supported a "colon-URL" provider string form (e.g.
/// `"anthropic-custom:https://api.z.ai/api/anthropic"`) where the URL was
/// embedded inline. V3 uses a typed `uri` field on the per-provider
/// alias entry. This helper splits the colon-URL form into `(type, url)`
/// so the migration can use `type` as the V3 provider key and store the
/// URL in `uri` on the alias entry. Returns `(type_key, Some(url))`
/// for colon-URL forms; otherwise `(raw.to_string(), None)`.
fn split_colon_url_provider(raw: &str) -> (String, Option<String>) {
    if let Some(colon_idx) = raw.find(':') {
        let (prefix, rest) = raw.split_at(colon_idx);
        let url = &rest[1..];
        if (prefix == "custom" || prefix == "anthropic-custom")
            && (url.starts_with("https://") || url.starts_with("http://"))
        {
            return (prefix.to_string(), Some(url.to_string()));
        }
    }
    (raw.to_string(), None)
}

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

    /// V3 dropped swarms entirely.
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
    /// `model_providers.<provider>.agent_<id>`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub agents: HashMap<String, toml::Value>,

    /// Everything else passes through unchanged.
    #[serde(flatten)]
    pub passthrough: toml::Table,
}

fn default_v2_schema_version() -> u32 {
    2
}

/// Channel section keys subject to V3 alias-wrapping. A missing entry
/// here sends its V2 `[channels.<type>]` block through the passthrough
/// branch, which leaves it flat instead of `<type>.default`-shaped and
/// the V3 deserializer then chokes on the typed `HashMap<String, T>`
/// slot. Tests cross-check this list against the typed channel slots
/// on `ChannelsConfig` to catch silent drift.
pub const V3_CHANNEL_TYPES: &[&str] = &[
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
    "voice_wake",
    "voice_duplex",
    "mqtt",
];

impl V2Config {
    /// Returns a V3-shaped `toml::Value`. The caller deserializes it
    /// into `Config` — that round-trip is the gate that catches any
    /// structural mismatch.
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

        // autonomy → risk_profiles.default, with V2 non_cli_excluded_tools
        // renamed to V3 excluded_tools (broader scope, same shape).
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

        // V3 RiskProfileConfig absorbed [security.sandbox] and
        // [security.resources]; enrich the autonomy-derived profile.
        fold_security_into_risk_profile(&mut passthrough);

        // agent → runtime_profiles.default
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

        // V3 dropped swarms.
        if !swarms.is_empty() {
            tracing::info!(
                target: "migration",
                "[swarms] dropped ({} entries)",
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

        // V3 eradicated provider fallback. Strip the V2 reliability
        // fields that referenced it; the rest of [reliability] stays.
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

        // Restructure providers: drop fallback, alias-wrap models,
        // fold V2 [providers] globals down to per-provider entries.
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

        // V3 ModelProviderConfig absorbed the V2 [providers] globals
        // (api_key, default_model, etc.) inline; fold them down.
        fold_providers_globals_into_models(&mut new_providers, &mut aliased_models);

        // V3 dropped cost.prices: the V2 keys ("<provider>/<model>")
        // don't carry the V3 alias path, so remapping is fragile.
        // Log each entry's last-known rates for manual reinstatement.
        let cost_passthrough = if let Some(cost_value) = cost {
            let (cost_remaining, prices) = strip_cost_prices(cost_value);
            if !prices.is_empty() {
                drop_cost_prices_with_logs(&prices);
            }
            cost_remaining
        } else {
            None
        };
        if !aliased_models.is_empty() {
            new_providers.insert("models".to_string(), toml::Value::Table(aliased_models));
        }

        // V3 renamed the route field `provider` → `model_provider` to
        // disambiguate from TTS/transcription providers. Apply to both
        // the [providers.<routes>] nested form and the bare top-level
        // [[model_routes]] / [[embedding_routes]] arrays.
        rename_route_provider_field(&mut new_providers, "model_routes");
        rename_route_provider_field(&mut new_providers, "embedding_routes");
        rename_route_provider_field(&mut passthrough, "model_routes");
        rename_route_provider_field(&mut passthrough, "embedding_routes");

        // Promote V2 [tts.<type>] / [transcription.<family>] sub-blocks
        // into V3 [<kind>_providers.<type>.default]. Global
        // default_provider keys are dropped — V3 has no such concept;
        // each agent declares its own provider.
        fold_v2_tts_into_providers(&mut passthrough, &mut new_providers);
        fold_v2_transcription_into_providers(&mut passthrough, &mut new_providers);

        // V3 collapses model/tts/transcription providers under a single
        // top-level `[providers]` table, with one sub-key per category.
        // Hoist providers.{models,tts,transcription} into a shared
        // `providers` table; *_routes stay top-level.
        let mut v3_providers = toml::Table::new();
        if let Some(models) = new_providers.remove("models") {
            v3_providers.insert("models".to_string(), models);
        }
        if let Some(tts) = new_providers.remove("tts") {
            v3_providers.insert("tts".to_string(), tts);
        }
        if let Some(transcription) = new_providers.remove("transcription") {
            v3_providers.insert("transcription".to_string(), transcription);
        }
        if !v3_providers.is_empty() {
            passthrough.insert("providers".to_string(), toml::Value::Table(v3_providers));
        }
        if let Some(routes) = new_providers.remove("model_routes") {
            passthrough.insert("model_routes".to_string(), routes);
        }
        if let Some(routes) = new_providers.remove("embedding_routes") {
            passthrough.insert("embedding_routes".to_string(), routes);
        }
        if !new_providers.is_empty() {
            tracing::warn!(
                target: "migration",
                "[providers] residual keys dropped during V3 hoist: {:?}",
                new_providers.keys().collect::<Vec<_>>()
            );
        }
        if let Some(remaining_cost) = cost_passthrough {
            passthrough.insert("cost".to_string(), remaining_cost);
        }

        // V2 [memory.qdrant], [memory.postgres], and [storage.provider.config]
        // all collapse into V3 [storage.<backend>.<alias>].
        fold_v2_storage_subsystems(&mut passthrough);

        // Alias-wrap each [channels.<type>], fold discord_history into
        // [channels.discord.<alias>].archive, and lift per-channel
        // inbound peer-auth fields (allowed_users, allowed_contacts,
        // allowed_from, allowed_numbers, allowed_senders, allowed_pubkeys)
        // into synthesized [peer_groups.<type>_default] entries. The
        // peer_groups sink is additive — operator entries survive.
        if let Some(channels_value) = channels {
            let mut peer_groups_for_fold = match passthrough.remove("peer_groups") {
                Some(toml::Value::Table(t)) => t,
                _ => toml::Table::new(),
            };
            let new_channels = alias_wrap_channels(channels_value, &mut peer_groups_for_fold);
            passthrough.insert("channels".to_string(), toml::Value::Table(new_channels));
            if !peer_groups_for_fold.is_empty() {
                passthrough.insert(
                    "peer_groups".to_string(),
                    toml::Value::Table(peer_groups_for_fold),
                );
            }
            tracing::info!(target: "migration", "[channels] sections alias-wrapped, discord_history folded, inbound peer-auth folded into [peer_groups.*]");
        }

        // V3 makes agents explicit — V1/V2 had an implicit single-agent
        // model. Strip inline brain fields onto provider aliases; if no
        // [agents] blocks but brain config exists, synthesize a default
        // agent (with the profile entries it references) so the upgrade
        // has at least one runnable agent.
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

        // V3 demoted [identity] to per-agent. Lift the V2 top-level block
        // into each declared [agents.<alias>.identity]. Runs after the
        // agents fold so synthesized and pre-existing agents both get it.
        lift_top_level_identity_into_agents(&mut passthrough);

        // V3 requires heartbeat.agent to be set when enabled=true.
        // V2 fell through to the implicit single agent; point this at
        // the synthesized (or first preserved) agent.
        backfill_heartbeat_agent(&mut passthrough);

        // peer_groups synthesized in the channels step used the bridge
        // alias "default". If named agents won out (no agents.default),
        // rewrite each peer_groups.<X>.agents = ["default"] to the
        // surviving agent alias.
        rewrite_dangling_peer_group_agents(&mut passthrough);

        // V3 renamed `provider` to a domain-qualified noun on a few
        // tables. Without this rewrite V3 errors with `missing field
        // <noun>_provider`.
        rename_subkey(&mut passthrough, "tunnel", "provider", "tunnel_provider");
        rename_subkey(
            &mut passthrough,
            "web_search",
            "provider",
            "search_provider",
        );

        passthrough.insert("schema_version".to_string(), toml::Value::Integer(3));

        Ok(toml::Value::Table(passthrough))
    }
}

/// Rename `inner` to `replacement` inside the `[<parent>]` table when both
/// the parent and the inner key are present. No-op if either is absent or
/// if `replacement` already exists (operator wins; their explicit V3 key is
/// the source of truth). Used for V3 schema field renames where the
/// migration just needs to rewrite a flat scalar in place.
fn rename_subkey(table: &mut toml::Table, parent: &str, inner: &str, replacement: &str) {
    let Some(toml::Value::Table(parent_tbl)) = table.get_mut(parent) else {
        return;
    };
    if parent_tbl.contains_key(replacement) {
        // Operator already wrote the V3 key; nothing to do. If they ALSO
        // wrote the V2 key, drop the stale one so the deserializer doesn't
        // see a stray field on a `#[serde(deny_unknown_fields)]` struct.
        let _ = parent_tbl.remove(inner);
        return;
    }
    if let Some(value) = parent_tbl.remove(inner) {
        parent_tbl.insert(replacement.to_string(), value);
        tracing::info!(
            target: "migration",
            "[{parent}].{inner} renamed to [{parent}].{replacement} (V3 qualified-noun rename)"
        );
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

    // V2 had `[[cron.jobs]]` array; V3 keys each job by its HashMap
    // alias, which makes the V2 `id: String` field redundant. Strip it.
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

/// Normalize a V2 provider type string to its V3 canonical name plus the
/// extras that the typed family config requires (region endpoint, auth_mode,
/// alias rename, family-specific fields).
///
/// Returns `(canonical_type, alias_key, extras_to_inject)`. `extras_to_inject`
/// is a vec of `(field_name, toml::Value)` pairs that the migration writes
/// onto the alias entry table — typically `endpoint = "cn"` for regional
/// collapses, `auth_mode = "oauth"` for oauth-mode collapses, `wire_api =
/// "responses"` + `requires_openai_auth = true` for the openai_codex fold.
///
/// The alias spellings here mirror the V2 registry's match arms in
/// `crates/zeroclaw-providers/src/lib.rs` (`is_<vendor>_alias` functions).
fn normalize_provider_type(
    raw: &str,
    incoming_alias: &str,
) -> (String, String, Vec<(&'static str, toml::Value)>) {
    let mut extras: Vec<(&'static str, toml::Value)> = Vec::new();

    // Vendor-canonical collapses (synonym kills only; alias unchanged).
    let synonym_canonical = match raw {
        // Azure: vendor name; was azure_openai|azure-openai|azure
        "azure_openai" | "azure-openai" | "azure" => Some("azure"),
        // xAI: was xai|grok
        "xai" | "grok" => Some("xai"),
        // Gemini: vendor product name; was gemini|google|google-gemini
        "gemini" | "google" | "google-gemini" => Some("gemini"),
        // Together: was together|together-ai
        "together" | "together-ai" => Some("together"),
        // Fireworks: was fireworks|fireworks-ai
        "fireworks" | "fireworks-ai" => Some("fireworks"),
        // Vercel AI Gateway: was vercel|vercel-ai
        "vercel" | "vercel-ai" => Some("vercel"),
        // Cloudflare AI Gateway: was cloudflare|cloudflare-ai
        "cloudflare" | "cloudflare-ai" => Some("cloudflare"),
        // NVIDIA: was nvidia|nvidia-nim|build.nvidia.com
        "nvidia" | "nvidia-nim" | "build.nvidia.com" => Some("nvidia"),
        // Bedrock: was bedrock|aws-bedrock
        "bedrock" | "aws-bedrock" => Some("bedrock"),
        // LMStudio: was lmstudio|lm-studio
        "lmstudio" | "lm-studio" => Some("lmstudio"),
        // LiteLLM: was litellm|lite-llm
        "litellm" | "lite-llm" => Some("litellm"),
        // HuggingFace: was huggingface|hf
        "huggingface" | "hf" => Some("huggingface"),
        // Yi: was yi|01ai|lingyiwanwu
        "yi" | "01ai" | "lingyiwanwu" => Some("yi"),
        // Hunyuan: was hunyuan|tencent
        "hunyuan" | "tencent" => Some("hunyuan"),
        // Qianfan/Baidu: was qianfan|baidu
        "qianfan" | "baidu" => Some("qianfan"),
        // Copilot: was copilot|github-copilot
        "copilot" | "github-copilot" => Some("copilot"),
        // OVH: was ovhcloud|ovh
        "ovhcloud" | "ovh" => Some("ovh"),
        // OpenCode: was opencode|opencode-zen, opencode-go folded as alias=go
        "opencode" | "opencode-zen" => Some("opencode"),
        // llama.cpp: was llamacpp|llama.cpp (dot in key drops)
        "llamacpp" | "llama.cpp" => Some("llamacpp"),
        // DeepMyst: was deepmyst|deep-myst
        "deepmyst" | "deep-myst" => Some("deepmyst"),
        // SiliconFlow: was siliconflow|silicon-flow
        "siliconflow" | "silicon-flow" => Some("siliconflow"),
        // DeepInfra: was deepinfra|deep-infra
        "deepinfra" | "deep-infra" => Some("deepinfra"),
        // AI21: was ai21|ai21-labs
        "ai21" | "ai21-labs" => Some("ai21"),
        // Friendli: was friendli|friendliai
        "friendli" | "friendliai" => Some("friendli"),
        // Lepton: was lepton|lepton-ai
        "lepton" | "lepton-ai" => Some("lepton"),
        // Stepfun: was stepfun|step (stepfun-intl handled below as variant)
        "stepfun" | "step" => Some("stepfun"),
        // KiloCli: was kilocli|kilo
        "kilocli" | "kilo" => Some("kilocli"),
        _ => None,
    };

    if let Some(canonical) = synonym_canonical {
        return (canonical.to_string(), incoming_alias.to_string(), extras);
    }

    // opencode-go folds under opencode as alias=go
    if raw == "opencode-go" {
        return ("opencode".to_string(), "go".to_string(), extras);
    }

    // OpenAI Codex folds under openai with wire_api=responses + requires_openai_auth=true
    if matches!(raw, "openai-codex" | "openai_codex" | "codex") {
        extras.push(("wire_api", toml::Value::String("responses".to_string())));
        extras.push(("requires_openai_auth", toml::Value::Boolean(true)));
        return ("openai".to_string(), "codex".to_string(), extras);
    }

    // claude-code folds under anthropic.claude-code (preserved from prior
    // migration; the canonical name for Anthropic's CLI variant).
    if raw == "claude-code" {
        return ("anthropic".to_string(), "claude-code".to_string(), extras);
    }

    // anthropic-custom is the V1/V2 colon-URL form for "Anthropic-API at
    // a custom URL" (the URL was already split out into `uri` above by
    // `alias_provider_models`). Folds under anthropic with alias "custom"
    // so a stock `anthropic.default` entry and an `anthropic-custom:URL`
    // entry both migrate cleanly without clobbering each other.
    if raw == "anthropic-custom" {
        return ("anthropic".to_string(), "custom".to_string(), extras);
    }

    // `custom` (the bare V2 placeholder for "user-supplied URL") folds
    // under the dedicated `custom` typed slot. Preserves the colon-URL
    // form's URI on the alias entry.
    if raw == "custom" {
        return ("custom".to_string(), incoming_alias.to_string(), extras);
    }

    // Regional + OAuth collapse for Chinese-vendor families. Each block
    // mirrors the upstream/master V2 alias-detector functions verbatim.

    // Moonshot/Kimi
    if matches!(
        raw,
        "moonshot-intl" | "moonshot-global" | "kimi-intl" | "kimi-global"
    ) {
        extras.push(("endpoint", toml::Value::String("intl".to_string())));
        return ("moonshot".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "moonshot" | "kimi" | "moonshot-cn" | "kimi-cn") {
        extras.push(("endpoint", toml::Value::String("cn".to_string())));
        return ("moonshot".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "kimi-code" | "kimi_coding" | "kimi_for_coding") {
        extras.push(("endpoint", toml::Value::String("code".to_string())));
        return ("moonshot".to_string(), incoming_alias.to_string(), extras);
    }

    // Qwen / DashScope / Bailian
    if matches!(raw, "qwen-cn" | "dashscope" | "qwen" | "dashscope-cn") {
        extras.push(("endpoint", toml::Value::String("cn".to_string())));
        return ("qwen".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(
        raw,
        "qwen-intl" | "dashscope-intl" | "qwen-international" | "dashscope-international"
    ) {
        extras.push(("endpoint", toml::Value::String("intl".to_string())));
        return ("qwen".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "qwen-us" | "dashscope-us") {
        extras.push(("endpoint", toml::Value::String("us".to_string())));
        return ("qwen".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "qwen-code" | "qwen-oauth" | "qwen_oauth") {
        extras.push(("endpoint", toml::Value::String("code".to_string())));
        extras.push(("auth_mode", toml::Value::String("oauth".to_string())));
        return ("qwen".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "bailian" | "aliyun-bailian" | "aliyun") {
        extras.push(("endpoint", toml::Value::String("cn".to_string())));
        return ("qwen".to_string(), incoming_alias.to_string(), extras);
    }

    // GLM / Zhipu
    if matches!(raw, "glm" | "zhipu" | "glm-global" | "zhipu-global") {
        extras.push(("endpoint", toml::Value::String("global".to_string())));
        return ("glm".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "glm-cn" | "zhipu-cn" | "bigmodel") {
        extras.push(("endpoint", toml::Value::String("cn".to_string())));
        return ("glm".to_string(), incoming_alias.to_string(), extras);
    }

    // Z.AI
    if matches!(raw, "zai" | "z.ai" | "zai-global" | "z.ai-global") {
        extras.push(("endpoint", toml::Value::String("global".to_string())));
        return ("zai".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "zai-cn" | "z.ai-cn") {
        extras.push(("endpoint", toml::Value::String("cn".to_string())));
        return ("zai".to_string(), incoming_alias.to_string(), extras);
    }

    // Minimax (cn/intl + oauth)
    if matches!(
        raw,
        "minimax"
            | "minimax-intl"
            | "minimax-io"
            | "minimax-global"
            | "minimax-portal"
            | "minimax-portal-global"
    ) {
        extras.push(("endpoint", toml::Value::String("intl".to_string())));
        return ("minimax".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "minimax-oauth" | "minimax-oauth-global") {
        extras.push(("endpoint", toml::Value::String("intl".to_string())));
        extras.push(("auth_mode", toml::Value::String("oauth".to_string())));
        return ("minimax".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "minimax-cn" | "minimaxi" | "minimax-portal-cn") {
        extras.push(("endpoint", toml::Value::String("cn".to_string())));
        return ("minimax".to_string(), incoming_alias.to_string(), extras);
    }
    if matches!(raw, "minimax-oauth-cn") {
        extras.push(("endpoint", toml::Value::String("cn".to_string())));
        extras.push(("auth_mode", toml::Value::String("oauth".to_string())));
        return ("minimax".to_string(), incoming_alias.to_string(), extras);
    }

    // Doubao / Volcengine
    if matches!(raw, "doubao" | "volcengine" | "ark" | "doubao-cn") {
        return ("doubao".to_string(), incoming_alias.to_string(), extras);
    }

    // gemini-cli stays as a separate slot (subprocess runtime, not a synonym)
    if raw == "gemini-cli" {
        return ("gemini_cli".to_string(), incoming_alias.to_string(), extras);
    }

    // stepfun-intl folds into stepfun with a different uri
    if matches!(raw, "stepfun-intl" | "step-intl") {
        extras.push((
            "uri",
            toml::Value::String("https://api.stepfun.com/intl/v1".to_string()),
        ));
        return ("stepfun".to_string(), incoming_alias.to_string(), extras);
    }

    // Unknown/passthrough: keep the raw key. Silent drop will happen at V3
    // deserialize if it doesn't match any typed slot — that's the migration's
    // accountability gap, intentional per #6273. Operators with truly novel
    // names (a forked custom backend) need a slot defined for it.
    (raw.to_string(), incoming_alias.to_string(), extras)
}

fn alias_provider_models(models: Option<toml::Value>) -> toml::Table {
    let flat = match models {
        Some(toml::Value::Table(t)) => t,
        _ => return toml::Table::new(),
    };
    let mut aliased = toml::Table::new();
    for (provider_id, mut config) in flat {
        // Colon-URL form like `"anthropic-custom:https://..."`: split the URL
        // out into `uri` and use only the prefix as the seed for normalization.
        let (raw_type, url) = split_colon_url_provider(&provider_id);
        if let Some(url) = url
            && let toml::Value::Table(t) = &mut config
        {
            t.entry("uri".to_string())
                .or_insert(toml::Value::String(url));
        }

        // V2 per-block `base_url` + optional `api_path` → V3 `uri` (full
        // endpoint URL). Matches the same concatenation
        // `fold_providers_globals_into_models` applies to V2 top-level
        // globals — without this, per-block [model_providers.<id>] entries
        // would survive into V3 with the unknown `base_url`/`api_path`
        // keys, and V3 deserialize silently drops them.
        if let toml::Value::Table(t) = &mut config {
            fold_base_url_api_path_into_uri(t);
        }

        let (provider_type, alias, extras) = normalize_provider_type(&raw_type, "default");

        // Inject family-specific extras (endpoint, auth_mode, wire_api,
        // requires_openai_auth, uri) onto the alias entry table — overrides
        // by the operator's own config win via .or_insert.
        if let toml::Value::Table(t) = &mut config {
            for (field, value) in extras {
                t.entry(field.to_string()).or_insert(value);
            }
        }

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
/// - `api_url` (+ optional `api_path` suffix) → `uri` (matches V3 `ModelProviderConfig.uri`)
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

    // Determine target (provider_type, alias). For colon-URL forms like
    // `"anthropic-custom:https://..."`, split the URL out of the type key so
    // the V3 reference grammar (`<type>.<alias>`) doesn't tokenize at a URL
    // dot. The URL is folded into `uri` below.
    //
    // Then run the V2-EOL provider name through `normalize_provider_type` so
    // synonym kills + regional/oauth collapses + claude_code/openai_codex
    // folds happen here too — same canonical-naming gate as
    // `alias_provider_models`. Without this, an operator with
    // `default_provider = "grok"` would land in a `grok` slot that doesn't
    // exist on V3 ModelProviders and silently disappear.
    let (target_type, target_alias, colon_url, normalized_extras) =
        match g_default_provider.as_ref().and_then(toml::Value::as_str) {
            Some(s) => {
                let (raw_type, url) = split_colon_url_provider(s);
                let (canonical, alias, extras) = normalize_provider_type(&raw_type, "default");
                (canonical, alias, url, extras)
            }
            None => match aliased_models.keys().next() {
                Some(k) => (k.clone(), "default".to_string(), None, Vec::new()),
                None => (
                    "openrouter".to_string(),
                    "default".to_string(),
                    None,
                    Vec::new(),
                ),
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

    // The colon-URL form's URL portion (split from default_provider) takes
    // precedence over the global `api_url` field — both originate from V2's
    // top-level providers block, but the colon-URL form was the more specific
    // hint when the user wrote `default_provider = "anthropic-custom:<url>"`.
    // V3's `uri` field is the full endpoint URL — concatenate any V2 `api_path`
    // suffix onto it, since `api_path` no longer exists separately.
    let base_url_source = colon_url.map(toml::Value::String).or(g_api_url);
    let uri_source = match (base_url_source, g_api_path) {
        (Some(toml::Value::String(b)), Some(toml::Value::String(p))) => {
            let trimmed_b = b.trim_end_matches('/');
            let suffix = if p.starts_with('/') {
                p
            } else {
                format!("/{p}")
            };
            Some(toml::Value::String(format!("{trimmed_b}{suffix}")))
        }
        (Some(b), _) => Some(b),
        // api_path alone, without a base, has nowhere to live in V3 — drop.
        (None, _) => None,
    };

    // Per-provider entries take precedence: only fill missing slots.
    for (target_key, source) in [
        ("api_key", g_api_key),
        ("uri", uri_source),
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

    // Inject family-specific extras (endpoint, auth_mode, wire_api,
    // requires_openai_auth, uri) from the normalize_provider_type call
    // above. Operator-set fields win — only fill missing slots.
    for (field, value) in normalized_extras {
        if !alias_table.contains_key(field) {
            alias_table.insert(field.to_string(), value);
        }
    }

    if any_value_globals {
        tracing::info!(
            target: "migration",
            "[providers] globals folded onto model_providers.{target_type}.{target_alias}"
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

/// Drop V2 `[cost.prices.*]` entries. V2 keyed pricing by composite
/// `"<provider>/<model>"` identifiers that don't carry the V3
/// `<provider_type>.<alias>` path, so any automatic remap is fragile.
/// Operators paste the rates manually under the right V3
/// `[model_providers.<type>.<alias>].pricing` block; the INFO log per
/// entry names the model id and last-known input/output rates.
fn drop_cost_prices_with_logs(prices: &toml::Table) {
    for (model_id, price) in prices {
        let (input, output) = match price.as_table() {
            Some(t) => (
                t.get("input").and_then(toml::Value::as_float),
                t.get("output").and_then(toml::Value::as_float),
            ),
            None => (None, None),
        };
        tracing::info!(
            target: "migration",
            "[cost.prices.{model_id}] dropped (V3 puts pricing on each \
             [model_providers.<type>.<alias>] block); last-known rates: \
             input={input:?} output={output:?}"
        );
    }
}

/// Synthesize one `[peer_groups.<channel_type>_<alias>]` entry from a
/// V2 channel's inbound peer-auth allow-list, and emit an INFO log.
/// The per-channel arms in [`apply_v2_to_v3_channel_folds`] each:
///
///   1. `instance.remove("<field>")` (V3 has no slot for the field —
///      strip regardless of whether the fold synthesizes a group).
///   2. Call this helper with the removed array and the channel's V3
///      `<type>.<alias>` ref so the synthesized group lands in
///      `peer_groups`.
///
/// Skip rules: empty arrays and any list containing `"*"` produce no
/// group (a peer group can't express "anyone"). Collisions with an
/// operator-authored `[peer_groups.<type>_<alias>]` are left
/// untouched.
///
/// V1/V2 had implicit single-agent semantics, so the synthesized
/// group always binds the migration-bridge `default` agent. That is
/// the *only* legitimate `default` usage in the V2→V3 fold path —
/// post-migration the operator owns peer_group membership.
fn synthesize_peer_group_from_allowlist(
    peer_groups: &mut toml::Table,
    channel_type: &str,
    channel_alias: &str,
    field_name: &str,
    allowed: toml::Value,
) {
    let toml::Value::Array(allowed) = allowed else {
        return;
    };
    let usernames: Vec<String> = allowed
        .iter()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "*")
        .map(str::to_string)
        .collect();
    if usernames.is_empty() {
        return;
    }
    let group_name = format!("{channel_type}_{channel_alias}");
    if peer_groups.contains_key(&group_name) {
        // Operator-authored group with the synthesized name wins.
        return;
    }
    let mut group_entry = toml::Table::new();
    // Channel type only (peer-groups bind to the type, not an alias).
    group_entry.insert(
        "channel".to_string(),
        toml::Value::String(channel_type.to_string()),
    );
    // V1/V2 single-agent semantics — bridge alias `default`.
    group_entry.insert(
        "agents".to_string(),
        toml::Value::Array(vec![toml::Value::String("default".to_string())]),
    );
    let external_peers: Vec<toml::Value> = usernames.into_iter().map(toml::Value::String).collect();
    group_entry.insert(
        "external_peers".to_string(),
        toml::Value::Array(external_peers),
    );
    peer_groups.insert(group_name, toml::Value::Table(group_entry));
    tracing::info!(
        target: "migration",
        "channels.{channel_type}.{channel_alias}.{field_name} folded into [peer_groups.{channel_type}_{channel_alias}]"
    );
}

/// Wrap V2 `Option<T>` channel sections into V3 `HashMap<String, T>` keyed
/// by `"default"`. Applies, per channel instance:
///
/// - **discord_history fold**: `[channels.discord_history]` →
///   `[channels.discord]` with `archive = true`. Effective `enabled` is
///   the OR of both sides so a user with only
///   `discord_history.enabled = true` still ends up with an enabled
///   merged discord block.
/// - Singular→plural fold per channel type (`discord.guild_id` →
///   `guild_ids[]`, `mattermost.channel_id` → `channel_ids[]`,
///   `reddit.subreddit` → `subreddits[]`, `signal.group_id` →
///   `group_ids[]` or `dm_only=true` for the `"dm"` sentinel).
///
/// `cli: bool` is preserved at the top-level `channels.cli`, not aliased.
fn alias_wrap_channels(channels_value: toml::Value, peer_groups: &mut toml::Table) -> toml::Table {
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

    // V3 collapses Feishu and Lark to one channel type — they share the same
    // bot framework, only the API endpoint differs (Feishu = open.feishu.cn
    // for China, Lark = open.larksuite.com for international). Stash the V2
    // [channels.feishu] block here so the alias-wrap loop processes the V2
    // [channels.lark] block normally; the stash is re-injected after the loop
    // as [channels.lark.feishu] (NOT lark.default) so two-bot deployments
    // survive without operator intervention.
    let stashed_feishu_v2 = strip_feishu_block(&mut channels_table);

    // Per-channel-type: singular→plural fold, peer-auth lift into
    // [peer_groups.<type>_default], then alias-wrap as <type>.default.
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
        fold_channel_peer_auth_into_peer_groups(ct, &mut instance, peer_groups);
        // V3 keeps the `enabled` field on every channel config — V2's
        // boolean ports through verbatim and the orchestrator gates on
        // it at registration time. Missing `enabled` deserializes to
        // `false` via `#[serde(default)]`, matching V2 semantics.
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

    // Re-inject the stashed V2 [channels.feishu] block as [channels.lark.feishu]
    // with use_feishu = true. The alias name is "feishu" — not "default" — so a
    // two-bot deployment with both [channels.lark] (international) AND
    // [channels.feishu] (CN) survives as [channels.lark.default] +
    // [channels.lark.feishu]; both bots remain reachable post-migration.
    inject_feishu_as_lark_alias(&mut new_channels, stashed_feishu_v2);

    new_channels
}

/// Pre-alias-wrap: remove the V2 `[channels.feishu]` block from `channels`
/// (so the alias-wrap loop doesn't process it) and return its body for
/// post-wrap injection as `[channels.lark.feishu]`.
fn strip_feishu_block(channels: &mut toml::Table) -> Option<toml::Table> {
    let feishu_value = channels.remove("feishu")?;
    match feishu_value {
        toml::Value::Table(t) => Some(t),
        _ => {
            tracing::warn!(
                target: "migration",
                "[channels.feishu] is not a table; dropping during fold to lark"
            );
            None
        }
    }
}

/// Post-alias-wrap: insert the stashed V2 feishu block as
/// `[channels.lark.feishu]` with `use_feishu = true`. The alias name is
/// `feishu` (not `default`) so a two-bot V2 deployment with both
/// `[channels.lark]` (international) AND `[channels.feishu]` (CN) survives as
/// two distinct V3 aliases — `lark.default` and `lark.feishu` — without
/// losing data or requiring operator intervention.
///
/// If a `lark.feishu` alias already exists in `new_channels` (impossible
/// from V2 input but cheap to defend), we do not overwrite — the existing
/// entry wins and a WARN names the dropped source.
fn inject_feishu_as_lark_alias(new_channels: &mut toml::Table, feishu_table: Option<toml::Table>) {
    let Some(mut feishu_table) = feishu_table else {
        return;
    };

    feishu_table.insert("use_feishu".to_string(), toml::Value::Boolean(true));

    let lark_entry = new_channels
        .entry("lark".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(lark_aliases) = lark_entry.as_table_mut() else {
        tracing::warn!(
            target: "migration",
            "[channels.lark] is not a table; cannot inject feishu alias"
        );
        return;
    };

    if lark_aliases.contains_key("feishu") {
        tracing::warn!(
            target: "migration",
            "[channels.lark.feishu] already exists; the V2 [channels.feishu] \
             block was dropped to avoid clobbering it. Recover the dropped \
             value from the pre-migration <config>.backup if needed."
        );
        return;
    }

    lark_aliases.insert("feishu".to_string(), toml::Value::Table(feishu_table));
    tracing::info!(
        target: "migration",
        "[channels.feishu] folded into [channels.lark.feishu] (use_feishu=true)"
    );
}

/// Fold V2 `[channels.discord_history]` into `[channels.discord]` in place.
/// Sets `archive = true`. Effective `enabled` = `discord.enabled` OR
/// `discord_history.enabled`. Existing discord keys win over history keys
/// for non-`enabled` fields (so a user-set discord.bot_token isn't
/// overwritten by history's bot_token).
///
/// When both blocks have a `bot_token` and the values **differ**, emit
/// one `WARN` line naming the source block whose token was dropped
/// (`[channels.discord_history].bot_token`) and the surviving block
/// (`[channels.discord]`). The dropped value itself is **not** logged
/// — operators recover from the pre-migration `<config>.backup`.
/// Two-bot deployments must reconfigure manually.
fn fold_discord_history(channels: &mut toml::Table) {
    let history_value = match channels.remove("discord_history") {
        Some(v) => v,
        None => return,
    };

    // Capture the conflict signal BEFORE the merge mutates either side.
    let discord_bot_token = channels
        .get("discord")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("bot_token"))
        .and_then(toml::Value::as_str)
        .map(ToString::to_string);
    let history_bot_token = history_value
        .as_table()
        .and_then(|t| t.get("bot_token"))
        .and_then(toml::Value::as_str)
        .map(ToString::to_string);
    let bot_token_conflict = match (&discord_bot_token, &history_bot_token) {
        (Some(d), Some(h)) => d != h,
        _ => false,
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
    if bot_token_conflict {
        tracing::warn!(
            target: "migration",
            "[channels.discord_history].bot_token differed from [channels.discord].bot_token; \
             the discord_history token was dropped and the discord token survives. \
             Two-bot deployments must reconfigure manually — recover the dropped value \
             from the pre-migration <config>.backup file adjacent to the migrated config."
        );
    }
}

/// Apply V2→V3 singular→plural folds:
/// `discord.guild_id` → `guild_ids[]`, `mattermost.channel_id` → `channel_ids[]`,
/// `reddit.subreddit` → `subreddits[]`, and `signal.group_id` → `group_ids[]`
/// (with the `"dm"` sentinel mapped to `dm_only=true` instead).
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

/// V2 → V3 inbound peer-auth fold per channel. Each channel that had
/// a user-allowlist field in V2 strips it from the instance and
/// synthesizes the V3 peer_group binding `default` agent to this
/// channel. Field name varies per platform; helper handles wildcard
/// / empty / collision skip rules uniformly.
///
/// Field-name table (the only place this list lives):
///
/// - Most channels: `allowed_users`
/// - iMessage:      `allowed_contacts`
/// - Signal:        `allowed_from`
/// - WhatsApp/Wati: `allowed_numbers`
/// - Linq/Email/GmailPush: `allowed_senders`
/// - Nostr:         `allowed_pubkeys`
///
/// Channels with no inbound peer-auth concept (Webhook, Reddit,
/// Bluesky, MQTT, voice_*, ClawdTalk, CLI) return `None` and the
/// function is a no-op.
fn fold_channel_peer_auth_into_peer_groups(
    channel_type: &str,
    instance: &mut toml::Table,
    peer_groups: &mut toml::Table,
) {
    let Some(field_name) = (match channel_type {
        "telegram" | "discord" | "slack" | "mattermost" | "matrix" | "nextcloud_talk" | "irc"
        | "lark" | "line" | "feishu" | "dingtalk" | "wecom" | "wechat" | "qq" | "twitter"
        | "mochat" => Some("allowed_users"),
        "imessage" => Some("allowed_contacts"),
        "signal" => Some("allowed_from"),
        "whatsapp" | "wati" => Some("allowed_numbers"),
        "linq" | "email" | "gmail_push" => Some("allowed_senders"),
        "nostr" => Some("allowed_pubkeys"),
        _ => None,
    }) else {
        return;
    };
    if let Some(allowed) = instance.remove(field_name) {
        synthesize_peer_group_from_allowlist(
            peer_groups,
            channel_type,
            "default",
            field_name,
            allowed,
        );
    }
}

/// Strip V2-specific fields from each agent and synthesize the V3 alias
/// references / per-agent profile overrides. Specifically:
///
/// - Inline brain fields (`provider`/`model`/`api_key`/`temperature`)
///   fold into a synthesized `model_providers.<provider>.agent_<id>`
///   entry; the agent gets `model_provider = "<provider>.agent_<id>"`.
/// - `max_iterations` is renamed to `max_tool_iterations` inline.
/// - `agentic` / `allowed_tools` / `timeout_secs` / `agentic_timeout_secs`
///   lift into a synthesized `runtime_profiles.agent_<id>`.
/// - `max_depth` lifts into a synthesized
///   `risk_profiles.agent_<id>.max_delegation_depth`.
/// - `skills_directory` lifts into a synthesized
///   `skill_bundles.agent_<id>.directory` and the alias is appended
///   to the agent's `skill_bundles` array.
/// - `memory_namespace` is dropped — V3 isolates memory under
///   `[agents.<alias>.memory]` instead.
/// - Every agent ends with `risk_profile` and `runtime_profile` set
///   to either a synthesized `agent_<id>` alias or `default`, with
///   the referenced profile entries guaranteed to exist (V3
///   validation rejects dangling profile refs).
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
        if let Some(toml::Value::String(raw_provider)) = provider {
            // Colon-URL form: split the URL out so the V3 outer key stays
            // dot-free and the URL lands in `uri`. Without this,
            // `split_once('.')` would tokenize at a URL dot like the one
            // inside `api.z.ai`.
            let (provider_type, colon_url) = split_colon_url_provider(&raw_provider);
            let provider_alias = format!("agent_{}", alias);
            let mut entry = toml::Table::new();
            if let Some(url) = colon_url {
                entry.insert("uri".to_string(), toml::Value::String(url));
            }
            if let Some(m) = model {
                entry.insert("model".to_string(), m);
            }
            if let Some(k) = api_key {
                entry.insert("api_key".to_string(), k);
            }
            if let Some(t) = temperature {
                entry.insert("temperature".to_string(), t);
            }
            // V3 keeps every provider category under `[providers]`:
            // `[providers.models.<type>.<alias>]` is the destination.
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

        // max_iterations → max_tool_iterations (V3 inline rename).
        if let Some(v) = agent_table.remove("max_iterations") {
            agent_table
                .entry("max_tool_iterations".to_string())
                .or_insert(v);
            tracing::info!(
                target: "migration",
                "agents.{alias}.max_iterations → agents.{alias}.max_tool_iterations"
            );
        }

        // V2 agent runtime overrides → per-agent runtime_profile.
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

        // max_depth → per-agent risk_profile.max_delegation_depth.
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

        // skills_directory → synthesize a per-agent skill_bundle and
        // append its alias to agent.skill_bundles. V3
        // SkillBundleConfig.directory carries the V2 path verbatim.
        if let Some(toml::Value::String(skills_dir)) = agent_table.remove("skills_directory")
            && !skills_dir.is_empty()
        {
            let bundle_alias = format!("agent_{}", alias);
            let mut bundle_entry = toml::Table::new();
            bundle_entry.insert("directory".to_string(), toml::Value::String(skills_dir));
            install_profile_entry(passthrough, "skill_bundles", &bundle_alias, bundle_entry);
            // V3 AliasedAgentConfig.skill_bundles is Vec<String> of aliases.
            // Append our synthesized bundle alias (preserve any user-set list).
            let existing = agent_table
                .remove("skill_bundles")
                .and_then(|v| match v {
                    toml::Value::Array(a) => Some(a),
                    _ => None,
                })
                .unwrap_or_default();
            let mut new_list = existing;
            let already = new_list
                .iter()
                .any(|v| v.as_str() == Some(bundle_alias.as_str()));
            if !already {
                new_list.push(toml::Value::String(bundle_alias.clone()));
            }
            agent_table.insert("skill_bundles".to_string(), toml::Value::Array(new_list));
            tracing::info!(
                target: "migration",
                "agents.{alias}.skills_directory → [skill_bundles.{bundle_alias}] (referenced \
                 from agents.{alias}.skill_bundles)"
            );
        }

        // Every V3 agent must reference a configured risk_profile and
        // runtime_profile. For agents that didn't trigger the
        // per-agent synthesis above, fall back to "default" and ensure
        // both entries exist (V3 rejects dangling profile refs).
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

        // V3 retired the V2 `memory_namespace` field on agents (and the
        // top-level [memory_namespaces.<alias>] section it referenced)
        // when per-agent memory backends landed under
        // [agents.<alias>.memory]. Drop the V2 key so it doesn't carry
        // through to the V3 deserialization step.
        agent_table.remove("memory_namespace");

        new_agents.insert(alias, toml::Value::Table(agent_table));
    }
    new_agents
}

/// Pull V2 `AliasedAgentConfig` fields that V3 moved onto
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

/// Fold V2 `base_url` (+ optional `api_path`) into V3 `uri` on a single
/// `[model_providers.<type>.<alias>]` entry table. No-op when `uri` is
/// already set (operator wins) or when `base_url` is absent. Matches the
/// top-level-globals fold so both V1/V2 entry points produce the same
/// V3 shape.
fn fold_base_url_api_path_into_uri(entry: &mut toml::Table) {
    if entry.contains_key("uri") {
        // Operator-set V3 key wins; drop stale V2 spellings so V3
        // deserialize doesn't see unknown fields.
        entry.remove("base_url");
        entry.remove("api_path");
        return;
    }
    let base = match entry.remove("base_url") {
        Some(toml::Value::String(s)) if !s.is_empty() => s,
        _ => {
            // No base_url to fold. api_path alone has nowhere to live.
            entry.remove("api_path");
            return;
        }
    };
    let path = match entry.remove("api_path") {
        Some(toml::Value::String(p)) if !p.is_empty() => Some(p),
        _ => None,
    };
    let uri = match path {
        Some(p) => {
            let trimmed = base.trim_end_matches('/');
            let suffix = if p.starts_with('/') {
                p
            } else {
                format!("/{p}")
            };
            format!("{trimmed}{suffix}")
        }
        None => base,
    };
    entry.insert("uri".to_string(), toml::Value::String(uri));
}

/// Rewrite any `peer_groups.<X>.agents = ["default"]` entries to point at
/// a real agent alias when `agents.default` doesn't exist. Step 7
/// synthesizes peer_groups with the bridge alias `"default"` before
/// step 8 decides what the actual agent map looks like; this post-pass
/// patches up the dangling reference in the multi-agent V2 case where
/// `agents.default` is never created.
///
/// Also injects the peer_group's channel ref into the chosen agent's
/// `channels` list. V3 validation rejects an agent listed in a peer_group
/// for a channel it doesn't own (`agents.<X>.channels` must contain the
/// peer_group's channel); V2 had no per-agent channel binding, so the
/// migration extends the chosen agent's reach to cover what V2's implicit
/// single-agent semantics expected.
///
/// No-op when `agents.default` exists (the bridge alias is valid) or
/// when the agents map is empty (no fix possible — the operator will
/// hit a different validation error). Operator-authored peer_groups
/// whose agents list isn't exactly `["default"]` are left untouched.
fn rewrite_dangling_peer_group_agents(passthrough: &mut toml::Table) {
    let replacement_alias = {
        let Some(agents_table) = passthrough.get("agents").and_then(toml::Value::as_table) else {
            return;
        };
        if agents_table.is_empty() || agents_table.contains_key("default") {
            return;
        }
        let Some(alias) = agents_table.keys().next().cloned() else {
            return;
        };
        alias
    };

    let mut rewritten_channels: Vec<String> = Vec::new();
    {
        let Some(toml::Value::Table(peer_groups)) = passthrough.get_mut("peer_groups") else {
            return;
        };
        for (group_name, group_value) in peer_groups.iter_mut() {
            let Some(group_table) = group_value.as_table_mut() else {
                continue;
            };
            let Some(toml::Value::Array(agents_arr)) = group_table.get("agents") else {
                continue;
            };
            let only_default = agents_arr.len() == 1 && agents_arr[0].as_str() == Some("default");
            if !only_default {
                continue;
            }
            group_table.insert(
                "agents".to_string(),
                toml::Value::Array(vec![toml::Value::String(replacement_alias.clone())]),
            );
            if let Some(toml::Value::String(channel_ref)) = group_table.get("channel") {
                rewritten_channels.push(channel_ref.clone());
            }
            tracing::info!(
                target: "migration",
                "peer_groups.{group_name}.agents rewritten from [\"default\"] to [{replacement_alias:?}] (no agents.default exists)"
            );
        }
    }

    if rewritten_channels.is_empty() {
        return;
    }
    let Some(toml::Value::Table(agents_table)) = passthrough.get_mut("agents") else {
        return;
    };
    let Some(toml::Value::Table(agent_entry)) = agents_table.get_mut(&replacement_alias) else {
        return;
    };
    let channels_array = agent_entry
        .entry("channels".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let Some(channels_arr) = channels_array.as_array_mut() else {
        return;
    };
    let mut added: Vec<String> = Vec::new();
    for ch in &rewritten_channels {
        let present = channels_arr.iter().any(|v| v.as_str() == Some(ch.as_str()));
        if !present {
            channels_arr.push(toml::Value::String(ch.clone()));
            added.push(ch.clone());
        }
    }
    if !added.is_empty() {
        tracing::info!(
            target: "migration",
            "agents.{replacement_alias}.channels extended with {added:?} so the rewritten peer_groups resolve"
        );
    }
}

/// V2 → V3 backfill: when `[heartbeat] enabled = true` and `agent` is
/// unset/empty, set `agent` to a configured agent alias. Picks `"default"`
/// when present (matching the synthesized-default-agent path), otherwise
/// the first agent in the table. No-op when `agents` is empty or
/// `heartbeat.agent` is already set (operator wins).
fn backfill_heartbeat_agent(passthrough: &mut toml::Table) {
    let needs_backfill = passthrough
        .get("heartbeat")
        .and_then(toml::Value::as_table)
        .is_some_and(|hb| {
            let enabled = hb
                .get("enabled")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            let agent_set = hb
                .get("agent")
                .and_then(toml::Value::as_str)
                .is_some_and(|s| !s.trim().is_empty());
            enabled && !agent_set
        });
    if !needs_backfill {
        return;
    }
    let alias = passthrough
        .get("agents")
        .and_then(toml::Value::as_table)
        .and_then(|agents| {
            if agents.contains_key("default") {
                Some("default".to_string())
            } else {
                agents.keys().next().cloned()
            }
        });
    let Some(alias) = alias else {
        return;
    };
    if let Some(toml::Value::Table(hb)) = passthrough.get_mut("heartbeat") {
        hb.insert("agent".to_string(), toml::Value::String(alias.clone()));
        tracing::info!(
            target: "migration",
            "heartbeat.agent unset with heartbeat.enabled = true → backfilled to {alias:?}"
        );
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

/// Lift the top-level `[identity]` table into each `[agents.<alias>.identity]`
/// during V2 → V3. V3 demoted identity to a per-agent block; leaving the
/// V2 top-level key intact would surface as an unknown field on the V3
/// deserializer. Operators who already wrote a per-agent identity block
/// keep it (no clobber). If no agents are present after the fold, the
/// top-level block is dropped with a warn (lossy but intentional — V3
/// has no other slot for it).
fn lift_top_level_identity_into_agents(passthrough: &mut toml::Table) {
    let Some(identity_value) = passthrough.remove("identity") else {
        return;
    };
    let Some(agents_value) = passthrough.get_mut("agents") else {
        tracing::warn!(
            target: "migration",
            "[identity] dropped during V2->V3 (no [agents] table to attach to)"
        );
        return;
    };
    let Some(agents_table) = agents_value.as_table_mut() else {
        return;
    };
    if agents_table.is_empty() {
        tracing::warn!(
            target: "migration",
            "[identity] dropped during V2->V3 (agents map empty after fold)"
        );
        return;
    }
    let aliases: Vec<String> = agents_table.keys().cloned().collect();
    let mut folded = 0usize;
    for alias in &aliases {
        let Some(agent_table) = agents_table
            .get_mut(alias)
            .and_then(toml::Value::as_table_mut)
        else {
            continue;
        };
        if agent_table.contains_key("identity") {
            continue;
        }
        agent_table.insert("identity".to_string(), identity_value.clone());
        folded += 1;
    }
    tracing::info!(
        target: "migration",
        "[identity] lifted into [agents.<alias>.identity] ({folded} agent(s))",
    );
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
    // V3 keeps every provider category under `[providers]`:
    // `[providers.models.<type>.<alias>]`. Walk in via the new path.
    let models = match passthrough
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get("models"))
        .and_then(toml::Value::as_table)
    {
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

/// Promote V2 `[tts.<type>]` per-provider sub-blocks into V3's unified
/// `[tts_providers.<type>.default]` alias map.
///
/// V2 `TtsConfig` had a separate `Option<*TtsConfig>` field per provider
/// (`openai`, `elevenlabs`, `google`, `edge`, `piper`); V3 keys them all
/// by `<type>.<alias>` like the model providers. `[tts]` top-level
/// scalars (`enabled`, `default_voice`, `default_format`,
/// `max_text_length`) stay on `[tts]`; `default_provider` is dropped —
/// V3 has no global default TTS provider.
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

    if tts_table.remove("default_provider").is_some() {
        tracing::info!(
            target: "migration",
            "[tts].default_provider dropped (V3 has no global default-provider; set agent.<X>.tts_provider instead)"
        );
    }

    if !tts_aliased.is_empty() {
        new_providers.insert("tts".to_string(), toml::Value::Table(tts_aliased));
        tracing::info!(
            target: "migration",
            "[tts.<type>] sub-blocks promoted to [tts_providers.<type>.default]"
        );
    }
}

/// Fold V2 `[transcription]` flat block + per-family sub-blocks into V3's
/// typed `[transcription_providers.<family>.<alias>]` shape. The Groq
/// fields lived directly on `[transcription]` in V2 (api_key, api_url,
/// model, language, initial_prompt) — they migrate to
/// `[transcription_providers.groq.default]`. Per-family sub-blocks
/// (`[transcription.openai]`, etc.) migrate to
/// `[transcription_providers.<family>.default]`.
///
/// Behavior fields (`enabled`, `transcribe_non_ptt_audio`,
/// `max_duration_secs`) stay on `[transcription]`. Legacy default-provider
/// keys (`default_provider`, `default_model_provider`,
/// `default_transcription_provider`) are dropped — V3 has no global
/// default; per-agent `transcription_provider` is the only selector.
fn fold_v2_transcription_into_providers(
    passthrough: &mut toml::Table,
    new_providers: &mut toml::Table,
) {
    let Some(toml::Value::Table(transcription_table)) = passthrough.get_mut("transcription") else {
        return;
    };

    let mut transcription_aliased = toml::Table::new();

    // Per-family sub-blocks: move to transcription_providers.<family>.default.
    const V3_TRANSCRIPTION_FAMILIES: &[&str] = &[
        "openai",
        "deepgram",
        "assemblyai",
        "google",
        "local_whisper",
    ];
    for family in V3_TRANSCRIPTION_FAMILIES {
        if let Some(value) = transcription_table.remove(*family) {
            let mut wrapped = toml::Table::new();
            wrapped.insert("default".to_string(), value);
            transcription_aliased.insert((*family).to_string(), toml::Value::Table(wrapped));
        }
    }

    // Groq lived directly on [transcription] in V2. Extract its fields into
    // [transcription_providers.groq.default] so V3 can find it via the typed
    // family slot. Pulled fields: api_key, api_url, model, language,
    // initial_prompt. Behavior fields (enabled, transcribe_non_ptt_audio,
    // max_duration_secs) stay on [transcription].
    let mut groq_entry = toml::Table::new();
    for groq_field in &["api_key", "api_url", "model", "language", "initial_prompt"] {
        if let Some(v) = transcription_table.remove(*groq_field) {
            groq_entry.insert((*groq_field).to_string(), v);
        }
    }
    if !groq_entry.is_empty() {
        let mut wrapped = toml::Table::new();
        wrapped.insert("default".to_string(), toml::Value::Table(groq_entry));
        transcription_aliased.insert("groq".to_string(), toml::Value::Table(wrapped));
        tracing::info!(
            target: "migration",
            "[transcription] Groq fields promoted to [transcription_providers.groq.default]"
        );
    }

    // Drop legacy default-provider keys — V3 has no global default-provider
    // field. Operators select transcription per agent
    // (`agent.<X>.transcription_provider`).
    for legacy_default in &[
        "default_provider",
        "default_model_provider",
        "default_transcription_provider",
    ] {
        if transcription_table.remove(*legacy_default).is_some() {
            tracing::info!(
                target: "migration",
                "[transcription].{legacy_default} dropped (V3 has no global default-provider; set agent.<X>.transcription_provider instead)"
            );
        }
    }

    if !transcription_aliased.is_empty() {
        // Merge into existing providers.transcription if any (operator may
        // have written V3-style entries already).
        let providers_transcription = new_providers
            .entry("transcription".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(existing) = providers_transcription.as_table_mut() {
            for (family, value) in transcription_aliased {
                existing.entry(family).or_insert(value);
            }
        }
        tracing::info!(
            target: "migration",
            "[transcription.<family>] sub-blocks promoted to [transcription_providers.<family>.default]"
        );
    }
}

/// Rename each route entry's V2 `provider` field to V3 `model_provider`.
/// Applies to `[providers.<routes_key>]` for `model_routes` and
/// `embedding_routes`. Bare provider names get promoted to the V3 dotted
/// form (`"openai"` → `"openai.default"`) so the dangling-reference
/// validator sees a real `model_providers.<type>.<alias>` reference.
fn rename_route_provider_field(new_providers: &mut toml::Table, routes_key: &str) {
    let Some(toml::Value::Array(routes)) = new_providers.get_mut(routes_key) else {
        return;
    };
    let mut renamed = 0usize;
    let mut promoted = 0usize;
    for entry in routes.iter_mut() {
        let toml::Value::Table(t) = entry else {
            continue;
        };
        if t.contains_key("model_provider") {
            // Already V3-shaped (operator wrote `model_provider` directly,
            // or migration ran twice). Drop a stray `provider` if present
            // so downstream serde doesn't trip on an unknown field.
            t.remove("provider");
        } else if let Some(value) = t.remove("provider") {
            t.insert("model_provider".to_string(), value);
            renamed += 1;
        }
        // V3's `model_provider` is a dotted alias (`<type>.<alias>`). V2
        // wrote a bare provider type (e.g. `"openai"`); promote it to
        // `"openai.default"` so V3 deserialize and the dangling-reference
        // validator both see a real `model_providers.<type>.<alias>` ref.
        if let Some(toml::Value::String(s)) = t.get_mut("model_provider")
            && !s.is_empty()
            && !s.contains('.')
        {
            *s = format!("{s}.default");
            promoted += 1;
        }
    }
    if renamed > 0 {
        tracing::info!(
            target: "migration",
            "[providers.{routes_key}] {renamed} entry/entries: `provider` field renamed to `model_provider`"
        );
    }
    if promoted > 0 {
        tracing::info!(
            target: "migration",
            "[providers.{routes_key}] {promoted} entry/entries: bare `model_provider` promoted to dotted `<type>.default` form"
        );
    }
}

/// Fold V2 `[memory.qdrant]`, `[memory.postgres]`, and
/// `[storage.provider.config]` into V3 `[storage.<backend>.<alias>]`. V3
/// unified V2's three storage sources under one typed map per backend:
///
/// - `[memory.qdrant]` → `[storage.qdrant.default]` (same field names).
/// - `[memory.postgres]` contributes only `vector_enabled` and
///   `vector_dimensions`; the remaining `db_url`, `schema`, `table`
///   come from `[storage.provider.config]` when the operator set
///   `provider = "postgres"` there.
/// - `[storage.provider.config]`'s `provider` field selects the V3
///   backend; remaining fields are adapted per-backend (sqlite extracts
///   path from a `sqlite://...` URL; qdrant maps `db_url` → `url`;
///   postgres maps directly).
/// - `[memory].sqlite_open_timeout_secs` lifts onto
///   `[storage.sqlite.default].open_timeout_secs`.
///
/// Operator-authored V3-shaped entries take precedence over the fold.
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

/// Fold V2 `[security.sandbox]` and `[security.resources]` into
/// `risk_profiles.default`. V3 `RiskProfileConfig` absorbed both,
/// placing them per-profile so each agent can override.
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
