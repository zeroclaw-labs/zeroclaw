use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

pub const V3_CHANNEL_TYPES: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "mattermost",
    "msteams",
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
    "voice_duplex",
    "mqtt",
    "amqp",
    "filesystem",
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

        if let Some(autonomy_value) = autonomy {
            let renamed = rename_table_keys(
                autonomy_value,
                &[("non_cli_excluded_tools", "excluded_tools")],
            );
            let (risk_fields, runtime_fields) = split_autonomy_into_profile_buckets(renamed);
            if let Some(risk_table) = risk_fields {
                let mut risk_profiles = passthrough
                    .remove("risk_profiles")
                    .and_then(|v| v.try_into::<toml::Table>().ok())
                    .unwrap_or_default();
                merge_into_profile_default(&mut risk_profiles, risk_table);
                passthrough.insert(
                    "risk_profiles".to_string(),
                    toml::Value::Table(risk_profiles),
                );
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "[autonomy] authorization fields → [risk_profiles.default]"
                );
            }
            if let Some(runtime_table) = runtime_fields {
                let mut runtime_profiles = passthrough
                    .remove("runtime_profiles")
                    .and_then(|v| v.try_into::<toml::Table>().ok())
                    .unwrap_or_default();
                merge_into_profile_default(&mut runtime_profiles, runtime_table);
                passthrough.insert(
                    "runtime_profiles".to_string(),
                    toml::Value::Table(runtime_profiles),
                );
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "[autonomy] budget/timeout fields → [runtime_profiles.default]"
                );
            }
        }

        fold_security_into_risk_profile(&mut passthrough);

        if let Some(toml::Value::Table(mut agent_table)) = agent {
            let allowed_tools = agent_table.remove("allowed_tools");
            if let Some(at_value) = allowed_tools {
                let mut risk_profiles = passthrough
                    .remove("risk_profiles")
                    .and_then(|v| v.try_into::<toml::Table>().ok())
                    .unwrap_or_default();
                let mut risk_default = toml::Table::new();
                risk_default.insert("allowed_tools".to_string(), at_value);
                merge_into_profile_default(&mut risk_profiles, risk_default);
                passthrough.insert(
                    "risk_profiles".to_string(),
                    toml::Value::Table(risk_profiles),
                );
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "[agent.allowed_tools] → [risk_profiles.default.allowed_tools]"
                );
            }
            if !agent_table.is_empty() {
                let mut runtime_profiles = passthrough
                    .remove("runtime_profiles")
                    .and_then(|v| v.try_into::<toml::Table>().ok())
                    .unwrap_or_default();
                merge_into_profile_default(&mut runtime_profiles, agent_table);
                passthrough.insert(
                    "runtime_profiles".to_string(),
                    toml::Value::Table(runtime_profiles),
                );
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "[agent] → [runtime_profiles.default]"
                );
            }
        }

        // V3 dropped swarms.
        if !swarms.is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("[swarms] dropped ({} entries)", swarms.len())
            );
        }

        // V3 eradicated provider fallback. Strip the V2 reliability
        // fields that referenced it; the rest of [reliability] stays.
        if let Some(toml::Value::Table(reliability_table)) = passthrough.get_mut("reliability") {
            let dropped_fb = reliability_table.remove("fallback_providers").is_some();
            let dropped_mf = reliability_table.remove("model_fallbacks").is_some();
            if dropped_fb || dropped_mf {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "providers.fallback eradicated"
            );
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "[providers] residual keys dropped during V3 hoist: {:?}",
                    new_providers.keys().collect::<Vec<_>>()
                )
            );
        }
        if let Some(remaining_cost) = cost_passthrough {
            passthrough.insert("cost".to_string(), remaining_cost);
        }

        // V2 [memory.qdrant], [memory.postgres], and [storage.provider.config]
        // all collapse into V3 [storage.<backend>.<alias>].
        fold_v2_storage_subsystems(&mut passthrough);

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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "[channels] sections alias-wrapped, discord_history folded, inbound peer-auth folded into [peer_groups.*]"
            );
        }

        if let Some(cron_value) = cron {
            let (new_cron, scheduler_extras) = restructure_cron(cron_value);
            if !new_cron.is_empty() {
                passthrough.insert("cron".to_string(), toml::Value::Table(new_cron));
            }
            if !scheduler_extras.is_empty() {
                merge_into_table(&mut passthrough, "scheduler", scheduler_extras);
            }
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "[cron] restructured into [cron.<alias>] + [scheduler]"
            );
        }

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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({"parent": parent, "inner": inner, "replacement": replacement})
            ),
            &format!(
                "[{parent}].{inner} renamed to [{parent}].{replacement} (V3 qualified-noun rename)"
            )
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
                    dot_delivery_channel(&mut t);
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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "[cron] had unmodeled keys: {:?}",
                cron_table.keys().collect::<Vec<_>>()
            )
        );
    }

    (new_cron, scheduler_extras)
}

fn dot_delivery_channel(job: &mut toml::Table) {
    let Some(toml::Value::Table(delivery)) = job.get_mut("delivery") else {
        return;
    };
    let Some(toml::Value::String(channel)) = delivery.get_mut("channel") else {
        return;
    };
    if !channel.contains('.') {
        *channel = format!("{channel}.default");
    }
}

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
        // Lambda AI: lambda_ai|lambda-ai
        "lambda_ai" | "lambda-ai" => Some("lambda_ai"),
        // GitHub Models: github_models|github-models
        "github_models" | "github-models" => Some("github_models"),
        // Stepfun: was stepfun|step (stepfun-intl handled below as variant)
        "stepfun" | "step" => Some("stepfun"),
        // KiloCli: was kilocli|kilo-cli
        "kilocli" | "kilo-cli" => Some("kilocli"),
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
    // accountability gap, intentional Operators with truly novel
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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({"target_type": target_type, "target_alias": target_alias})
            ),
            "[providers] globals folded onto model_providers.."
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

fn drop_cost_prices_with_logs(prices: &toml::Table) {
    for (model_id, price) in prices {
        let (input, output) = match price.as_table() {
            Some(t) => (
                t.get("input").and_then(toml::Value::as_float),
                t.get("output").and_then(toml::Value::as_float),
            ),
            None => (None, None),
        };
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({"model_id": model_id, "input": format!("{:?}", input), "output": format!("{:?}", output)})
            ),
            &format!(
                "[cost.prices.{model_id}] dropped (V3 puts pricing on each \
                 [model_providers.<type>.<alias>] block); last-known rates: \
                 input={input:?} output={output:?}"
            )
        );
    }
}

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
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({"channel_type": channel_type, "channel_alias": channel_alias, "field_name": field_name})
        ),
        &format!(
            "channels.{channel_type}.{channel_alias}.{field_name} folded into [peer_groups.{channel_type}_{channel_alias}]"
        )
    );
}

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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "[channels] passthrough for unmodeled keys: {:?}",
                leftover_keys
            )
        );
        for (k, v) in channels_table {
            new_channels.insert(k, v);
        }
    }

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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "[channels.feishu] is not a table; dropping during fold to lark"
            );
            None
        }
    }
}

fn inject_feishu_as_lark_alias(new_channels: &mut toml::Table, feishu_table: Option<toml::Table>) {
    let Some(mut feishu_table) = feishu_table else {
        return;
    };

    feishu_table.insert("use_feishu".to_string(), toml::Value::Boolean(true));

    let lark_entry = new_channels
        .entry("lark".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(lark_aliases) = lark_entry.as_table_mut() else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "[channels.lark] is not a table; cannot inject feishu alias"
        );
        return;
    };

    if lark_aliases.contains_key("feishu") {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "[channels.lark.feishu] already exists; the V2 [channels.feishu] \
             block was dropped to avoid clobbering it. Recover the dropped \
             value from the pre-migration <config>.backup if needed."
        );
        return;
    }

    lark_aliases.insert("feishu".to_string(), toml::Value::Table(feishu_table));
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "[channels.feishu] folded into [channels.lark.feishu] (use_feishu=true)"
    );
}

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
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"effective_enabled": effective_enabled})),
        "[channels.discord_history] folded into [channels.discord] (archive=true, effective enabled=)"
    );
    if bot_token_conflict {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
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
        "discord" if fold_string_into_array(instance, "guild_id", "guild_ids") => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "channels.discord.guild_id folded into channels.discord.guild_ids[]"
            );
        }
        "mattermost" if fold_string_into_array(instance, "channel_id", "channel_ids") => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "channels.mattermost.channel_id folded into channels.mattermost.channel_ids[]"
            );
        }
        "reddit" if fold_string_into_array(instance, "subreddit", "subreddits") => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "channels.reddit.subreddit folded into channels.reddit.subreddits[]"
            );
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
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
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
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "channels.signal.group_id folded into channels.signal.group_ids[]"
                    );
                }
            }
        }
        _ => {}
    }
}

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

        // Brain fold: provider/model/api_key/temperature/timeout_secs →
        // model-provider alias. V2's per-agent `timeout_secs` was the HTTP
        // timeout for LLM calls; V3 hangs it off the model_provider entry,
        // not the agent.
        let provider = agent_table.remove("provider");
        let model = agent_table.remove("model");
        let api_key = agent_table.remove("api_key");
        let temperature = agent_table.remove("temperature");
        let provider_timeout_secs = extract_provider_timeout_secs(&mut agent_table);
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
            if let Some(t) = provider_timeout_secs {
                entry.insert("timeout_secs".to_string(), t);
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
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"alias": alias, "provider_type": provider_type, "provider_alias": provider_alias})), "agents.: inline brain → providers.models..");
        } else {
            // No provider declared but operator still set timeout_secs;
            // drop it rather than silently storing on the agent block,
            // since V3 has no agent-level slot for it.
            if provider_timeout_secs.is_some() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"alias": alias})),
                    "agents..timeout_secs dropped: V3 stores it on \
                     [model_providers.<type>.<alias>] and this agent has no \
                     inline provider to fold it onto. Set it manually after \
                     migration."
                );
            }
            if let Some(other) = provider {
                agent_table.insert("provider".to_string(), other);
            }
        }

        // max_iterations lifts into the synthesized per-agent runtime
        // profile as max_tool_iterations.
        let max_iterations = agent_table
            .remove("max_iterations")
            .or_else(|| agent_table.remove("max_tool_iterations"));

        let allowed_tools = agent_table.remove("allowed_tools");
        let agentic_flag = agent_table.remove("agentic");
        let max_depth = agent_table.remove("max_depth");
        let agentic_timeout_secs = extract_agentic_timeout_secs(&mut agent_table);

        let profile_alias = format!("agent_{}", alias);

        if let Some(at_value) = allowed_tools {
            let mut overrides = toml::Table::new();
            overrides.insert("allowed_tools".to_string(), at_value);
            install_profile_entry(passthrough, "risk_profiles", &profile_alias, overrides);
            agent_table
                .entry("risk_profile".to_string())
                .or_insert_with(|| toml::Value::String(profile_alias.clone()));
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"alias": alias, "profile_alias": profile_alias})
                    ),
                "agents..allowed_tools → risk_profiles..allowed_tools"
            );
        }

        if agentic_flag.is_some()
            || max_depth.is_some()
            || agentic_timeout_secs.is_some()
            || max_iterations.is_some()
        {
            let mut overrides = toml::Table::new();
            if let Some(v) = agentic_flag {
                overrides.insert("agentic".to_string(), v);
            }
            if let Some(d) = max_depth {
                overrides.insert("max_delegation_depth".to_string(), d);
            }
            if let Some(t) = agentic_timeout_secs {
                overrides.insert("agentic_timeout_secs".to_string(), t);
            }
            if let Some(mi) = max_iterations {
                overrides.insert("max_tool_iterations".to_string(), mi);
            }
            install_profile_entry(passthrough, "runtime_profiles", &profile_alias, overrides);
            agent_table
                .entry("runtime_profile".to_string())
                .or_insert_with(|| toml::Value::String(profile_alias.clone()));
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"alias": alias, "profile_alias": profile_alias})
                    ),
                "agents.: agentic/max_depth/agentic_timeout_secs/max_iterations → runtime_profiles."
            );
        }

        if let Some(toml::Value::String(skills_dir)) = agent_table.remove("skills_directory")
            && !skills_dir.is_empty()
        {
            let bundle_alias = format!("agent_{}", alias);
            let mut bundle_entry = toml::Table::new();
            let trimmed = skills_dir.trim().trim_start_matches("./");
            let stays_inside_shared = !std::path::Path::new(trimmed).is_absolute()
                && (trimmed == "shared" || trimmed.starts_with("shared/"));
            if stays_inside_shared {
                bundle_entry.insert("directory".to_string(), toml::Value::String(skills_dir));
            } else {
                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"alias": alias, "skills_dir": skills_dir, "bundle_alias": bundle_alias})), "agents..skills_directory = \"\" lies outside \
                     <install>/shared/. V3 confines skill-bundles to \
                     <install>/shared/skills/<alias>/; the path was dropped and the bundle \
                     falls back to the default. Copy the V2 skill files into \
                     <install>/shared/skills// to restore them.");
            }
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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"alias": alias, "bundle_alias": bundle_alias})
                    ),
                "agents..skills_directory → [skill_bundles.] (referenced \
                 from agents..skill_bundles)"
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

        agent_table.remove("memory_namespace");

        new_agents.insert(alias, toml::Value::Table(agent_table));
    }
    new_agents
}

/// Pull V2 `[agents.<alias>].agentic_timeout_secs` off the agent table
/// and hand it to the caller for routing onto the synthesized
/// `runtime_profiles.agent_<alias>.agentic_timeout_secs`.
fn extract_agentic_timeout_secs(agent: &mut toml::Table) -> Option<toml::Value> {
    agent.remove("agentic_timeout_secs")
}

/// Pull V2 `[agents.<alias>].timeout_secs` off the agent table; the
/// caller folds this into the agent's resolved model_provider entry.
fn extract_provider_timeout_secs(agent: &mut toml::Table) -> Option<toml::Value> {
    agent.remove("timeout_secs")
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

    let mut rewritten_channel_types: Vec<String> = Vec::new();
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
                rewritten_channel_types.push(channel_ref.clone());
            }
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"group_name": group_name, "replacement_alias": format!("{:?}", replacement_alias)})), "peer_groups..agents rewritten from [\"default\"] to [] (no agents.default exists)");
        }
    }

    if rewritten_channel_types.is_empty() {
        return;
    }

    let mut resolved_refs: Vec<String> = Vec::new();
    if let Some(toml::Value::Table(channels_table)) = passthrough.get("channels") {
        for channel_type in &rewritten_channel_types {
            let aliases = channels_table
                .get(channel_type)
                .and_then(toml::Value::as_table)
                .map(|t| t.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for alias in aliases {
                let dotted = format!("{channel_type}.{alias}");
                if !resolved_refs.contains(&dotted) {
                    resolved_refs.push(dotted);
                }
            }
        }
    }
    if resolved_refs.is_empty() {
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
    for ch in &resolved_refs {
        let present = channels_arr.iter().any(|v| v.as_str() == Some(ch.as_str()));
        if !present {
            channels_arr.push(toml::Value::String(ch.clone()));
            added.push(ch.clone());
        }
    }
    if !added.is_empty() {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"replacement_alias": replacement_alias, "added": format!("{:?}", added)})), "agents..channels extended with  so the rewritten peer_groups resolve");
    }
}

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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"alias": format!("{:?}", alias)})),
            &format!(
                "heartbeat.agent unset with heartbeat.enabled = true → backfilled to {alias:?}"
            )
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

fn lift_top_level_identity_into_agents(passthrough: &mut toml::Table) {
    let Some(identity_value) = passthrough.remove("identity") else {
        return;
    };
    let Some(agents_value) = passthrough.get_mut("agents") else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "[identity] dropped during V2->V3 (no [agents] table to attach to)"
        );
        return;
    };
    let Some(agents_table) = agents_value.as_table_mut() else {
        return;
    };
    if agents_table.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
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
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"folded": folded})),
        &format!("[identity] lifted into [agents.<alias>.identity] ({folded} agent(s))")
    );
}

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
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "synthesized [agents.default] from V1/V2 implicit single-agent semantics"
    );
    agents
}

/// V3 TTS provider type keys. Matches the V2 `TtsConfig` per-provider
/// option fields.
const V3_TTS_TYPES: &[&str] = &["openai", "elevenlabs", "google", "edge", "piper"];

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
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "tts.elevenlabs.model_id renamed to tts.elevenlabs.model"
                );
            }
            let mut wrapped = toml::Table::new();
            wrapped.insert("default".to_string(), value);
            tts_aliased.insert((*ty).to_string(), toml::Value::Table(wrapped));
        }
    }

    if tts_table.remove("default_provider").is_some() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "[tts].default_provider dropped (V3 has no global default-provider; set agent.<X>.tts_provider instead)"
        );
    }

    if !tts_aliased.is_empty() {
        new_providers.insert("tts".to_string(), toml::Value::Table(tts_aliased));
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "[tts.<type>] sub-blocks promoted to [tts_providers.<type>.default]"
        );
    }
}

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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"legacy_default": legacy_default})),
                &format!(
                    "[transcription].{legacy_default} dropped (V3 has no global default-provider; set agent.<X>.transcription_provider instead)"
                )
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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "[transcription.<family>] sub-blocks promoted to [transcription_providers.<family>.default]"
        );
    }
}

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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"routes_key": routes_key, "renamed": renamed})),
            "[providers.]  entry/entries: `provider` field renamed to `model_provider`"
        );
    }
    if promoted > 0 {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"routes_key": routes_key, "promoted": promoted})),
            "[providers.]  entry/entries: bare `model_provider` promoted to dotted `<type>.default` form"
        );
    }
}

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
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "[memory.qdrant] promoted to [storage.qdrant.default]"
        );
    }
    if let Some(timeout_value) = memory_sqlite_timeout {
        let mut sqlite_fields = toml::Table::new();
        sqlite_fields.insert("open_timeout_secs".to_string(), timeout_value);
        merge_storage_default(storage_table, "sqlite", sqlite_fields);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "memory.sqlite_open_timeout_secs → [storage.sqlite.default].open_timeout_secs"
        );
    }
    if let Some(toml::Value::Table(postgres_vector_data)) = memory_postgres {
        merge_storage_default(storage_table, "postgres", postgres_vector_data);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
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
            _ => {
                drop_empty_subsystem_blocks(passthrough);
                return;
            }
        };
        if config_table.is_empty() {
            drop_empty_subsystem_blocks(passthrough);
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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"provider_type": provider_type})),
                "[storage.provider.config provider=] promoted to [storage..default]"
            );
        }
    }

    drop_empty_subsystem_blocks(passthrough);
}

fn drop_empty_subsystem_blocks(passthrough: &mut toml::Table) {
    for key in ["memory", "storage"] {
        if let Some(toml::Value::Table(t)) = passthrough.get(key)
            && t.is_empty()
        {
            passthrough.remove(key);
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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"provider_type": format!("{:?}", provider_type)})
                    ),
                "[storage.provider.config] unknown provider type ; passthrough as-is"
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

    if let Some(toml::Value::Table(resources_table)) = resources
        && !resources_table.is_empty()
    {
        let dropped: Vec<String> = resources_table
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!(
                "[security.resources] dropped during V2→V3 migration (no V3 enforcement \
             codepath existed; sandbox backends own resource budgets): {}",
                dropped.join(", ")
            )
        );
    }

    let Some(toml::Value::Table(sandbox_table)) = sandbox else {
        return;
    };
    if sandbox_table.is_empty() {
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

    for (k, v) in sandbox_table {
        let target_key = match k.as_str() {
            "enabled" => "sandbox_enabled",
            "backend" => "sandbox_backend",
            "firejail_args" => "firejail_args",
            _ => continue,
        };
        default_profile.entry(target_key.to_string()).or_insert(v);
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "[security.sandbox] folded into [risk_profiles.default]"
    );
}

fn split_autonomy_into_profile_buckets(
    value: toml::Value,
) -> (Option<toml::Table>, Option<toml::Table>) {
    let Ok(table) = value.try_into::<toml::Table>() else {
        return (None, None);
    };
    const RUNTIME_FIELDS: &[&str] = &[
        "max_actions_per_hour",
        "max_cost_per_day_cents",
        "shell_timeout_secs",
        "max_delegation_depth",
        "delegation_timeout_secs",
        "agentic_timeout_secs",
    ];
    let mut risk = toml::Table::new();
    let mut runtime = toml::Table::new();
    for (k, v) in table {
        if RUNTIME_FIELDS.contains(&k.as_str()) {
            runtime.insert(k, v);
        } else {
            risk.insert(k, v);
        }
    }
    let risk = (!risk.is_empty()).then_some(risk);
    let runtime = (!runtime.is_empty()).then_some(runtime);
    (risk, runtime)
}

/// Merge a field set into `<profile_kind>.default`, preserving values
/// that already exist on the destination (`entry().or_insert`).
fn merge_into_profile_default(profiles: &mut toml::Table, fields: toml::Table) {
    let default_entry = profiles
        .entry("default".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(default_table) = default_entry.as_table_mut() else {
        return;
    };
    for (k, v) in fields {
        default_table.entry(k).or_insert(v);
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

use anyhow::{Context as MigContext, Result as MigResult};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};

/// Destination class for a top-level entry under the legacy
/// `<install>/workspace/` directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2WorkspaceDest {
    /// Wholesale relocation into `<install>/data/<name>`.
    DataDir,
    /// Wholesale relocation into `<install>/shared/<name>`.
    SharedDir,
    /// Wholesale relocation into `<install>/agents/default/workspace/<name>`.
    AgentDefault,
    /// `workspace/memory/` has mixed contents: shared DBs / archive /
    /// snapshot stay in `data/memory/`; markdown daily files belong to
    /// the agent. The orchestrator iterates subentries and dispatches
    /// via [`V2_MEMORY_DATA_NAMES`].
    MemorySubentryDispatch,
}

pub const V2_WORKSPACE_TOPLEVEL_DISPATCH: &[(&str, V2WorkspaceDest)] = &[
    ("memory", V2WorkspaceDest::MemorySubentryDispatch),
    ("sessions", V2WorkspaceDest::DataDir),
    ("state", V2WorkspaceDest::DataDir),
    ("skills", V2WorkspaceDest::SharedDir),
    // Top-level instance-state file. The DeviceRegistry reader at
    // `api_pairing.rs:40` opens `<data_dir>/devices.db`, so unlike
    // per-agent files this has to land in `data/`, not in the agent
    // workspace where the default-branch would otherwise send it.
    ("devices.db", V2WorkspaceDest::DataDir),
];

pub const V2_MEMORY_DATA_NAMES: &[&str] = &[
    "brain.db",
    "audit.db",
    "response_cache.db",
    "MEMORY_SNAPSHOT.md",
    "archive",
];

/// V3 root directories that should exist after a successful migration.
/// The e2e test asserts every entry under `<install>` is either one of
/// these, the post-migration `config.toml(.backup)?`, or a `backup-*/`.
pub const V3_INSTALL_ROOT_NAMES: &[&str] = &["data", "shared", "agents"];

/// Dispatch a top-level legacy entry name to its V2WorkspaceDest class.
pub fn v2_workspace_toplevel_dest(name: &str) -> V2WorkspaceDest {
    V2_WORKSPACE_TOPLEVEL_DISPATCH
        .iter()
        .copied()
        .find(|(n, _)| *n == name)
        .map(|(_, d)| d)
        .unwrap_or(V2WorkspaceDest::AgentDefault)
}

pub fn workspace_toplevel_v3_path(install: &Path, name: &str) -> PathBuf {
    match v2_workspace_toplevel_dest(name) {
        V2WorkspaceDest::DataDir | V2WorkspaceDest::MemorySubentryDispatch => {
            install.join("data").join(name)
        }
        V2WorkspaceDest::SharedDir => install.join("shared").join(name),
        V2WorkspaceDest::AgentDefault => install
            .join("agents")
            .join("default")
            .join("workspace")
            .join(name),
    }
}

/// V3 destination path for a subentry under legacy `workspace/memory/`.
pub fn memory_subentry_v3_path(install: &Path, sub_name: &str) -> PathBuf {
    if V2_MEMORY_DATA_NAMES.contains(&sub_name) {
        install.join("data").join("memory").join(sub_name)
    } else {
        install
            .join("agents")
            .join("default")
            .join("workspace")
            .join("memory")
            .join(sub_name)
    }
}

/// Result of a successful filesystem migration.
#[derive(Debug, Clone)]
pub struct FilesystemMigrationReport {
    /// Timestamped backup directory (e.g. `<install>/backup-20260516T140530`).
    /// Empty when no migration ran.
    pub backup_dir: Option<PathBuf>,
    /// Number of top-level entries relocated.
    pub entries_relocated: usize,
}

pub fn migrate_v2_to_v3_install_filesystem(
    install_root: &Path,
) -> MigResult<FilesystemMigrationReport> {
    let legacy = install_root.join("workspace");
    let agent_default = install_root
        .join("agents")
        .join("default")
        .join("workspace");

    if !legacy.is_dir() {
        relocate_default_agent_skills_to_shared(install_root)?;
        return Ok(FilesystemMigrationReport {
            backup_dir: None,
            entries_relocated: 0,
        });
    }

    let data_target = install_root.join("data");
    let data_populated = data_target
        .is_dir()
        .then(|| std::fs::read_dir(&data_target).ok())
        .flatten()
        .is_some_and(|mut it| it.next().is_some());
    let agent_populated = agent_default
        .is_dir()
        .then(|| std::fs::read_dir(&agent_default).ok())
        .flatten()
        .is_some_and(|mut it| it.next().is_some());
    if data_populated && agent_populated {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "data_target": data_target.display().to_string(),
                    "agent_target": agent_default.display().to_string(),
                    "legacy": legacy.display().to_string(),
                })
            ),
            "[system] filesystem migration: targets already populated; skipping split"
        );
        relocate_default_agent_skills_to_shared(install_root)?;
        return Ok(FilesystemMigrationReport {
            backup_dir: None,
            entries_relocated: 0,
        });
    }

    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let backup_dir = install_root
        .join(format!("backup-{timestamp}"))
        .join("legacy-workspace");
    std::fs::create_dir_all(&backup_dir).with_context(|| {
        format!(
            "[system] failed to create migration backup dir at {}",
            backup_dir.display()
        )
    })?;
    copy_dir_recursive(&legacy, &backup_dir).with_context(|| {
        format!(
            "[system] failed to back up legacy workspace from {} to {}",
            legacy.display(),
            backup_dir.display()
        )
    })?;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "backup": backup_dir.display().to_string(),
            })
        ),
        "[system] filesystem migration: legacy workspace backed up"
    );

    let entries_relocated = relocate_workspace_toplevel(&legacy, install_root, &backup_dir)
        .with_context(|| {
            format!(
                "[system] failed during workspace top-level relocation under {}",
                install_root.display()
            )
        })?;

    if std::fs::read_dir(&legacy)
        .map(|mut it| it.next().is_none())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(&legacy);
    }

    relocate_default_agent_skills_to_shared(install_root)?;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "backup": backup_dir.display().to_string(),
                "entries_relocated": entries_relocated,
            })
        ),
        "[system] filesystem migration: legacy workspace split into V3 layout"
    );

    Ok(FilesystemMigrationReport {
        backup_dir: Some(backup_dir.parent().unwrap_or(&backup_dir).to_path_buf()),
        entries_relocated,
    })
}

/// Iterate `legacy/` top-level entries and relocate each via the
/// dispatch tables. Returns the count of entries successfully moved
/// (entries already at the target are counted as moved).
fn relocate_workspace_toplevel(
    legacy: &Path,
    install_root: &Path,
    backup_dir: &Path,
) -> MigResult<usize> {
    let mut count = 0usize;
    for entry in std::fs::read_dir(legacy).with_context(|| {
        format!(
            "[system] failed to enumerate legacy workspace at {}",
            legacy.display()
        )
    })? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "legacy": legacy.display().to_string(),
                    })),
                "[system] filesystem migration: skipping non-UTF-8 entry"
            );
            continue;
        };
        let src = entry.path();

        match v2_workspace_toplevel_dest(name_str) {
            V2WorkspaceDest::MemorySubentryDispatch => {
                count += relocate_memory_subentries(&src, install_root, backup_dir)?;
            }
            _ => {
                let dst = workspace_toplevel_v3_path(install_root, name_str);
                if move_with_refuse_to_clobber(&src, &dst)? {
                    count += 1;
                }
            }
        }
    }
    Ok(count)
}

/// Iterate `legacy/memory/`'s subentries and route each per
/// [`memory_subentry_v3_path`].
fn relocate_memory_subentries(
    legacy_memory_dir: &Path,
    install_root: &Path,
    _backup_dir: &Path,
) -> MigResult<usize> {
    if !legacy_memory_dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(legacy_memory_dir).with_context(|| {
        format!(
            "[system] failed to enumerate {} during memory sub-dispatch",
            legacy_memory_dir.display()
        )
    })? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "legacy_memory_dir": legacy_memory_dir.display().to_string(),
                    })),
                "[system] filesystem migration: skipping non-UTF-8 entry under memory"
            );
            continue;
        };
        let src = entry.path();
        let dst = memory_subentry_v3_path(install_root, name_str);
        if move_with_refuse_to_clobber(&src, &dst)? {
            count += 1;
        }
    }
    // Remove the now-empty memory dir (best-effort).
    if std::fs::read_dir(legacy_memory_dir)
        .map(|mut it| it.next().is_none())
        .unwrap_or(false)
    {
        let _ = std::fs::remove_dir(legacy_memory_dir);
    }
    Ok(count)
}

fn move_with_refuse_to_clobber(src: &Path, dst: &Path) -> MigResult<bool> {
    if dst.exists() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "source": src.display().to_string(),
                    "target": dst.display().to_string(),
                })),
            "[system] filesystem migration: target already exists; refusing to clobber"
        );
        return Ok(false);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("[system] failed to create parent dir {}", parent.display())
        })?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(true);
    }
    // Cross-filesystem fallback.
    if src.is_dir() {
        copy_dir_recursive(src, dst).with_context(|| {
            format!(
                "[system] failed to copy {} to {}",
                src.display(),
                dst.display()
            )
        })?;
        std::fs::remove_dir_all(src)
            .with_context(|| format!("[system] failed to remove {} after copy", src.display()))?;
    } else {
        std::fs::copy(src, dst).with_context(|| {
            format!(
                "[system] failed to copy {} to {}",
                src.display(),
                dst.display()
            )
        })?;
        std::fs::remove_file(src)
            .with_context(|| format!("[system] failed to remove {} after copy", src.display()))?;
    }
    Ok(true)
}

/// Heal intermediate installs that landed skills under
/// `agents/default/workspace/skills/` before the host-wide
/// `shared/skills/` layout was introduced. Idempotent.
pub fn relocate_default_agent_skills_to_shared(install_root: &Path) -> MigResult<bool> {
    let src = install_root
        .join("agents")
        .join("default")
        .join("workspace")
        .join("skills");
    let dst = install_root.join("shared").join("skills");
    if !src.is_dir() {
        return Ok(false);
    }
    let dst_populated = dst
        .is_dir()
        .then(|| std::fs::read_dir(&dst).ok())
        .flatten()
        .is_some_and(|mut it| it.next().is_some());
    if dst_populated {
        return Ok(false);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "[system] failed to create shared workspace parent {}",
                parent.display()
            )
        })?;
    }
    if std::fs::rename(&src, &dst).is_err() {
        copy_dir_recursive(&src, &dst).with_context(|| {
            format!(
                "[system] failed to copy {} to {}",
                src.display(),
                dst.display()
            )
        })?;
        std::fs::remove_dir_all(&src)
            .with_context(|| format!("[system] failed to remove {} after copy", src.display()))?;
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "from": src.display().to_string(),
                "to": dst.display().to_string(),
            })
        ),
        "[system] filesystem migration: lifted default-agent skills into shared/"
    );
    Ok(true)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> MigResult<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            {
                let target = std::fs::read_link(&from)?;
                std::os::unix::fs::symlink(&target, &to)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(&from, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// SQLite agent_id backfill.
// -----------------------------------------------------------------------------

/// On-disk schema version stamped after a successful SQLite memory
/// migration. Future migrations consult this rather than re-running
/// PRAGMA detection.
pub const SQLITE_MEMORY_SCHEMA_VERSION: i64 = 1;

#[cfg(feature = "memory-postgres")]
fn postgres_memory_schema_version() -> MigResult<i32> {
    i32::try_from(SQLITE_MEMORY_SCHEMA_VERSION)
        .context("Postgres memory schema version exceeds INTEGER range")
}

pub fn migrate_sqlite_memory_to_v3(db_path: &Path, conn: &Connection) -> MigResult<()> {
    if sqlite_memories_agent_id_is_not_null(conn)? && sqlite_memories_has_unique_agent_key(conn)? {
        return Ok(());
    }

    if sqlite_memories_row_count(conn)? > 0 && db_path.exists() {
        backup_sqlite_for_multi_agent_migration(db_path)?;
    }

    conn.execute_batch("BEGIN IMMEDIATE; PRAGMA defer_foreign_keys = ON;")?;
    let result = (|| -> MigResult<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agents (
                id          TEXT PRIMARY KEY,
                alias       TEXT NOT NULL UNIQUE,
                created_at  TEXT NOT NULL
             );",
        )?;
        let default_uuid = sqlite_ensure_default_agent_uuid(conn)?;

        if !sqlite_memories_has_agent_id_column(conn)? {
            conn.execute_batch("ALTER TABLE memories ADD COLUMN agent_id TEXT;")?;
        }
        conn.execute(
            "UPDATE memories SET agent_id = ?1 WHERE agent_id IS NULL",
            params![default_uuid],
        )?;

        conn.execute_batch(
            "DROP TRIGGER IF EXISTS memories_ai;
             DROP TRIGGER IF EXISTS memories_ad;
             DROP TRIGGER IF EXISTS memories_au;
             DROP TABLE IF EXISTS memories_fts;

             CREATE TABLE memories_new (
                id            TEXT PRIMARY KEY,
                key           TEXT NOT NULL,
                content       TEXT NOT NULL,
                category      TEXT NOT NULL DEFAULT 'core',
                embedding     BLOB,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                session_id    TEXT,
                namespace     TEXT DEFAULT 'default',
                importance    REAL DEFAULT 0.5,
                superseded_by TEXT,
                agent_id      TEXT NOT NULL REFERENCES agents(id),
                UNIQUE (agent_id, key)
             );

             INSERT INTO memories_new (
                id, key, content, category, embedding, created_at, updated_at,
                session_id, namespace, importance, superseded_by, agent_id
             )
             SELECT
                id, key, content, category, embedding, created_at, updated_at,
                session_id, namespace, importance, superseded_by, agent_id
             FROM memories;

             DROP TABLE memories;
             ALTER TABLE memories_new RENAME TO memories;

             CREATE INDEX IF NOT EXISTS idx_memories_category  ON memories(category);
             CREATE INDEX IF NOT EXISTS idx_memories_key       ON memories(key);
             CREATE INDEX IF NOT EXISTS idx_memories_session   ON memories(session_id);
             CREATE INDEX IF NOT EXISTS idx_memories_namespace ON memories(namespace);
             CREATE INDEX IF NOT EXISTS idx_memories_agent_id  ON memories(agent_id);

             CREATE VIRTUAL TABLE memories_fts USING fts5(
                key, content, content=memories, content_rowid=rowid
             );
             INSERT INTO memories_fts(memories_fts) VALUES('rebuild');

             CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
             END;
             CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
             END;
             CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
             END;",
        )?;

        sqlite_ensure_schema_version_table(conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO schema_version (component, version, applied_at) \
             VALUES ('memories', ?1, ?2)",
            params![
                SQLITE_MEMORY_SCHEMA_VERSION,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

fn sqlite_ensure_schema_version_table(conn: &Connection) -> MigResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            component  TEXT PRIMARY KEY,
            version    INTEGER NOT NULL,
            applied_at TEXT NOT NULL
         );",
    )?;
    Ok(())
}

fn sqlite_memories_agent_id_is_not_null(conn: &Connection) -> MigResult<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
    let agent_id_notnull: Option<bool> = stmt
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let notnull: i64 = row.get(3)?;
            Ok((name, notnull != 0))
        })?
        .filter_map(Result::ok)
        .find(|(name, _)| name == "agent_id")
        .map(|(_, notnull)| notnull);

    let Some(true) = agent_id_notnull else {
        return Ok(false);
    };

    let mut fk_stmt = conn.prepare("PRAGMA foreign_key_list(memories)")?;
    let has_fk = fk_stmt
        .query_map([], |row| {
            let target_table: String = row.get(2)?;
            let from_col: String = row.get(3)?;
            Ok((target_table, from_col))
        })?
        .filter_map(Result::ok)
        .any(|(target, from)| target == "agents" && from == "agent_id");
    Ok(has_fk)
}

fn sqlite_memories_has_agent_id_column(conn: &Connection) -> MigResult<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
    Ok(stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == "agent_id"))
}

fn sqlite_memories_has_unique_agent_key(conn: &Connection) -> MigResult<bool> {
    // `PRAGMA index_list` returns one row per index; `PRAGMA index_info`
    // returns one row per column in that index.  We want an index that is
    // UNIQUE and whose column set is exactly {"agent_id", "key"}.
    let mut idx_stmt = conn.prepare("PRAGMA index_list(memories)")?;
    let index_names: Vec<(String, bool)> = idx_stmt
        .query_map([], |row| {
            let name: String = row.get(1)?;
            let unique: i64 = row.get(2)?;
            Ok((name, unique != 0))
        })?
        .filter_map(Result::ok)
        .collect();

    for (idx_name, is_unique) in index_names {
        if !is_unique {
            continue;
        }
        // PRAGMA index_info does not support parameter binding; format inline.
        // Index names come from sqlite_master and are controlled by SQLite
        // itself or our own migrations, so this is safe.
        let pragma = format!("PRAGMA index_info(\"{}\")", idx_name.replace('"', "\"\""));
        let mut info_stmt = conn.prepare(&pragma)?;
        let cols: Vec<String> = info_stmt
            .query_map([], |row| row.get::<_, String>(2))?
            .filter_map(Result::ok)
            .collect();
        if cols.len() == 2
            && cols.contains(&"agent_id".to_string())
            && cols.contains(&"key".to_string())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sqlite_memories_row_count(conn: &Connection) -> MigResult<i64> {
    let table_exists: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !table_exists {
        return Ok(0);
    }
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
    Ok(count)
}

/// Mint or query the `default` agent's row. Idempotent on concurrent
/// first-init: the returned UUID is the row that actually persisted,
/// not the candidate we attempted to insert.
pub fn sqlite_ensure_default_agent_uuid(conn: &Connection) -> MigResult<String> {
    sqlite_ensure_agent_uuid(conn, "default")
}

/// Mint-or-query a single agent row keyed by alias. Used by the
/// SQLite migration's default-agent backfill and by the `ensure_agent_uuid`
/// trait impl on the memory backend (alias resolution at agent-loop entry).
pub fn sqlite_ensure_agent_uuid(conn: &Connection, alias: &str) -> MigResult<String> {
    let new_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO agents (id, alias, created_at) VALUES (?1, ?2, ?3)",
        params![new_id, alias, now],
    )?;
    let final_id: String = conn.query_row(
        "SELECT id FROM agents WHERE alias = ?1 LIMIT 1",
        params![alias],
        |row| row.get(0),
    )?;
    Ok(final_id)
}

fn backup_sqlite_for_multi_agent_migration(db_path: &Path) -> MigResult<()> {
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let backup_path = db_path.with_file_name(format!(
        "{}.backup-{timestamp}",
        db_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "brain.db".to_string()),
    ));
    std::fs::copy(db_path, &backup_path).with_context(|| {
        format!(
            "failed to copy {} to {} before multi-agent migration",
            db_path.display(),
            backup_path.display(),
        )
    })?;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "backup": backup_path.display().to_string(),
            })
        ),
        "multi-agent migration: backed up SQLite memory DB before adding agents table"
    );
    Ok(())
}

// -----------------------------------------------------------------------------
// Postgres agent_id backfill.
// -----------------------------------------------------------------------------

#[cfg(feature = "memory-postgres")]
pub fn migrate_postgres_memory_to_v3(
    client: &mut postgres::Client,
    schema_ident: &str,
    qualified_table: &str,
) -> MigResult<()> {
    let qualified_agents = format!("{schema_ident}.agents");

    client.batch_execute(&format!(
        "CREATE TABLE IF NOT EXISTS {qualified_agents} (
            id          TEXT PRIMARY KEY,
            alias       TEXT NOT NULL UNIQUE,
            created_at  TIMESTAMPTZ NOT NULL
        );"
    ))?;

    let candidate_uuid = uuid::Uuid::new_v4().to_string();
    client.execute(
        &format!(
            "INSERT INTO {qualified_agents} (id, alias, created_at)
             VALUES ($1, 'default', NOW())
             ON CONFLICT (alias) DO NOTHING"
        ),
        &[&candidate_uuid],
    )?;
    let default_uuid: String = client
        .query_one(
            &format!("SELECT id FROM {qualified_agents} WHERE alias = 'default' LIMIT 1"),
            &[],
        )?
        .get(0);

    client.batch_execute(&format!(
        "ALTER TABLE {qualified_table} ADD COLUMN IF NOT EXISTS agent_id TEXT;
         CREATE INDEX IF NOT EXISTS idx_memories_agent_id ON {qualified_table}(agent_id);"
    ))?;
    client.execute(
        &format!("UPDATE {qualified_table} SET agent_id = $1 WHERE agent_id IS NULL"),
        &[&default_uuid],
    )?;

    client.batch_execute(&format!(
        "
        DO $$
        BEGIN
            IF NOT EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conname = 'memories_agent_id_notnull_chk'
            ) THEN
                ALTER TABLE {qualified_table}
                    ADD CONSTRAINT memories_agent_id_notnull_chk
                    CHECK (agent_id IS NOT NULL) NOT VALID;
            END IF;
        END$$;
        ALTER TABLE {qualified_table} VALIDATE CONSTRAINT memories_agent_id_notnull_chk;
        ALTER TABLE {qualified_table} ALTER COLUMN agent_id SET NOT NULL;
        DO $$
        BEGIN
            IF NOT EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conname = 'memories_agent_id_fk'
            ) THEN
                ALTER TABLE {qualified_table}
                    ADD CONSTRAINT memories_agent_id_fk
                    FOREIGN KEY (agent_id) REFERENCES {qualified_agents}(id) NOT VALID;
            END IF;
        END$$;
        ALTER TABLE {qualified_table} VALIDATE CONSTRAINT memories_agent_id_fk;
        -- Swap the legacy key-only uniqueness for composite (agent_id, key)
        -- so two agents may hold rows under the same caller-chosen key.
        ALTER TABLE {qualified_table} DROP CONSTRAINT IF EXISTS memories_key_key;
        DO $$
        BEGIN
            IF NOT EXISTS (
                SELECT 1 FROM pg_constraint
                WHERE conname = 'memories_agent_key_uniq'
            ) THEN
                ALTER TABLE {qualified_table}
                    ADD CONSTRAINT memories_agent_key_uniq UNIQUE (agent_id, key);
            END IF;
        END$$;
        "
    ))?;

    client.batch_execute(&format!(
        "CREATE TABLE IF NOT EXISTS {schema_ident}.schema_version (
            component  TEXT PRIMARY KEY,
            version    INTEGER NOT NULL,
            applied_at TIMESTAMPTZ NOT NULL
        );"
    ))?;
    let memory_schema_version = postgres_memory_schema_version()?;
    client.execute(
        &format!(
            "INSERT INTO {schema_ident}.schema_version (component, version, applied_at) \
             VALUES ('memories', $1, NOW()) \
             ON CONFLICT (component) DO UPDATE SET version = EXCLUDED.version, applied_at = EXCLUDED.applied_at"
        ),
        &[&memory_schema_version],
    )?;
    Ok(())
}

pub const QDRANT_DEFAULT_AGENT_ID: &str = "default";

pub async fn migrate_qdrant_collection_to_v3(
    client: &reqwest::Client,
    base_url: &str,
    collection: &str,
    api_key: Option<&str>,
) -> MigResult<usize> {
    let base_url = base_url.trim_end_matches('/');
    let mut next_offset: Option<serde_json::Value> = None;
    let mut updated = 0usize;

    loop {
        let mut scroll_body = serde_json::json!({
            "limit": 1000,
            "with_payload": true,
            "with_vector": false,
            // Match only points that lack agent_id. is_empty supports
            // the missing-key case (the filter matches a point whose
            // payload key is absent or whose stored value is null).
            "filter": {
                "must": [{ "is_empty": { "key": "agent_id" } }]
            }
        });
        if let Some(ref offset) = next_offset {
            scroll_body["offset"] = offset.clone();
        }

        let url = format!("{base_url}/collections/{collection}/points/scroll");
        let mut req = client.request(reqwest::Method::POST, &url);
        if let Some(key) = api_key {
            req = req.header("api-key", key);
        }
        let resp = req
            .header("Content-Type", "application/json")
            .json(&scroll_body)
            .send()
            .await
            .context("[system] Qdrant V3 migration: scroll request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant scroll failed ({status}): {text}");
        }

        #[derive(serde::Deserialize)]
        struct ScrollPage {
            result: ScrollResult,
        }
        #[derive(serde::Deserialize)]
        struct ScrollResult {
            points: Vec<ScrollPoint>,
            #[serde(default)]
            next_page_offset: Option<serde_json::Value>,
        }
        #[derive(serde::Deserialize)]
        struct ScrollPoint {
            id: serde_json::Value,
        }

        let page: ScrollPage = resp
            .json()
            .await
            .context("[system] Qdrant V3 migration: scroll page parse failed")?;
        let ids: Vec<serde_json::Value> = page.result.points.into_iter().map(|p| p.id).collect();
        if !ids.is_empty() {
            let set_url = format!("{base_url}/collections/{collection}/points/payload");
            let body = serde_json::json!({
                "payload": { "agent_id": QDRANT_DEFAULT_AGENT_ID },
                "points": ids,
            });
            let mut req = client.request(reqwest::Method::POST, &set_url);
            if let Some(key) = api_key {
                req = req.header("api-key", key);
            }
            let resp = req
                .header("Content-Type", "application/json")
                .query(&[("wait", "true")])
                .json(&body)
                .send()
                .await
                .context("[system] Qdrant V3 migration: set payload request failed")?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Qdrant set payload failed ({status}): {text}");
            }
            let batch_count = body["points"].as_array().map(|a| a.len()).unwrap_or(0);
            updated += batch_count;
        }

        match page.result.next_page_offset {
            Some(offset) if !offset.is_null() => next_offset = Some(offset),
            _ => break,
        }
    }

    if updated > 0 {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "collection": collection,
                    "updated": updated,
                })
            ),
            "[system] Qdrant V3 migration: backfilled agent_id payload"
        );
    }
    Ok(updated)
}

#[cfg(test)]
mod fs_db_migration_tests {
    use super::*;
    use rusqlite::Connection;
    use std::collections::BTreeSet;
    use std::fs;
    use tempfile::TempDir;

    /// Walk a directory tree and return a sorted list of (relative
    /// path, file contents) pairs. Used to diff pre-migration backup
    /// against the legacy snapshot for byte-equal verification.
    fn snapshot_tree(root: &Path) -> BTreeSet<(PathBuf, Vec<u8>)> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<(PathBuf, Vec<u8>)>) {
            let Ok(rd) = fs::read_dir(dir) else { return };
            for entry in rd.flatten() {
                let path = entry.path();
                if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                    walk(root, &path, out);
                } else if let Ok(bytes) = fs::read(&path)
                    && let Ok(rel) = path.strip_prefix(root)
                {
                    out.insert((rel.to_path_buf(), bytes));
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(root, root, &mut out);
        out
    }

    /// Lay down a V2 install rooted at `install`: a `workspace/` tree
    /// hitting every dispatch branch, plus a populated `brain.db` in
    /// pre-multi-agent shape so the SQLite migration has rows to
    /// backfill.
    fn seed_v2_install(install: &Path) {
        // Top-level instance file (devices.db bug fix lands this in
        // data/ post-migration).
        fs::create_dir_all(install.join("workspace")).unwrap();
        fs::write(
            install.join("workspace/devices.db"),
            b"pretend-paired-devices-blob",
        )
        .unwrap();

        // Per-agent identity files (default branch → agent workspace).
        for fname in [
            "MEMORY.md",
            "IDENTITY.md",
            "SOUL.md",
            "USER.md",
            "AGENTS.md",
        ] {
            fs::write(install.join("workspace").join(fname), format!("# {fname}")).unwrap();
        }

        // workspace/sessions/ (wholesale → data/sessions/).
        fs::create_dir_all(install.join("workspace/sessions")).unwrap();
        fs::write(install.join("workspace/sessions/sessions.db"), b"sessions").unwrap();

        // workspace/state/ (wholesale → data/state/).
        fs::create_dir_all(install.join("workspace/state")).unwrap();
        fs::write(
            install.join("workspace/state/runtime-trace.jsonl"),
            b"trace",
        )
        .unwrap();

        // workspace/skills/ (wholesale → shared/skills/).
        fs::create_dir_all(install.join("workspace/skills/my-skill")).unwrap();
        fs::write(install.join("workspace/skills/my-skill/SKILL.md"), b"skill").unwrap();

        // workspace/memory/ subentries: split between data/memory/ and
        // agents/default/workspace/memory/ per V2_MEMORY_DATA_NAMES.
        let mem_dir = install.join("workspace/memory");
        fs::create_dir_all(&mem_dir).unwrap();
        for sub in V2_MEMORY_DATA_NAMES {
            let p = mem_dir.join(sub);
            if *sub == "archive" {
                fs::create_dir_all(&p).unwrap();
                fs::write(p.join("old-recall.jsonl"), b"archived").unwrap();
            } else if (*sub).ends_with(".db") {
                // Real SQLite file so the in-DB migration has something
                // to migrate after the FS move.
                let conn = Connection::open(&p).unwrap();
                if *sub == "brain.db" {
                    conn.execute_batch(
                        "PRAGMA foreign_keys = ON;
                         CREATE TABLE memories (
                            id TEXT PRIMARY KEY,
                            key TEXT NOT NULL UNIQUE,
                            content TEXT NOT NULL,
                            category TEXT NOT NULL DEFAULT 'core',
                            embedding BLOB,
                            created_at TEXT NOT NULL,
                            updated_at TEXT NOT NULL,
                            session_id TEXT,
                            namespace TEXT DEFAULT 'default',
                            importance REAL DEFAULT 0.5,
                            superseded_by TEXT
                         );
                         INSERT INTO memories (id, key, content, created_at, updated_at)
                         VALUES ('m1', 'hello', 'world', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                                ('m2', 'foo',   'bar',   '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
                    )
                    .unwrap();
                }
            } else {
                fs::write(&p, format!("{sub} payload").as_bytes()).unwrap();
            }
        }
        // Markdown daily files (must land in agents/default/workspace/memory/).
        fs::write(
            mem_dir.join("2025-04-12.md"),
            b"# daily 2025-04-12\nhello world\n",
        )
        .unwrap();
        fs::write(
            mem_dir.join("2025-04-13.md"),
            b"# daily 2025-04-13\nstill here\n",
        )
        .unwrap();
    }

    #[test]
    fn migrate_v2_install_into_v3_layout_with_real_filesystem() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path();
        seed_v2_install(install);

        let legacy_snapshot = snapshot_tree(&install.join("workspace"));
        assert!(
            !legacy_snapshot.is_empty(),
            "fixture seed must produce content under workspace/"
        );

        let report = migrate_v2_to_v3_install_filesystem(install).expect("migration must succeed");
        assert!(report.entries_relocated > 0);
        let backup_root = report.backup_dir.expect("backup dir present");

        // Backup is byte-equal to the pre-migration workspace snapshot.
        let backup_snapshot = snapshot_tree(&backup_root.join("legacy-workspace"));
        assert_eq!(
            backup_snapshot, legacy_snapshot,
            "backup must be a byte-equal copy of the pre-migration workspace"
        );

        // Every legacy file lives at exactly one V3 location, predicted
        // by the shared dispatch fns. We never name V3 paths here —
        // the same fns the migration uses produce them.
        for (rel, expected_bytes) in &legacy_snapshot {
            let v3_path = predict_v3_path(install, rel);
            assert!(
                v3_path.exists(),
                "file {} should exist at predicted V3 path {}",
                rel.display(),
                v3_path.display(),
            );
            let actual_bytes = fs::read(&v3_path).unwrap_or_else(|e| {
                panic!(
                    "failed to read predicted V3 path {} for legacy {}: {e}",
                    v3_path.display(),
                    rel.display(),
                )
            });
            assert_eq!(
                actual_bytes,
                *expected_bytes,
                "byte mismatch at {}",
                v3_path.display()
            );
        }

        // Legacy workspace/ is gone (it was empty after the relocation).
        assert!(
            !install.join("workspace").exists(),
            "legacy workspace must be removed after a clean split"
        );

        // Nothing outside the V3 root names + backup + (no config.toml in
        // this test) lives at install root.
        let mut roots = BTreeSet::new();
        for entry in fs::read_dir(install).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            roots.insert(name);
        }
        for name in &roots {
            let allowed =
                V3_INSTALL_ROOT_NAMES.contains(&name.as_str()) || name.starts_with("backup-");
            assert!(
                allowed,
                "unexpected install-root entry {name:?}; allowed: {V3_INSTALL_ROOT_NAMES:?} + backup-*"
            );
        }

        // Idempotent re-run is a no-op: same on-disk state afterward.
        let post_first = snapshot_tree(install);
        let report2 =
            migrate_v2_to_v3_install_filesystem(install).expect("second run must be a no-op");
        assert_eq!(report2.entries_relocated, 0);
        let post_second = snapshot_tree(install);
        assert_eq!(
            post_first, post_second,
            "idempotent re-run must not modify disk"
        );

        // In-DB migration: open the now-moved brain.db and run
        // migrate_sqlite_memory_to_v3. Should backfill agent_id on the
        // 2 seeded rows and stamp schema_version.
        let brain_path = install.join("data/memory/brain.db");
        assert!(
            brain_path.is_file(),
            "brain.db must have moved to data/memory/"
        );
        let conn = Connection::open(&brain_path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate_sqlite_memory_to_v3(&brain_path, &conn).expect("SQLite migration must succeed");

        let null_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE agent_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            null_count, 0,
            "all memories must have agent_id post-migration"
        );

        let agent_row_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agents WHERE alias = 'default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(agent_row_count, 1, "default agent row must exist");

        // SQLite migration is idempotent.
        migrate_sqlite_memory_to_v3(&brain_path, &conn)
            .expect("SQLite migration second run must be a no-op");

        // SQLite backup file from the in-DB migration is present.
        let backup_glob: Vec<_> = fs::read_dir(install.join("data/memory"))
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("brain.db.backup-")
            })
            .collect();
        assert_eq!(
            backup_glob.len(),
            1,
            "in-DB SQLite migration must write exactly one backup file"
        );
    }

    #[test]
    fn migrate_sqlite_memory_to_v3_adds_unique_constraint_when_missing() {
        use rusqlite::Connection;
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("brain.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

        // Manually build the "partially-migrated" shape: agent_id NOT NULL +
        // FK, but NO UNIQUE (agent_id, key) constraint.
        conn.execute_batch(
            "CREATE TABLE agents (
                id         TEXT PRIMARY KEY,
                alias      TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL
             );
             INSERT INTO agents VALUES ('uuid-1','default','2025-01-01T00:00:00Z');

             CREATE TABLE memories (
                id         TEXT PRIMARY KEY,
                key        TEXT NOT NULL,
                content    TEXT NOT NULL,
                category   TEXT NOT NULL DEFAULT 'core',
                embedding  BLOB,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                session_id TEXT,
                namespace  TEXT DEFAULT 'default',
                importance REAL DEFAULT 0.5,
                superseded_by TEXT,
                agent_id   TEXT NOT NULL REFERENCES agents(id)
                -- intentionally NO UNIQUE (agent_id, key)
             );
             INSERT INTO memories VALUES (
                'mid-1','test-key','test-content','core',NULL,
                '2025-01-01T00:00:00Z','2025-01-01T00:00:00Z',
                NULL,'default',0.5,NULL,'uuid-1'
             );",
        )
        .unwrap();

        // Migration must detect the missing unique constraint and re-run.
        migrate_sqlite_memory_to_v3(&db_path, &conn)
            .expect("migration must succeed on partially-migrated DB");

        // Idempotent second call must also succeed.
        migrate_sqlite_memory_to_v3(&db_path, &conn).expect("second migration run must be a no-op");

        // The unique index must now exist.
        let has_unique = sqlite_memories_has_unique_agent_key(&conn).unwrap();
        assert!(
            has_unique,
            "UNIQUE (agent_id, key) must be present after migration"
        );

        // Existing row must have survived.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE key='test-key'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "existing memory row must survive the migration");
    }

    #[cfg(feature = "memory-postgres")]
    #[test]
    fn memory_schema_version_binds_to_postgres_int4() {
        use postgres::types::{ToSql, Type};

        fn accepts_int4<T: ToSql>(_: &T) -> bool {
            T::accepts(&Type::INT4)
        }

        let version = postgres_memory_schema_version().expect("version fits Postgres INTEGER");
        assert_eq!(i64::from(version), SQLITE_MEMORY_SCHEMA_VERSION);
        assert!(accepts_int4(&version));
    }

    /// Predict the V3 absolute path for a legacy path relative to
    /// `<install>/workspace/`. Uses the same dispatch fns the migration
    /// uses; never names a V3 path literally.
    fn predict_v3_path(install: &Path, rel: &Path) -> PathBuf {
        let mut parts = rel.components();
        let top = parts
            .next()
            .expect("legacy snapshot paths have at least one component");
        let top_name = top.as_os_str().to_string_lossy().to_string();

        // Sub-dispatch for memory/<x>: first segment is the subentry name.
        if v2_workspace_toplevel_dest(&top_name) == V2WorkspaceDest::MemorySubentryDispatch {
            let sub = parts.next();
            let Some(sub) = sub else {
                return workspace_toplevel_v3_path(install, &top_name);
            };
            let sub_name = sub.as_os_str().to_string_lossy().to_string();
            let base = memory_subentry_v3_path(install, &sub_name);
            let rest: PathBuf = parts.as_path().to_path_buf();
            if rest.as_os_str().is_empty() {
                base
            } else {
                base.join(rest)
            }
        } else {
            let top_v3 = workspace_toplevel_v3_path(install, &top_name);
            let rest: PathBuf = parts.as_path().to_path_buf();
            if rest.as_os_str().is_empty() {
                top_v3
            } else {
                top_v3.join(rest)
            }
        }
    }

    #[test]
    fn fresh_install_is_noop() {
        let tmp = TempDir::new().unwrap();
        let report =
            migrate_v2_to_v3_install_filesystem(tmp.path()).expect("fresh install must be a no-op");
        assert_eq!(report.entries_relocated, 0);
        assert!(report.backup_dir.is_none());
    }

    #[test]
    fn refuse_to_clobber_existing_v3_target() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path();
        seed_v2_install(install);

        // Pre-seed an operator-authored file at the V3 destination for
        // devices.db. Migration must NOT overwrite it.
        fs::create_dir_all(install.join("data")).unwrap();
        let v3_devices = workspace_toplevel_v3_path(install, "devices.db");
        fs::write(&v3_devices, b"operator-owned").unwrap();

        let _ = migrate_v2_to_v3_install_filesystem(install).expect("migration must not fail");

        // Operator file untouched.
        let after = fs::read(&v3_devices).unwrap();
        assert_eq!(
            after, b"operator-owned",
            "refuse-to-clobber: operator file must survive"
        );

        // Legacy devices.db is still in place (left for operator inspection)
        // OR moved to backup; in either case the file is not lost.
        let legacy_still = install.join("workspace/devices.db").exists();
        let in_backup = fs::read_dir(install).unwrap().flatten().any(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("backup-") && e.path().join("legacy-workspace/devices.db").exists()
        });
        assert!(
            legacy_still || in_backup,
            "legacy devices.db must be preserved (in legacy/ or backup/)"
        );
    }
}
