use super::traits::{Tool, ToolResult};
use crate::config::{ClassificationRule, Config, DelegateAgentConfig, ModelRouteConfig};
use crate::security::SecurityPolicy;
use crate::util::MaybeSet;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

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
            anyhow::anyhow!(
                "Failed to read config file {}: {error}",
                self.config.config_path.display()
            )
        })?;

        let mut parsed: Config = toml::from_str(&contents).map_err(|error| {
            anyhow::anyhow!(
                "Failed to parse config file {}: {error}",
                self.config.config_path.display()
            )
        })?;
        parsed.config_path = self.config.config_path.clone();
        parsed.workspace_dir = self.config.workspace_dir.clone();
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
                let value = item
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'{field}' array must only contain strings"))?;
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
            .ok_or_else(|| anyhow::anyhow!("Missing '{field}'"))?
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
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a string or null"))?
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

        let value = raw
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a number or null"))?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_usize_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<usize>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let raw_value = raw
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a non-negative integer or null"))?;
        let value = usize::try_from(raw_value)
            .map_err(|_| anyhow::anyhow!("'{field}' is too large for this platform"))?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_u32_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<u32>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let raw_value = raw
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a non-negative integer or null"))?;
        let value =
            u32::try_from(raw_value).map_err(|_| anyhow::anyhow!("'{field}' must fit in u32"))?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_i32_update(args: &Value, field: &str) -> anyhow::Result<MaybeSet<i32>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let raw_value = raw
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be an integer or null"))?;
        let value =
            i32::try_from(raw_value).map_err(|_| anyhow::anyhow!("'{field}' must fit in i32"))?;
        Ok(MaybeSet::Set(value))
    }

    fn parse_optional_bool(args: &Value, field: &str) -> anyhow::Result<Option<bool>> {
        let Some(raw) = args.get(field) else {
            return Ok(None);
        };

        let value = raw
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a boolean"))?;
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
            "provider": route.provider,
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
            agents.insert(
                name.clone(),
                json!({
                    "provider": agent.provider,
                    "model": agent.model,
                    "system_prompt": agent.system_prompt,
                    "api_key_configured": agent
                        .api_key
                        .as_ref()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "temperature": agent.temperature,
                    "max_depth": agent.max_depth,
                    "agentic": agent.agentic,
                    "allowed_tools": agent.allowed_tools,
                    "max_iterations": agent.max_iterations,
                }),
            );
        }

        json!({
            "default": {
                "provider": cfg.default_provider,
                "model": cfg.default_model,
                "temperature": cfg.default_temperature,
            },
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
                        "provider": "kimi",
                        "model": "moonshot-v1-8k",
                        "classification_enabled": false
                    },
                    "coding": {
                        "action": "upsert_scenario",
                        "hint": "coding",
                        "provider": "openai",
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
        let provider_update = Self::parse_optional_string_update(args, "provider")?;
        let model_update = Self::parse_optional_string_update(args, "model")?;
        let temperature_update = Self::parse_optional_f64_update(args, "temperature")?;

        let any_update = !matches!(provider_update, MaybeSet::Unset)
            || !matches!(model_update, MaybeSet::Unset)
            || !matches!(temperature_update, MaybeSet::Unset);

        if !any_update {
            anyhow::bail!("set_default requires at least one of: provider, model, temperature");
        }

        let mut cfg = self.load_config_without_env()?;

        match provider_update {
            MaybeSet::Set(provider) => cfg.default_provider = Some(provider),
            MaybeSet::Null => cfg.default_provider = None,
            MaybeSet::Unset => {}
        }

        match model_update {
            MaybeSet::Set(model) => cfg.default_model = Some(model),
            MaybeSet::Null => cfg.default_model = None,
            MaybeSet::Unset => {}
        }

        match temperature_update {
            MaybeSet::Set(temperature) => {
                if !(0.0..=2.0).contains(&temperature) {
                    anyhow::bail!("'temperature' must be between 0.0 and 2.0");
                }
                cfg.default_temperature = temperature;
            }
            MaybeSet::Null => {
                cfg.default_temperature = Config::default().default_temperature;
            }
            MaybeSet::Unset => {}
        }

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Default provider/model settings updated",
                "config": Self::snapshot(&cfg),
            }))?,
            error: None,
        })
    }

    async fn handle_upsert_scenario(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let hint = Self::parse_non_empty_string(args, "hint")?;
        let provider = Self::parse_non_empty_string(args, "provider")?;
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
            provider: provider.clone(),
            model: model.clone(),
            api_key: None,
        });

        next_route.hint = hint.clone();
        next_route.provider = provider;
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
        let provider = Self::parse_non_empty_string(args, "provider")?;
        let model = Self::parse_non_empty_string(args, "model")?;

        let system_prompt_update = Self::parse_optional_string_update(args, "system_prompt")?;
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

        let mut next_agent = cfg
            .agents
            .get(&name)
            .cloned()
            .unwrap_or(DelegateAgentConfig {
                provider: provider.clone(),
                model: model.clone(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: DEFAULT_AGENT_MAX_DEPTH,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: DEFAULT_AGENT_MAX_ITERATIONS,
            });

        next_agent.provider = provider;
        next_agent.model = model;

        match system_prompt_update {
            MaybeSet::Set(value) => next_agent.system_prompt = Some(value),
            MaybeSet::Null => next_agent.system_prompt = None,
            MaybeSet::Unset => {}
        }

        match api_key_update {
            MaybeSet::Set(value) => next_agent.api_key = Some(value),
            MaybeSet::Null => next_agent.api_key = None,
            MaybeSet::Unset => {}
        }

        match temperature_update {
            MaybeSet::Set(value) => {
                if !(0.0..=2.0).contains(&value) {
                    anyhow::bail!("'temperature' must be between 0.0 and 2.0");
                }
                next_agent.temperature = Some(value);
            }
            MaybeSet::Null => next_agent.temperature = None,
            MaybeSet::Unset => {}
        }

        match max_depth_update {
            MaybeSet::Set(value) => next_agent.max_depth = value,
            MaybeSet::Null => next_agent.max_depth = DEFAULT_AGENT_MAX_DEPTH,
            MaybeSet::Unset => {}
        }

        match max_iterations_update {
            MaybeSet::Set(value) => next_agent.max_iterations = value,
            MaybeSet::Null => next_agent.max_iterations = DEFAULT_AGENT_MAX_ITERATIONS,
            MaybeSet::Unset => {}
        }

        if let Some(agentic) = agentic_update {
            next_agent.agentic = agentic;
        }

        if let Some(allowed_tools) = allowed_tools_update {
            next_agent.allowed_tools = allowed_tools;
        }

        if next_agent.max_depth == 0 {
            anyhow::bail!("'max_depth' must be greater than 0");
        }

        if next_agent.max_iterations == 0 {
            anyhow::bail!("'max_iterations' must be greater than 0");
        }

        if next_agent.agentic && next_agent.allowed_tools.is_empty() {
            anyhow::bail!(
                "Agent '{name}' has agentic=true but allowed_tools is empty. Set allowed_tools or disable agentic mode."
            );
        }

        cfg.agents.insert(name.clone(), next_agent);
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
            anyhow::bail!("No delegate agent found with name '{name}'");
        }

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Delegate agent removed",
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
        "Manage default model settings, scenario-based provider/model routes, classification rules, and delegate sub-agent profiles"
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
                "provider": {
                    "type": "string",
                    "description": "Provider for set_default/upsert_scenario/upsert_agent"
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
                    "description": "Optional API key override for scenario route or delegate agent"
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
                    "description": "Delegate sub-agent name for upsert_agent/remove_agent"
                },
                "system_prompt": {
                    "type": ["string", "null"],
                    "description": "Optional system prompt override for delegate agent"
                },
                "max_depth": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Delegate max recursion depth"
                },
                "agentic": {
                    "type": "boolean",
                    "description": "Enable tool-call loop mode for delegate agent"
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
            "set_default"
            | "upsert_scenario"
            | "remove_scenario"
            | "upsert_agent"
            | "remove_agent" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }

                match action.as_str() {
                    "set_default" => self.handle_set_default(&args).await,
                    "upsert_scenario" => self.handle_upsert_scenario(&args).await,
                    "remove_scenario" => self.handle_remove_scenario(&args).await,
                    "upsert_agent" => self.handle_upsert_agent(&args).await,
                    "remove_agent" => self.handle_remove_agent(&args).await,
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
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

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
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.save().await.unwrap();
        Arc::new(config)
    }

    #[tokio::test]
    async fn set_default_updates_provider_model_and_temperature() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let result = tool
            .execute(json!({
                "action": "set_default",
                "provider": "kimi",
                "model": "moonshot-v1-8k",
                "temperature": 0.2
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let output: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(
            output["config"]["default"]["provider"].as_str(),
            Some("kimi")
        );
        assert_eq!(
            output["config"]["default"]["model"].as_str(),
            Some("moonshot-v1-8k")
        );
        assert_eq!(
            output["config"]["default"]["temperature"].as_f64(),
            Some(0.2)
        );
    }

    #[tokio::test]
    async fn upsert_scenario_creates_route_and_rule() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let result = tool
            .execute(json!({
                "action": "upsert_scenario",
                "hint": "coding",
                "provider": "openai",
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
                && item["provider"] == json!("openai")
                && item["model"] == json!("gpt-5.3-codex")
        }));
    }

    #[tokio::test]
    async fn remove_scenario_also_removes_rule() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let _ = tool
            .execute(json!({
                "action": "upsert_scenario",
                "hint": "coding",
                "provider": "openai",
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
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let upsert = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "coder",
                "provider": "openai",
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
        assert_eq!(output["agents"]["coder"]["provider"], json!("openai"));
        assert_eq!(output["agents"]["coder"]["model"], json!("gpt-5.3-codex"));
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
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, readonly_security());

        let result = tool
            .execute(json!({
                "action": "set_default",
                "provider": "openai"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("read-only"));
    }
}
