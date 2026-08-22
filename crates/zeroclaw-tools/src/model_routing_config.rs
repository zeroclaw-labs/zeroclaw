use crate::util_helpers::MaybeSet;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{ClassificationRule, Config, DelegateTargetConfig, ModelRouteConfig};
use zeroclaw_providers::ProviderDispatch;

const DEFAULT_AGENT_MAX_DEPTH: u32 = 3;
const DEFAULT_AGENT_MAX_ITERATIONS: usize = 10;

/// Why a model probe failed.
///
/// A local provider-construction failure is not a transient request failure:
/// nothing was ever sent, and the saved configuration cannot produce a usable
/// provider on reload. The linked issue requires that such a failure cannot pass
/// model validation, so the two are kept distinct all the way to the operator-visible
/// `ToolResult` instead of being flattened into one `anyhow::Error` and sorted
/// by the chat-oriented `is_non_retryable` heuristic.
#[derive(Debug)]
enum ProbeFailure {
    /// The provider could not be built from the effective configuration.
    Construction(anyhow::Error),
    /// The provider was built and the ping request itself failed.
    Request(anyhow::Error),
}

impl ProbeFailure {
    fn error(&self) -> &anyhow::Error {
        match self {
            Self::Construction(error) | Self::Request(error) => error,
        }
    }

    /// True when the new configuration must not be kept.
    ///
    /// Construction failures are always fatal. Request failures defer to the
    /// existing retry classifier, so transient network conditions still keep
    /// the new config.
    fn is_fatal(&self) -> bool {
        match self {
            Self::Construction(_) => true,
            Self::Request(error) => zeroclaw_providers::reliable::is_non_retryable(error),
        }
    }
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error())
    }
}

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
                output: ToolOutput::default(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Some(ToolResult {
                success: false,
                output: ToolOutput::default(),
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

    fn parse_delegate_targets(
        raw: &Value,
        field: &str,
    ) -> anyhow::Result<Vec<DelegateTargetConfig>> {
        // Keep the config-editing tool as permissive as the schema loader:
        // operators may pass a comma-separated legacy string, a string array,
        // or object entries with explicit mode. The stored config still uses
        // `DelegateTargetConfig`, so mode semantics are not reimplemented here.
        if let Some(raw_string) = raw.as_str() {
            return Ok(raw_string
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(DelegateTargetConfig::bounded)
                .collect());
        }

        if let Some(array) = raw.as_array() {
            let mut out = Vec::new();
            for item in array {
                let mut target: DelegateTargetConfig =
                    serde_json::from_value(item.clone()).map_err(|error| {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "field": field,
                                "error": format!("{}", error),
                            })),
                            "model_routing_config: delegate target element has invalid shape"
                        );
                        anyhow::Error::msg(format!(
                            "'{field}' array must contain strings or objects with agent/mode: {error}"
                        ))
                    })?;
                target.agent = target.agent.trim().to_string();
                if !target.agent.is_empty() {
                    out.push(target);
                }
            }
            return Ok(out);
        }

        anyhow::bail!("'{field}' must be a string, string[], or delegate target object[]")
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
                    "delegate_same_risk_profile": agent.delegate_same_risk_profile,
                    "delegates": agent.delegates,
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
            output: serde_json::to_string_pretty(&Self::snapshot(&cfg))?.into(),
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
            }))?
            .into(),
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
            && let Err(probe_err) = self.probe_model(&cfg, &provider_name, &model_name).await
        {
            if probe_err.is_fatal() {
                let reverted_model = previous_provider_entry
                    .as_ref()
                    .and_then(|e| e.model.as_deref())
                    .unwrap_or("(none)")
                    .to_string();

                // Restore the previous entry when there was one. When there
                // was not, this `set_default` call is what created the alias,
                // so leaving it behind would persist an alias that just failed
                // validation — and report `Reverted to '(none)'` while doing
                // it. Remove it instead, so the rollback claim holds for a
                // newly added alias as well.
                let reverted_to = match previous_provider_entry {
                    Some(prev_entry) => {
                        if let Some(slot) = cfg.providers.models.ensure(&type_k, &alias_k) {
                            *slot = prev_entry;
                        }
                        format!("Reverted to '{reverted_model}'.")
                    }
                    None => {
                        cfg.providers.models.remove_alias(&type_k, &alias_k);
                        format!("Removed the newly created '{provider_name}' alias.")
                    }
                };
                cfg.save().await?;

                return Ok(ToolResult {
                    success: false,
                    output: format!(
                        "Model '{model_name}' is not available: {probe_err}. {reverted_to}"
                    )
                    .into(),
                    error: Some(probe_err.to_string()),
                });
            }
            // Retryable request errors (e.g. transient network issues) — keep
            // the new config and let the resilient wrapper handle retries.
            // Construction failures never reach here; they are fatal above.
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"model": model_name, "probe_err": probe_err.to_string()})), "Model probe returned retryable error (keeping new config)");
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Default model_provider/model settings updated",
                "config": Self::snapshot(&cfg),
            }))?
            .into(),
            error: None,
        })
    }

    /// Build the configuration the probe validates against, using the same
    /// precedence a post-save reload applies.
    ///
    /// `cfg` was just loaded via `load_config_without_env`, which parses the
    /// on-disk TOML as-is and never decrypts `#[secret]` fields — so any
    /// persisted API key is still `enc2:`-ciphertext at this point, exactly
    /// as `save()` last wrote it. `load_or_init` decrypts and *then* applies
    /// the `ZEROCLAW_*` override layer, which is the final in-memory
    /// authority (see `docs/book/src/reference/env-vars.md`, persistence
    /// boundary). Reproducing only the decrypt half would validate the model
    /// against whatever is on disk while the reloaded runtime uses the
    /// environment values — the probe could then authenticate against the
    /// wrong account, or send an environment-provided credential to the
    /// stored URI rather than the effective one. Overrides also reach sibling
    /// fields the probe depends on (`uri`, OAuth material, headers, timeouts,
    /// and every runtime option resolved by
    /// `provider_runtime_options_for_alias`), so the layer is applied whole
    /// rather than field by field.
    ///
    /// This is a private working copy; the caller's `cfg` — and therefore
    /// what reaches disk — is untouched.
    fn effective_config_for_probe(cfg: &Config) -> anyhow::Result<Config> {
        let mut effective = cfg.clone();
        let zeroclaw_dir = effective
            .config_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| effective.config_path.clone());
        let store =
            zeroclaw_config::security::SecretStore::new(&zeroclaw_dir, effective.secrets.encrypt);
        effective.decrypt_secrets(&store)?;
        let applied = zeroclaw_config::env_overrides::apply_env_overrides(&mut effective)?;
        effective.env_overridden_paths = applied.paths;
        effective.pre_override_snapshots = applied.snapshots;
        Ok(effective)
    }

    /// Send a minimal 1-token chat request to verify the model is accessible.
    /// Returns `Ok(())` if the probe succeeds **or** if no API key resolves for
    /// the saved alias (checking the runtime snapshot too — see below), or the
    /// saved config's secrets cannot be decrypted (either way the probe would
    /// fail with an auth error unrelated to model validity, and an operator may
    /// be configuring the alias while offline). A failure to construct the
    /// model_provider is surfaced as an `Err` like any other probe failure,
    /// rather than treated as a passing probe.
    ///
    /// Errors are classified via [`ProbeFailure`] so the caller can tell a
    /// local construction failure from a request failure.
    async fn probe_model(
        &self,
        cfg: &Config,
        provider_name: &str,
        model: &str,
    ) -> Result<(), ProbeFailure> {
        // Resolve alias identity, endpoint, credentials, and runtime options
        // from the effective post-reload view of what was just saved — disk
        // decrypted, then the `ZEROCLAW_*` layer applied on top — rather than
        // from the pre-update runtime snapshot or from disk alone.
        let Ok(mut effective) = Self::effective_config_for_probe(cfg) else {
            return Ok(());
        };
        let (family, alias) = provider_name
            .split_once('.')
            .unwrap_or((provider_name, "default"));

        // `effective` already carries any env-sourced credential for this
        // alias, so it is the authority. The runtime snapshot remains a
        // fallback for a key this process resolved at boot through a path the
        // override layer cannot reproduce here.
        let effective_key = effective
            .providers
            .models
            .find(family, alias)
            .and_then(|e| e.api_key.clone())
            .filter(|key| !key.trim().is_empty());
        let api_key = match effective_key {
            Some(key) => Some(key),
            None => self
                .config
                .providers
                .models
                .find(family, alias)
                .and_then(|e| e.api_key.clone())
                .filter(|key| !key.trim().is_empty()),
        };
        let Some(api_key) = api_key else {
            return Ok(());
        };

        // Carry the resolved key on the config handed to the factory so
        // alias-specific credentials resolve correctly regardless of which
        // config they came from.
        if let Some(entry) = effective.providers.models.ensure(family, alias) {
            entry.api_key = Some(api_key);
        }

        // Probe the model the post-reload runtime will actually serve. A
        // `ZEROCLAW_*` override on the alias's `model` field is authoritative
        // in memory after load, so probing the saved value would validate a
        // model the runtime never uses — and could roll the disk change back
        // over a model that was never going to be served.
        let effective_model = effective
            .providers
            .models
            .find(family, alias)
            .and_then(|e| e.model.clone())
            .filter(|value| !value.trim().is_empty());
        let probe_model_name = effective_model.as_deref().unwrap_or(model);

        let model_provider =
            zeroclaw_providers::create_model_provider_from_ref(&effective, provider_name)
                .map_err(ProbeFailure::Construction)?;

        // Greedy sampling: the ping is a liveness check, not a generation task.
        const PING_TEMPERATURE: f64 = 0.0;
        ProviderDispatch::from_ref(&*model_provider)
            .chat_with_system(
                Some("Respond with OK."),
                "ping",
                probe_model_name,
                Some(PING_TEMPERATURE),
            )
            .await
            .map_err(ProbeFailure::Request)?;

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
            }))?
            .into(),
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
            }))?
            .into(),
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

        let delegate_same_risk_profile_update =
            Self::parse_optional_bool(args, "delegate_same_risk_profile")?;
        let delegates_update = if let Some(raw) = args.get("delegates") {
            Some(Self::parse_delegate_targets(raw, "delegates")?)
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
        }

        // Get or create the agent and wire up alias references.
        let next_agent = cfg.agents.entry(name.clone()).or_default();
        next_agent.model_provider = agent_model_provider_ref.into();
        next_agent.risk_profile = name.clone().into();
        next_agent.runtime_profile = name.clone().into();
        if let Some(same_profile) = delegate_same_risk_profile_update {
            next_agent.delegate_same_risk_profile = same_profile;
        }
        if let Some(delegates) = delegates_update {
            next_agent.delegates = delegates;
        }

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Delegate agent upserted",
                "name": name,
                "config": Self::snapshot(&cfg),
            }))?
            .into(),
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
            }))?
            .into(),
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
        let delegates_schema = json!({
            "description": "Explicit delegate roster. Accepts a comma-separated string, string array, or objects with {agent, mode}; mode is bounded or independent.",
            "oneOf": [
                {"type": "string"},
                {
                    "type": "array",
                    "items": {
                        "oneOf": [
                            {"type": "string"},
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["agent"],
                                "properties": {
                                    "agent": {"type": "string", "minLength": 1},
                                    "mode": {
                                        "type": "string",
                                        "enum": ["bounded", "independent"],
                                        "default": "bounded"
                                    }
                                }
                            }
                        ]
                    }
                }
            ]
        });
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
                },
                "delegate_same_risk_profile": {
                    "type": "boolean",
                    "description": "Auto-allow delegation to same-risk-profile peers (default true). Set false to restrict reach to the explicit delegates list."
                },
                "delegates": delegates_schema
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
                output: ToolOutput::default(),
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
    async fn upsert_agent_writes_delegate_roster_fields() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        // Create the explicit delegate target first so the roster names a
        // real agent (snapshot/readback does not validate, but keep it real).
        let _ = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "aaalore",
                "model_provider": "openai",
                "model": "gpt-5.3"
            }))
            .await
            .unwrap();

        let upsert = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "aaa",
                "model_provider": "openai",
                "model": "gpt-5.3",
                "delegate_same_risk_profile": false,
                "delegates": [{"agent": "aaalore", "mode": "independent"}]
            }))
            .await
            .unwrap();
        assert!(upsert.success, "{:?}", upsert.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(
            output["agents"]["aaa"]["delegate_same_risk_profile"],
            json!(false)
        );
        assert_eq!(
            output["agents"]["aaa"]["delegates"],
            json!([{"agent": "aaalore", "mode": "independent"}])
        );
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

    #[tokio::test]
    async fn probe_model_uses_saved_alias_config_not_stale_snapshot() {
        let tmp = TempDir::new().unwrap();
        let stale_snapshot = test_config(&tmp).await;
        let tool = ModelRoutingConfigTool::new(stale_snapshot.clone(), test_security());

        // The tool's own construction-time snapshot has no credentials for
        // this alias at all, so probing directly against it is a legitimate
        // skip (no API key means nothing to validate).
        let against_stale = tool
            .probe_model(stale_snapshot.as_ref(), "openai.default", "gpt-test")
            .await;
        assert!(against_stale.is_ok(), "{against_stale:?}");

        // A config that has since been saved carries a credential the stale
        // snapshot never had (mismatched on purpose, so construction fails
        // locally rather than reaching the network). Both configs offer the
        // same alias, so this exercises alias resolution, not the runtime
        // credential fallback: the probe must resolve from the config it is
        // given, not fall back to the runtime snapshot captured at tool
        // construction.
        let mut saved = (*stale_snapshot).clone();
        saved
            .providers
            .models
            .ensure("openai", "default")
            .unwrap()
            .api_key = Some("sk-ant-not-an-openai-key".to_string());

        let against_saved = tool.probe_model(&saved, "openai.default", "gpt-test").await;
        assert!(
            against_saved.is_err(),
            "probe must resolve credentials from the saved alias config, not the stale runtime snapshot"
        );
    }

    #[tokio::test]
    async fn probe_model_does_not_swallow_provider_construction_errors() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = (*test_config(&tmp).await).clone();
        cfg.providers
            .models
            .ensure("openai", "default")
            .unwrap()
            .api_key = Some("sk-ant-not-an-openai-key".to_string());
        let tool = ModelRoutingConfigTool::new(Arc::new(cfg.clone()), test_security());

        let result = tool.probe_model(&cfg, "openai.default", "gpt-test").await;
        let error = result.expect_err(
            "a model_provider that fails to construct must not be reported as a passing probe",
        );
        assert!(error.to_string().contains("prefix mismatch"), "{error}");
    }

    #[tokio::test]
    async fn probe_model_falls_back_to_runtime_snapshot_credential_when_saved_config_has_none() {
        let tmp = TempDir::new().unwrap();
        let base = test_config(&tmp).await;

        // Mirrors what `load_or_init` hands the tool at boot: decrypted, and
        // with `ZEROCLAW_*` env-var bridged credentials already resolved
        // onto the alias - something `load_config_without_env` never sees.
        // Mismatched on purpose so a fallback attempt fails locally instead
        // of reaching the network.
        let mut runtime_snapshot = (*base).clone();
        runtime_snapshot
            .providers
            .models
            .ensure("openai", "default")
            .unwrap()
            .api_key = Some("sk-ant-env-sourced-key".to_string());
        let tool = ModelRoutingConfigTool::new(Arc::new(runtime_snapshot), test_security());

        // The saved config - what `load_config_without_env` produced - has
        // no credential for this alias at all.
        let saved = (*base).clone();

        let result = tool.probe_model(&saved, "openai.default", "gpt-test").await;
        assert!(
            result.is_err(),
            "probe must fall back to the runtime snapshot's credential for this alias instead of silently skipping"
        );
    }

    /// `set_default` can create the alias it then probes. On a fatal probe the
    /// rollback must remove that alias: restoring "the previous entry" is a
    /// no-op when there was none, which would persist an alias that just
    /// failed validation while reporting `Reverted to '(none)'`.
    #[tokio::test]
    async fn set_default_fails_and_removes_a_newly_created_alias_on_construction_error() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");

        // The credential reaches the probe through the runtime-snapshot
        // fallback — the container/CI posture where the key is environmental
        // and never written to disk. It is anthropic-shaped on purpose so
        // provider construction fails locally, with no network request.
        let base = test_config(&tmp).await;
        let mut runtime_snapshot = (*base).clone();
        runtime_snapshot
            .providers
            .models
            .ensure("openai", "fresh_alias")
            .unwrap()
            .api_key = Some("sk-ant-not-an-openai-key".to_string());
        let tool = ModelRoutingConfigTool::new(Arc::new(runtime_snapshot), test_security());

        // Nothing for this alias exists on disk, so the call below is what
        // creates it.
        assert!(
            read_saved_provider_entry(&cfg_path, "openai", "fresh_alias").is_none(),
            "precondition: the alias must not exist before set_default creates it"
        );

        let result = tool
            .execute(json!({
                "action": "set_default",
                "model_provider": "openai.fresh_alias",
                "model": "gpt-test-model"
            }))
            .await
            .unwrap();

        // Nothing was ever sent and the saved config cannot build a usable
        // provider on reload, so this cannot be reported as a successful
        // update.
        assert!(
            !result.success,
            "a provider-construction failure must not pass model validation: {result:?}"
        );
        let output = result.output.to_string();
        assert!(
            output.contains("API key prefix mismatch"),
            "the operator-visible result must name the construction failure: {output}"
        );
        assert!(
            result.error.is_some(),
            "an unsuccessful probe result must carry the error field"
        );
        assert!(
            output.contains("Removed the newly created"),
            "the result must state that the new alias was removed, not that it reverted to \
             '(none)': {output}"
        );

        // Outer boundary: the failed alias must be absent from what was
        // persisted, not merely absent from the in-memory config.
        let persisted = read_saved_provider_entry(&cfg_path, "openai", "fresh_alias");
        assert!(
            persisted.is_none(),
            "a newly created alias must be absent from the persisted config after a failed \
             probe, got {persisted:?}"
        );
    }

    /// The sibling case: when the alias already existed, rollback still
    /// restores the previous entry rather than removing it.
    #[tokio::test]
    async fn set_default_restores_the_previous_entry_on_construction_error() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        let tool = ModelRoutingConfigTool::new(Box::pin(test_config(&tmp)).await, test_security());

        // Seed a real (but provider-mismatched) credential and model onto the
        // alias through a separate write, so the tool's own runtime snapshot
        // never observes it — mirroring the "saved config the process hasn't
        // seen yet" scenario the probe must read from.
        let seed = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "default",
                "model_provider": "openai",
                "model": "placeholder",
                "api_key": "sk-ant-not-an-openai-key"
            }))
            .await
            .unwrap();
        assert!(seed.success, "{:?}", seed.error);
        let before = read_saved_provider_entry(&cfg_path, "openai", "default")
            .expect("precondition: the alias must exist before set_default updates it");

        let result = tool
            .execute(json!({
                "action": "set_default",
                "model_provider": "openai",
                "model": "gpt-test-model"
            }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "a provider-construction failure must not pass model validation: {result:?}"
        );
        let after = read_saved_provider_entry(&cfg_path, "openai", "default")
            .expect("a pre-existing alias must survive rollback");
        assert_eq!(
            after.model, before.model,
            "rollback must restore the previous model, not keep the failed one"
        );
        assert_ne!(after.model.as_deref(), Some("gpt-test-model"));
    }

    /// Environment overrides are the final in-memory authority after
    /// decryption, so the probe must be built from the same effective
    /// configuration a post-save reload produces — not from the disk values
    /// alone. `set_default` never writes an API key, so when a disk key and a
    /// `ZEROCLAW_*` key both exist, the runtime will use the environment key
    /// while the disk key is merely what `load_config_without_env` parsed.
    ///
    /// Probing the disk key would validate against the wrong account, and
    /// probing the disk `uri` would send an environment-provided credential to
    /// the wrong endpoint.
    #[tokio::test]
    async fn probe_model_prefers_env_overridden_credential_and_endpoint_over_disk() {
        let tmp = TempDir::new().unwrap();
        let base = test_config(&tmp).await;

        // A test-owned alias, so the process-wide env vars below cannot
        // collide with another test's alias.
        const ALIAS: &str = "probe_env_case";

        // Disk carries a well-formed openai key. Alone, it would construct
        // successfully and reach the network. No `uri` here: a custom endpoint
        // disables the provider factory's key-prefix pre-flight, which is the
        // no-network signal phase 1 relies on.
        let mut saved = (*base).clone();
        {
            let entry = saved.providers.models.ensure("openai", ALIAS).unwrap();
            entry.api_key = Some("sk-disk-key".to_string());
        }

        // Phase 1 — the credential the probe actually uses. Only the key is
        // overridden here: the disk key is well-formed openai and would
        // construct cleanly, while the env key is anthropic-shaped, so
        // construction failing is observable proof of which layer was read,
        // with no network request.
        let key_var = "ZEROCLAW_providers__models__openai__probe_env_case__api_key";
        // SAFETY: this test owns this env-var key and removes it before the
        // next phase. The value is synthetic, not a credential.
        unsafe { std::env::set_var(key_var, "sk-ant-env-key") };

        let tool = ModelRoutingConfigTool::new(Arc::clone(&base), test_security());
        let probe = tool
            .probe_model(&saved, &format!("openai.{ALIAS}"), "gpt-test")
            .await;

        // SAFETY: undo the test-only process env mutation above.
        unsafe { std::env::remove_var(key_var) };

        let failure = probe.expect_err(
            "the env-overridden anthropic-shaped key must reach provider construction; the \
             disk key would have constructed cleanly",
        );
        assert!(
            matches!(failure, ProbeFailure::Construction(_)),
            "an unusable effective credential is a construction failure, not a request failure"
        );
        assert!(
            failure.to_string().contains("API key prefix mismatch"),
            "the probe must receive the runtime-effective credential: {failure}"
        );

        // Phase 2 — the sibling fields. Overriding the endpoint too, and
        // inspecting the effective config the probe builds from, proves the
        // whole override layer is applied rather than the credential alone.
        // This phase is pure config resolution; nothing is dispatched.
        let uri_var = "ZEROCLAW_providers__models__openai__probe_env_case__uri";
        // SAFETY: this test owns these env-var keys and removes them below.
        unsafe {
            std::env::set_var(key_var, "sk-ant-env-key");
            std::env::set_var(uri_var, "https://env.invalid/v1");
        }

        let effective = ModelRoutingConfigTool::effective_config_for_probe(&saved);

        // SAFETY: undo the test-only process env mutations above.
        unsafe {
            std::env::remove_var(key_var);
            std::env::remove_var(uri_var);
        }

        let effective = effective.expect("effective config must build");
        let entry = effective
            .providers
            .models
            .find("openai", ALIAS)
            .expect("the alias must survive the override layer");
        assert_eq!(
            entry.api_key.as_deref(),
            Some("sk-ant-env-key"),
            "credential must come from the environment layer"
        );
        assert_eq!(
            entry.uri.as_deref(),
            Some("https://env.invalid/v1"),
            "endpoint must come from the environment layer, or a credential could be sent to the \
             wrong host"
        );
        assert_eq!(
            zeroclaw_providers::provider_runtime_options_for_alias(&effective, "openai", ALIAS)
                .provider_api_url
                .as_deref(),
            Some("https://env.invalid/v1"),
            "the representative runtime option resolved for the probe must also be \
             environment-effective"
        );
    }

    /// The environment layer is the in-memory authority after load, so an
    /// alias `model` override decides what the post-reload runtime serves.
    /// Probing the saved disk value instead would validate — or roll back
    /// over — a model that is never dispatched.
    #[tokio::test]
    async fn probe_model_dispatches_the_env_effective_alias_model_not_the_saved_one() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = TempDir::new().unwrap();
        let base = test_config(&tmp).await;

        // A test-owned alias, so the process-wide env var below cannot
        // collide with another test's alias.
        const ALIAS: &str = "probe_env_model_case";

        let server = MockServer::start().await;
        // Match on the verb alone: the assertion below is about the dispatched
        // `model`, and pinning the alias's endpoint shape here would make this
        // regression fail for an unrelated provider-route change.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "OK" }]
                }]
            })))
            .mount(&server)
            .await;

        // Disk says `gpt-disk`. A custom `uri` points the probe at the mock
        // and disables the factory's key-prefix pre-flight, so the request is
        // actually dispatched and its body is observable.
        let mut saved = (*base).clone();
        {
            let entry = saved.providers.models.ensure("openai", ALIAS).unwrap();
            entry.api_key = Some("sk-disk-key".to_string());
            entry.model = Some("gpt-disk".to_string());
            entry.uri = Some(format!("{}/v1", server.uri()));
        }

        let model_var = "ZEROCLAW_providers__models__openai__probe_env_model_case__model";
        // SAFETY: this test owns this env-var key and removes it below. The
        // value is a synthetic model name, not a credential.
        unsafe { std::env::set_var(model_var, "gpt-env") };

        let tool = ModelRoutingConfigTool::new(Arc::clone(&base), test_security());
        let probe = tool
            .probe_model(&saved, &format!("openai.{ALIAS}"), "gpt-disk")
            .await;

        // SAFETY: undo the test-only process env mutation above.
        unsafe { std::env::remove_var(model_var) };

        assert!(probe.is_ok(), "the mock accepts the probe: {probe:?}");

        let requests = server
            .received_requests()
            .await
            .expect("the mock server records requests");
        assert_eq!(
            requests.len(),
            1,
            "the probe dispatches exactly one request"
        );
        let body: serde_json::Value = requests[0]
            .body_json()
            .expect("the probe sends a JSON chat request");
        assert_eq!(
            body["model"].as_str(),
            Some("gpt-env"),
            "the probe must dispatch the environment-effective alias model; probing the saved \
             `gpt-disk` would validate a model the post-reload runtime never serves"
        );
    }

    /// A request failure that the retry classifier calls transient must still
    /// keep the new config — the construction/request split must not turn
    /// every probe failure into a rollback.
    #[tokio::test]
    async fn probe_failure_is_fatal_only_for_construction_and_non_retryable_requests() {
        assert!(
            ProbeFailure::Construction(anyhow::Error::msg("API key prefix mismatch")).is_fatal(),
            "a local construction failure is never transient"
        );
        assert!(
            !ProbeFailure::Request(anyhow::Error::msg(
                "error sending request: connection reset"
            ))
            .is_fatal(),
            "a transient request failure must keep the new config"
        );
        assert!(
            ProbeFailure::Request(anyhow::Error::msg("404 model not found")).is_fatal(),
            "a non-retryable request failure must still roll back"
        );
    }
}
