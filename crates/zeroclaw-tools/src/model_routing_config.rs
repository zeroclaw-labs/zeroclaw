use crate::util_helpers::MaybeSet;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{ClassificationRule, Config, ModelRouteConfig};

const DEFAULT_AGENT_MAX_DEPTH: u32 = 3;
const DEFAULT_AGENT_MAX_ITERATIONS: usize = 10;

pub struct ModelRoutingConfigTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl ModelRoutingConfigTool {
    pub fn new(config: Arc<Config>, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
    }

    fn load_config_without_env(&self) -> anyhow::Result<Config> {
        let contents = fs::read_to_string(&self.config.config_path).map_err(|error| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": self.config.config_path.display().to_string(),
                        "error": format!("{}", error),
                    })),
                "model_routing_config: failed to read config file"
            );
            anyhow::Error::msg(format!(
                "Failed to read config file {}: {error}",
                self.config.config_path.display()
            ))
        })?;

        let mut parsed =
            zeroclaw_config::migration::migrate_to_current(&contents).map_err(|error| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "path": self.config.config_path.display().to_string(),
                            "error": format!("{}", error),
                        })),
                    "model_routing_config: failed to parse config file"
                );
                anyhow::Error::msg(format!(
                    "Failed to parse config file {}: {error}",
                    self.config.config_path.display()
                ))
            })?;
        parsed.config_path = self.config.config_path.clone();
        parsed.data_dir = self.config.data_dir.clone();
        Ok(parsed)
    }

    fn require_write_access(&self) -> Option<ToolResult> {
        if !self.security.can_act() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        None
    }

    fn parse_string_list(raw: &Value, field: &str) -> anyhow::Result<Vec<String>> {
        if let Some(raw_string) = raw.as_str() {
            return Ok(raw_string
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect());
        }

        if let Some(array) = raw.as_array() {
            let mut out = Vec::new();
            for item in array {
                let value = item.as_str().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"field": field})),
                        "model_routing_config: array element must be a string"
                    );
                    anyhow::Error::msg(format!("'{field}' array must only contain strings"))
                })?;
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
            }
            return Ok(out);
        }

        anyhow::bail!("'{field}' must be a string or string[]")
    }

    fn parse_non_empty_string(args: &Value, field: &str) -> anyhow::Result<String> {
        let value = args
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": field})),
                    "model_routing_config: missing required string param"
                );
                anyhow::Error::msg(format!("Missing '{field}'"))
            })?
            .trim();

        if value.is_empty() {
            anyhow::bail!("'{field}' must not be empty");
        }

        Ok(value.to_string())
    }

    fn parse_optional_string_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<String>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let value = raw
            .as_str()
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"field": field})),
                    "model_routing_config: field must be string or null"
                );
                anyhow::Error::msg(format!("'{field}' must be a string or null"))
            })?
            .trim()
            .to_string();

        let output = if value.is_empty() {
            MaybeSet::Null
        } else {
            MaybeSet::Set(value)
        };
        Ok(output)
    }

    fn parse_optional_f64_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<f64>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let value = raw.as_f64().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field})),
                "model_routing_config: field must be number or null"
            );
            anyhow::Error::msg(format!("'{field}' must be a number or null"))
        })?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_usize_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<usize>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let raw_value = raw.as_u64().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field})),
                "model_routing_config: usize field must be non-negative integer or null"
            );
            anyhow::Error::msg(format!("'{field}' must be a non-negative integer or null"))
        })?;
        let value = usize::try_from(raw_value).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field, "raw_value": raw_value})),
                "model_routing_config: usize value too large"
            );
            anyhow::Error::msg(format!("'{field}' is too large for this platform"))
        })?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_u32_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<u32>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let raw_value = raw.as_u64().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field})),
                "model_routing_config: u32 field must be non-negative integer or null"
            );
            anyhow::Error::msg(format!("'{field}' must be a non-negative integer or null"))
        })?;
        let value = u32::try_from(raw_value).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field, "raw_value": raw_value})),
                "model_routing_config: u32 value too large"
            );
            anyhow::Error::msg(format!("'{field}' must fit in u32"))
        })?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_i32_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<i32>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let raw_value = raw.as_i64().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field})),
                "model_routing_config: i32 field must be integer or null"
            );
            anyhow::Error::msg(format!("'{field}' must be an integer or null"))
        })?;
        let value = i32::try_from(raw_value).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field, "raw_value": raw_value})),
                "model_routing_config: i32 value out of range"
            );
            anyhow::Error::msg(format!("'{field}' must fit in i32"))
        })?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_bool(args: &Value, field: &str) -> anyhow::Result<Option<bool>> {
        let Some(raw) = args.get(field) else {
            return Ok(None);
        };

        let value = raw.as_bool().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": field})),
                "model_routing_config: field must be boolean"
            );
            anyhow::Error::msg(format!("'{field}' must be a boolean"))
        })?;
        Ok(Some(value))
    }

    fn scenario_row(route: &ModelRouteConfig, rule: Option<&ClassificationRule>) -> Value {
        let classification = rule.map(|r| {
            json!({
                "keywords": r.keywords,
                "patterns": r.patterns,
                "min_length": r.min_length,
                "max_length": r.max_length,
                "priority": r.priority,
            })
        });

        json!({
            "hint": route.hint,
            "model_provider": route.model_provider,
            "model": route.model,
            "api_key_configured": route
                .api_key
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty()),
            "classification": classification,
        })
    }

    fn snapshot(cfg: &Config) -> Value {
        let mut routes = cfg.model_routes.clone();
        routes.sort_by(|a, b| a.hint.cmp(&b.hint));

        let mut rules = cfg.query_classification.rules.clone();
        rules.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.hint.cmp(&b.hint))
        });

        let mut scenarios = Vec::with_capacity(routes.len());
        for route in &routes {
            let rule = rules.iter().find(|r| r.hint == route.hint);
            scenarios.push(Self::scenario_row(route, rule));
        }

        let classification_only_rules: Vec<Value> = rules
            .iter()
            .filter(|rule| !routes.iter().any(|route| route.hint == rule.hint))
            .map(|rule| {
                json!({
                    "hint": rule.hint,
                    "keywords": rule.keywords,
                    "patterns": rule.patterns,
                    "min_length": rule.min_length,
                    "max_length": rule.max_length,
                    "priority": rule.priority,
                })
            })
            .collect();

        let mut agents: BTreeMap<String, Value> = BTreeMap::new();
        for (name, agent) in &cfg.agents {
            let risk = cfg.risk_profiles.get(agent.risk_profile.as_str());
            let runtime = cfg.runtime_profiles.get(agent.runtime_profile.as_str());
            agents.insert(
                name.clone(),
                json!({
                    "model_provider": agent.model_provider,
                    "risk_profile": agent.risk_profile,
                    "runtime_profile": agent.runtime_profile,
                    "max_delegation_depth": runtime.map(|r| r.max_delegation_depth),
                    "agentic": runtime.map(|r| r.agentic),
                    "allowed_tools": risk.map(|r| &r.allowed_tools),
                    "max_tool_iterations": runtime.map(|r| r.max_tool_iterations),
                }),
            );
        }

        json!({
            "query_classification": {
                "enabled": cfg.query_classification.enabled,
                "rules_count": cfg.query_classification.rules.len(),
            },
            "scenarios": scenarios,
            "classification_only_rules": classification_only_rules,
            "agents": agents,
        })
    }

    fn normalize_and_sort_routes(routes: &mut Vec<ModelRouteConfig>) {
        routes.retain(|route| !route.hint.trim().is_empty());
        routes.sort_by(|a, b| a.hint.cmp(&b.hint));
    }

    fn normalize_and_sort_rules(rules: &mut Vec<ClassificationRule>) {
        rules.retain(|rule| !rule.hint.trim().is_empty());
        rules.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.hint.cmp(&b.hint))
        });
    }

    fn has_rule_matcher(rule: &ClassificationRule) -> bool {
        !rule.keywords.is_empty()
            || !rule.patterns.is_empty()
            || rule.min_length.is_some()
            || rule.max_length.is_some()
    }

    fn ensure_rule_defaults(rule: &mut ClassificationRule, hint: &str) {
        if !Self::has_rule_matcher(rule) {
            rule.keywords = vec![hint.to_string()];
        }
    }

    fn handle_get(&self) -> anyhow::Result<ToolResult> {
        let cfg = self.load_config_without_env()?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&Self::snapshot(&cfg))?,
            error: None,
        })
    }

    fn handle_list_hints(&self) -> anyhow::Result<ToolResult> {
        let cfg = self.load_config_without_env()?;
        let mut route_hints: Vec<String> =
            cfg.model_routes.iter().map(|r| r.hint.clone()).collect();
        route_hints.sort();
        route_hints.dedup();

        let mut classification_hints: Vec<String> = cfg
            .query_classification
            .rules
            .iter()
            .map(|r| r.hint.clone())
            .collect();
        classification_hints.sort();
        classification_hints.dedup();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "model_route_hints": route_hints,
                "classification_hints": classification_hints,
                "example": {
                    "conversation": {
                        "action": "upsert_scenario",
                        "hint": "conversation",
                        "model_provider": "kimi",
                        "model": "moonshot-v1-8k",
                        "classification_enabled": false
                    },
                    "coding": {
                        "action": "upsert_scenario",
                        "hint": "coding",
                        "model_provider": "openai",
                        "model": "gpt-5.3-codex",
                        "classification_enabled": true,
                        "keywords": ["code", "bug", "refactor", "test"],
                        "patterns": ["```"],
                        "priority": 50
                    }
                }
            }))?,
            error: None,
        })
    }

    async fn handle_set_default(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let provider_update = Self::parse_optional_string_update(args, "model_provider")?;
        let model_update = Self::parse_optional_string_update(args, "model")?;
        let temperature_update = Self::parse_optional_f64_update(args, "temperature")?;

        let any_update = !matches!(provider_update, MaybeSet::Unset)
            || !matches!(model_update, MaybeSet::Unset)
            || !matches!(temperature_update, MaybeSet::Unset);

        if !any_update {
            anyhow::bail!(
                "set_default requires at least one of: model_provider, model, temperature"
            );
        }

        let mut cfg = self.load_config_without_env()?;

        // Determine which models entry to update.
        let (type_k, alias_k) = match &provider_update {
            MaybeSet::Set(model_provider) => model_provider
                .split_once('.')
                .map(|(t, a)| (t.to_string(), a.to_string()))
                .unwrap_or_else(|| (model_provider.clone(), "default".to_string())),
            MaybeSet::Null | MaybeSet::Unset => {
                // Update whichever entry already exists, or create a placeholder.
                cfg.providers
                    .models
                    .iter_entries()
                    .next()
                    .map(|(t, a, _)| (t.to_string(), a.to_string()))
                    .unwrap_or_else(|| ("custom".to_string(), "default".to_string()))
            }
        };

        // Capture previous provider entry for rollback on probe failure.
        let previous_provider_entry = cfg.providers.models.find(&type_k, &alias_k).cloned();
        let entry = cfg
            .providers
            .models
            .ensure(&type_k, &alias_k)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "model_provider_type": &type_k,
                            "alias": &alias_k,
                        })),
                    "model_routing_config: unknown model_provider type"
                );
                anyhow::Error::msg(format!(
                    "unknown model_provider type `{type_k}`. no typed slot in ModelProviders"
                ))
            })?;

        match model_update {
            MaybeSet::Set(model) => entry.model = Some(model),
            MaybeSet::Null => entry.model = None,
            MaybeSet::Unset => {}
        }

        match temperature_update {
            MaybeSet::Set(temperature) => {
                if !(0.0..=2.0).contains(&temperature) {
                    anyhow::bail!("'temperature' must be between 0.0 and 2.0");
                }
                entry.temperature = Some(temperature);
            }
            MaybeSet::Null => {
                entry.temperature = None;
            }
            MaybeSet::Unset => {}
        }

        cfg.save().await?;

        // Probe the new model with a minimal API call to catch invalid model IDs
        // before the channel hot-reload picks up the change.
        let current_model = cfg
            .providers
            .models
            .find(&type_k, &alias_k)
            .and_then(|e| e.model.clone());
        let provider_name = format!("{type_k}.{alias_k}");
        if let Some(model_name) = current_model
            && let Err(probe_err) = self.probe_model(&provider_name, &model_name).await
        {
            if zeroclaw_providers::reliable::is_non_retryable(&probe_err) {
                let reverted_model = previous_provider_entry
                    .as_ref()
                    .and_then(|e| e.model.as_deref())
                    .unwrap_or("(none)")
                    .to_string();

                // Rollback: restore the previous entry's baseline fields for
                // this type.alias slot. Family-specific extras on the typed
                // family config are NOT touched — they survive the modify+
                // restore cycle because we only ever mutated baseline fields
                // (model, temperature, api_key) above.
                if let Some(prev_entry) = previous_provider_entry
                    && let Some(slot) = cfg.providers.models.ensure(&type_k, &alias_k)
                {
                    *slot = prev_entry;
                }
                cfg.save().await?;

                return Ok(ToolResult {
                    success: false,
                    output: format!(
                        "Model '{model_name}' is not available: {probe_err}. Reverted to '{reverted_model}'.",
                    ),
                    error: None,
                });
            }
            // Retryable errors (e.g. transient network issues) — keep the
            // new config and let the resilient wrapper handle retries.
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model": model_name, "probe_err": probe_err.to_string()})), "Model probe returned retryable error (keeping new config)");
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Default model_provider/model settings updated",
                "config": Self::snapshot(&cfg),
            }))?,
            error: None,
        })
    }

    /// Send a minimal 1-token chat request to verify the model is accessible.
    /// Returns `Ok(())` if the probe succeeds **or** if no API key is available
    /// (the probe would fail with an auth error unrelated to model validity).
    /// ModelProvider construction failures are also treated as non-fatal.
    async fn probe_model(&self, provider_name: &str, model: &str) -> anyhow::Result<()> {
        // Use the runtime config's API key (which includes env-sourced keys),
        // not the on-disk config (which may have no key at all).
        let (family, alias) = provider_name
            .split_once('.')
            .unwrap_or((provider_name, "default"));
        let entry = self.config.providers.models.find(family, alias);
        let api_key = entry.and_then(|e| e.api_key.as_deref());
        if api_key.is_none_or(|k| k.trim().is_empty()) {
            return Ok(());
        }

        let model_provider = match zeroclaw_providers::create_model_provider_with_url(
            provider_name,
            api_key,
            entry.and_then(|e| e.uri.as_deref()),
        ) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };

        // Greedy sampling: the ping is a liveness check, not a generation task.
        const PING_TEMPERATURE: f64 = 0.0;
        model_provider
            .chat_with_system(
                Some("Respond with OK."),
                "ping",
                model,
                Some(PING_TEMPERATURE),
            )
            .await?;

        Ok(())
    }

    async fn handle_upsert_scenario(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let hint = Self::parse_non_empty_string(args, "hint")?;
        let model_provider = Self::parse_non_empty_string(args, "model_provider")?;
        let model = Self::parse_non_empty_string(args, "model")?;
        let api_key_update = Self::parse_optional_string_update(args, "api_key")?;

        let keywords_update = if let Some(raw) = args.get("keywords") {
            Some(Self::parse_string_list(raw, "keywords")?)
        } else {
            None
        };
        let patterns_update = if let Some(raw) = args.get("patterns") {
            Some(Self::parse_string_list(raw, "patterns")?)
        } else {
            None
        };
        let min_length_update = Self::parse_optional_usize_update(args, "min_length")?;
        let max_length_update = Self::parse_optional_usize_update(args, "max_length")?;
        let priority_update = Self::parse_optional_i32_update(args, "priority")?;
        let classification_enabled = Self::parse_optional_bool(args, "classification_enabled")?;

        let should_touch_rule = classification_enabled.is_some()
            || keywords_update.is_some()
            || patterns_update.is_some()
            || !matches!(min_length_update, MaybeSet::Unset)
            || !matches!(max_length_update, MaybeSet::Unset)
            || !matches!(priority_update, MaybeSet::Unset);

        let mut cfg = self.load_config_without_env()?;

        let existing_route = cfg
            .model_routes
            .iter()
            .find(|route| route.hint == hint)
            .cloned();

        let mut next_route = existing_route.unwrap_or(ModelRouteConfig {
            hint: hint.clone(),
            model_provider: model_provider.clone(),
            model: model.clone(),
            api_key: None,
        });

        next_route.hint = hint.clone();
        next_route.model_provider = model_provider;
        next_route.model = model;

        match api_key_update {
            MaybeSet::Set(api_key) => next_route.api_key = Some(api_key),
            MaybeSet::Null => next_route.api_key = None,
            MaybeSet::Unset => {}
        }

        cfg.model_routes.retain(|route| route.hint != hint);
        cfg.model_routes.push(next_route);
        Self::normalize_and_sort_routes(&mut cfg.model_routes);

        if should_touch_rule {
            if matches!(classification_enabled, Some(false)) {
                cfg.query_classification
                    .rules
                    .retain(|rule| rule.hint != hint);
            } else {
                let existing_rule = cfg
                    .query_classification
                    .rules
                    .iter()
                    .find(|rule| rule.hint == hint)
                    .cloned();

                let mut next_rule = existing_rule.unwrap_or_else(|| ClassificationRule {
                    hint: hint.clone(),
                    ..ClassificationRule::default()
                });

                if let Some(keywords) = keywords_update {
                    next_rule.keywords = keywords;
                }
                if let Some(patterns) = patterns_update {
                    next_rule.patterns = patterns;
                }

                match min_length_update {
                    MaybeSet::Set(value) => next_rule.min_length = Some(value),
                    MaybeSet::Null => next_rule.min_length = None,
                    MaybeSet::Unset => {}
                }

                match max_length_update {
                    MaybeSet::Set(value) => next_rule.max_length = Some(value),
                    MaybeSet::Null => next_rule.max_length = None,
                    MaybeSet::Unset => {}
                }

                match priority_update {
                    MaybeSet::Set(value) => next_rule.priority = value,
                    MaybeSet::Null => next_rule.priority = 0,
                    MaybeSet::Unset => {}
                }

                if matches!(classification_enabled, Some(true)) {
                    Self::ensure_rule_defaults(&mut next_rule, &hint);
                }

                if !Self::has_rule_matcher(&next_rule) {
                    anyhow::bail!(
                        "Classification rule for hint '{hint}' has no matching criteria. Provide keywords/patterns or set min_length/max_length."
                    );
                }

                cfg.query_classification
                    .rules
                    .retain(|rule| rule.hint != hint);
                cfg.query_classification.rules.push(next_rule);
            }
        }

        Self::normalize_and_sort_rules(&mut cfg.query_classification.rules);
        cfg.query_classification.enabled = !cfg.query_classification.rules.is_empty();

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Scenario route upserted",
                "hint": hint,
                "config": Self::snapshot(&cfg),
            }))?,
            error: None,
        })
    }

    async fn handle_remove_scenario(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let hint = Self::parse_non_empty_string(args, "hint")?;
        let remove_classification = args
            .get("remove_classification")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let mut cfg = self.load_config_without_env()?;

        let before_routes = cfg.model_routes.len();
        cfg.model_routes.retain(|route| route.hint != hint);
        let routes_removed = before_routes.saturating_sub(cfg.model_routes.len());

        let mut rules_removed = 0usize;
        if remove_classification {
            let before_rules = cfg.query_classification.rules.len();
            cfg.query_classification
                .rules
                .retain(|rule| rule.hint != hint);
            rules_removed = before_rules.saturating_sub(cfg.query_classification.rules.len());
        }

        if routes_removed == 0 && rules_removed == 0 {
            anyhow::bail!("No scenario found for hint '{hint}'");
        }

        Self::normalize_and_sort_routes(&mut cfg.model_routes);
        Self::normalize_and_sort_rules(&mut cfg.query_classification.rules);
        cfg.query_classification.enabled = !cfg.query_classification.rules.is_empty();

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Scenario removed",
                "hint": hint,
                "routes_removed": routes_removed,
                "classification_rules_removed": rules_removed,
                "config": Self::snapshot(&cfg),
            }))?,
            error: None,
        })
    }

    async fn handle_upsert_agent(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let name = Self::parse_non_empty_string(args, "name")?;
        let model_provider = Self::parse_non_empty_string(args, "model_provider")?;
        let model = Self::parse_non_empty_string(args, "model")?;

        let api_key_update = Self::parse_optional_string_update(args, "api_key")?;
        let temperature_update = Self::parse_optional_f64_update(args, "temperature")?;
        let max_depth_update = Self::parse_optional_u32_update(args, "max_depth")?;
        let max_iterations_update = Self::parse_optional_usize_update(args, "max_iterations")?;
        let agentic_update = Self::parse_optional_bool(args, "agentic")?;

        let allowed_tools_update = if let Some(raw) = args.get("allowed_tools") {
            Some(Self::parse_string_list(raw, "allowed_tools")?)
        } else {
            None
        };

        let mut cfg = self.load_config_without_env()?;

        // synthesize providers.models[model_provider_family][name] from inline brain params.
        // The arg is the family name (e.g. "openai"); the agent's `model_provider`
        // reference becomes the dotted form (e.g. "openai.coder").
        let model_provider_family = model_provider;
        let agent_model_provider_ref = format!("{model_provider_family}.{name}");
        {
            let provider_entry =
                cfg.providers.models
                    .ensure(&model_provider_family, &name)
                    .ok_or_else(|| {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "model_provider_family": &model_provider_family,
                                    "name": &name,
                                })),
                            "model_routing_config: unknown model_provider family"
                        );
                        anyhow::Error::msg(format!(
                            "unknown model_provider type `{model_provider_family}`. no typed slot in ModelProviders"
                        ))
                    })?;
            provider_entry.model = Some(model.clone());
            match api_key_update {
                MaybeSet::Set(ref v) => provider_entry.api_key = Some(v.clone()),
                MaybeSet::Null => provider_entry.api_key = None,
                MaybeSet::Unset => {}
            }
            match temperature_update {
                MaybeSet::Set(value) => {
                    if !(0.0..=2.0).contains(&value) {
                        anyhow::bail!("'temperature' must be between 0.0 and 2.0");
                    }
                    provider_entry.temperature = Some(value);
                }
                MaybeSet::Null => provider_entry.temperature = None,
                MaybeSet::Unset => {}
            }
        }

        // synthesize risk_profiles[name] from allowed_tools (authorization).
        {
            let risk = cfg.risk_profiles.entry(name.clone()).or_default();
            if let Some(tools) = allowed_tools_update {
                risk.allowed_tools = tools;
            }
        }

        // synthesize runtime_profiles[name] from agentic/max_iterations/max_depth.
        {
            let runtime = cfg.runtime_profiles.entry(name.clone()).or_default();
            if let Some(agentic) = agentic_update {
                runtime.agentic = agentic;
            }
            if let MaybeSet::Set(iters) = max_iterations_update {
                if iters == 0 {
                    anyhow::bail!("'max_iterations' must be greater than 0");
                }
                runtime.max_tool_iterations = iters;
            } else if runtime.max_tool_iterations == 0 {
                runtime.max_tool_iterations = DEFAULT_AGENT_MAX_ITERATIONS;
            }
            if let MaybeSet::Set(depth) = max_depth_update {
                if depth == 0 {
                    anyhow::bail!("'max_depth' must be greater than 0");
                }
                runtime.max_delegation_depth = depth;
            } else if runtime.max_delegation_depth == 0 {
                runtime.max_delegation_depth = DEFAULT_AGENT_MAX_DEPTH;
            }
            if runtime.agentic {
                let allowed_tools_empty = cfg
                    .risk_profiles
                    .get(&name)
                    .is_none_or(|r| r.allowed_tools.is_empty());
                if allowed_tools_empty {
                    anyhow::bail!("Agent '{name}' has agentic=true but allowed_tools is empty.");
                }
            }
        }

        // Get or create the agent and wire up alias references.
        let next_agent = cfg.agents.entry(name.clone()).or_default();
        next_agent.model_provider = agent_model_provider_ref.into();
        next_agent.risk_profile = name.clone().into();
        next_agent.runtime_profile = name.clone().into();

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Delegate agent upserted",
                "name": name,
                "config": Self::snapshot(&cfg),
            }))?,
            error: None,
        })
    }

    async fn handle_remove_agent(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let name = Self::parse_non_empty_string(args, "name")?;

        let mut cfg = self.load_config_without_env()?;
        if cfg.agents.remove(&name).is_none() {
            anyhow::bail!("No aliased agent found with name '{name}'");
        }

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Aliased agent removed",
                "name": name,
                "config": Self::snapshot(&cfg),
            }))?,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for ModelRoutingConfigTool {
    fn name(&self) -> &str {
        "model_routing_config"
    }

    fn description(&self) -> &str {
        "Manage default model settings, scenario-based model_provider/model routes, classification rules, and aliased agent profiles"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "get",
                        "list_hints",
                        "set_default",
                        "upsert_scenario",
                        "remove_scenario",
                        "upsert_agent",
                        "remove_agent"
                    ],
                    "default": "get"
                },
                "hint": {
                    "type": "string",
                    "description": "Scenario hint name (for example: conversation, coding, reasoning)"
                },
                "model_provider": {
                    "type": "string",
                    "description": "ModelProvider for set_default/upsert_scenario/upsert_agent"
                },
                "model": {
                    "type": "string",
                    "description": "Model for set_default/upsert_scenario/upsert_agent"
                },
                "temperature": {
                    "type": ["number", "null"],
                    "description": "Optional temperature override (0.0-2.0)"
                },
                "api_key": {
                    "type": ["string", "null"],
                    "description": "Optional API key override for scenario route or aliased agent"
                },
                "keywords": {
                    "description": "Classification keywords for upsert_scenario (string or string array)",
                    "oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "patterns": {
                    "description": "Classification literal patterns for upsert_scenario (string or string array)",
                    "oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "min_length": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Optional minimum message length matcher"
                },
                "max_length": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Optional maximum message length matcher"
                },
                "priority": {
                    "type": ["integer", "null"],
                    "description": "Classification priority (higher runs first)"
                },
                "classification_enabled": {
                    "type": "boolean",
                    "description": "When true, upsert classification rule for this hint; false removes it"
                },
                "remove_classification": {
                    "type": "boolean",
                    "description": "When remove_scenario, whether to remove matching classification rule (default true)"
                },
                "name": {
                    "type": "string",
                    "description": "Aliased agent name for upsert_agent/remove_agent"
                },
                "max_depth": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Delegate max recursion depth"
                },
                "agentic": {
                    "type": "boolean",
                    "description": "Enable tool-call loop mode for aliased agent"
                },
                "allowed_tools": {
                    "description": "Allowed tools for agentic delegate mode (string or string array)",
                    "oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "max_iterations": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Maximum tool-call iterations for agentic delegate mode"
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("get")
            .to_ascii_lowercase();

        let result = match action.as_str() {
            "get" => self.handle_get(),
            "list_hints" => self.handle_list_hints(),
            "set_default" | "upsert_scenario" | "remove_scenario" | "upsert_agent"
            | "remove_agent" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }

                match action.as_str() {
                    "set_default" => Box::pin(self.handle_set_default(&args)).await,
                    "upsert_scenario" => Box::pin(self.handle_upsert_scenario(&args)).await,
                    "remove_scenario" => Box::pin(self.handle_remove_scenario(&args)).await,
                    "upsert_agent" => Box::pin(self.handle_upsert_agent(&args)).await,
                    "remove_agent" => Box::pin(self.handle_remove_agent(&args)).await,
                    _ => unreachable!("validated above"),
                }
            }
            _ => anyhow::bail!(
                "Unknown action '{action}'. Valid: get, list_hints, set_default, upsert_scenario, remove_scenario, upsert_agent, remove_agent"
            ),
        };

        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn readonly_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    async fn test_config(tmp: &TempDir) -> Arc<Config> {
        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.save().await.unwrap();
        Arc::new(config)
    }

    fn read_saved_provider_entry(
        cfg_path: &std::path::Path,
        family: &str,
        alias: &str,
    ) -> Option<zeroclaw_config::schema::ModelProviderConfig> {
        let contents = std::fs::read_to_string(cfg_path).ok()?;
        let cfg = zeroclaw_config::migration::migrate_to_current(&contents).ok()?;
        cfg.providers.models.find(family, alias).cloned()
    }

    #[tokio::test]
    async fn set_default_updates_provider_model_and_temperature() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        let result = tool
            .execute(json!({
                "action": "set_default",
                "model_provider": "moonshot",
                "model": "moonshot-v1-8k",
                "temperature": 0.2
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let entry = read_saved_provider_entry(&cfg_path, "moonshot", "default")
            .expect("set_default must materialize the moonshot.default slot");
        assert_eq!(entry.model.as_deref(), Some("moonshot-v1-8k"));
        assert_eq!(entry.temperature, Some(0.2));
    }

    #[tokio::test]
    async fn upsert_scenario_creates_route_and_rule() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        let result = tool
            .execute(json!({
                "action": "upsert_scenario",
                "hint": "coding",
                "model_provider": "openai",
                "model": "gpt-5.3-codex",
                "classification_enabled": true,
                "keywords": ["code", "bug", "refactor"],
                "patterns": ["```"],
                "priority": 50
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        assert!(get_result.success);
        let output: Value = serde_json::from_str(&get_result.output).unwrap();

        assert_eq!(output["query_classification"]["enabled"], json!(true));

        let scenarios = output["scenarios"].as_array().unwrap();
        assert!(scenarios.iter().any(|item| {
            item["hint"] == json!("coding")
                && item["model_provider"] == json!("openai")
                && item["model"] == json!("gpt-5.3-codex")
        }));
    }

    #[tokio::test]
    async fn remove_scenario_also_removes_rule() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        let _ = tool
            .execute(json!({
                "action": "upsert_scenario",
                "hint": "coding",
                "model_provider": "openai",
                "model": "gpt-5.3-codex",
                "classification_enabled": true,
                "keywords": ["code"]
            }))
            .await
            .unwrap();

        let removed = tool
            .execute(json!({
                "action": "remove_scenario",
                "hint": "coding"
            }))
            .await
            .unwrap();
        assert!(removed.success, "{:?}", removed.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(output["query_classification"]["enabled"], json!(false));
        assert!(output["scenarios"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_and_remove_delegate_agent() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        let upsert = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "coder",
                "model_provider": "openai",
                "model": "gpt-5.3-codex",
                "agentic": true,
                "allowed_tools": ["file_read", "file_write", "shell"],
                "max_iterations": 6
            }))
            .await
            .unwrap();
        assert!(upsert.success, "{:?}", upsert.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        // V3 surfaces the dotted alias ref on the agent. The actual model
        // string lives under model_providers.openai.coder (synthesized
        // from the `model` upsert arg).
        assert_eq!(
            output["agents"]["coder"]["model_provider"],
            json!("openai.coder")
        );
        assert_eq!(output["agents"]["coder"]["agentic"], json!(true));

        let remove = tool
            .execute(json!({
                "action": "remove_agent",
                "name": "coder"
            }))
            .await
            .unwrap();
        assert!(remove.success, "{:?}", remove.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        assert!(output["agents"]["coder"].is_null());
    }

    #[tokio::test]
    async fn read_only_mode_blocks_mutating_actions() {
        let tmp = TempDir::new().unwrap();
        let tool =
            ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, readonly_security());

        let result = tool
            .execute(json!({
                "action": "set_default",
                "model_provider": "openai"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("read-only"));
    }

    #[tokio::test]
    async fn set_default_skips_probe_without_api_key() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        let result = tool
            .execute(json!({
                "action": "set_default",
                "model_provider": "anthropic",
                "model": "totally-fake-model-12345"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let entry = read_saved_provider_entry(&cfg_path, "anthropic", "default")
            .expect("set_default must materialize the anthropic.default slot");
        assert_eq!(entry.model.as_deref(), Some("totally-fake-model-12345"));
    }

    #[tokio::test]
    async fn set_default_temperature_only_skips_probe() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        let result = tool
            .execute(json!({
                "action": "set_default",
                "temperature": 1.5
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let entry = read_saved_provider_entry(&cfg_path, "custom", "default")
            .expect("temperature-only set_default must create the custom.default placeholder slot");
        assert_eq!(entry.temperature, Some(1.5));
    }
}
