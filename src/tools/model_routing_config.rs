use super::traits::{Tool, ToolResult};
use crate::config::{
    AgentLoadBalanceStrategy, AgentTeamsConfig, ClassificationRule, Config, DelegateAgentConfig,
    ModelRouteConfig, SubAgentsConfig,
};
use crate::providers::has_provider_credential;
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

    fn normalize_transport_value(raw: &str, field: &str) -> anyhow::Result<String> {
        let normalized = raw.trim().to_ascii_lowercase().replace(['-', '_'], "");
        match normalized.as_str() {
            "auto" => Ok("auto".to_string()),
            "websocket" | "ws" => Ok("websocket".to_string()),
            "sse" | "http" => Ok("sse".to_string()),
            _ => anyhow::bail!("'{field}' must be one of: auto, websocket, sse"),
        }
    }

    fn parse_optional_transport_update(
        args: &Value,
        field: &str,
    ) -> anyhow::Result<MaybeSet<String>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let value = raw
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a string or null"))?
            .trim();

        if value.is_empty() {
            return Ok(MaybeSet::Null);
        }

        Ok(MaybeSet::Set(Self::normalize_transport_value(
            value, field,
        )?))
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

    fn parse_load_strategy(raw: &str, field: &str) -> anyhow::Result<AgentLoadBalanceStrategy> {
        let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "semantic" | "score_first" | "scored" => Ok(AgentLoadBalanceStrategy::Semantic),
            "adaptive" | "balanced" | "load_adaptive" => Ok(AgentLoadBalanceStrategy::Adaptive),
            "least_loaded" | "leastload" | "least_load" => {
                Ok(AgentLoadBalanceStrategy::LeastLoaded)
            }
            _ => anyhow::bail!("'{field}' must be one of: semantic, adaptive, least_loaded"),
        }
    }

    fn parse_optional_load_strategy_update(
        args: &Value,
        field: &str,
    ) -> anyhow::Result<MaybeSet<AgentLoadBalanceStrategy>> {
        let Some(raw) = args.get(field) else {
            return Ok(MaybeSet::Unset);
        };

        if raw.is_null() {
            return Ok(MaybeSet::Null);
        }

        let value = raw
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a string or null"))?
            .trim();

        if value.is_empty() {
            return Ok(MaybeSet::Null);
        }

        Ok(MaybeSet::Set(Self::parse_load_strategy(value, field)?))
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
            "transport": route.transport,
            "api_key_configured": has_provider_credential(&route.provider, route.api_key.as_deref()),
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
                    "api_key_configured": has_provider_credential(
                        &agent.provider,
                        agent.api_key.as_deref()
                    ),
                    "enabled": agent.enabled,
                    "capabilities": agent.capabilities,
                    "priority": agent.priority,
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
            "agent_orchestration": {
                "teams": {
                    "enabled": cfg.agent.teams.enabled,
                    "auto_activate": cfg.agent.teams.auto_activate,
                    "max_agents": cfg.agent.teams.max_agents,
                    "strategy": cfg.agent.teams.strategy,
                    "load_window_secs": cfg.agent.teams.load_window_secs,
                    "inflight_penalty": cfg.agent.teams.inflight_penalty,
                    "recent_selection_penalty": cfg.agent.teams.recent_selection_penalty,
                    "recent_failure_penalty": cfg.agent.teams.recent_failure_penalty,
                },
                "subagents": {
                    "enabled": cfg.agent.subagents.enabled,
                    "auto_activate": cfg.agent.subagents.auto_activate,
                    "max_concurrent": cfg.agent.subagents.max_concurrent,
                    "strategy": cfg.agent.subagents.strategy,
                    "load_window_secs": cfg.agent.subagents.load_window_secs,
                    "inflight_penalty": cfg.agent.subagents.inflight_penalty,
                    "recent_selection_penalty": cfg.agent.subagents.recent_selection_penalty,
                    "recent_failure_penalty": cfg.agent.subagents.recent_failure_penalty,
                    "queue_wait_ms": cfg.agent.subagents.queue_wait_ms,
                    "queue_poll_ms": cfg.agent.subagents.queue_poll_ms,
                }
            }
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
                    },
                    "orchestration": {
                        "action": "set_orchestration",
                        "teams_enabled": true,
                        "teams_auto_activate": true,
                        "max_team_agents": 12,
                        "teams_strategy": "adaptive",
                        "teams_load_window_secs": 120,
                        "teams_inflight_penalty": 8,
                        "teams_recent_selection_penalty": 2,
                        "teams_recent_failure_penalty": 12,
                        "subagents_enabled": true,
                        "subagents_auto_activate": true,
                        "max_concurrent_subagents": 4,
                        "subagents_strategy": "adaptive",
                        "subagents_load_window_secs": 180,
                        "subagents_inflight_penalty": 10,
                        "subagents_recent_selection_penalty": 3,
                        "subagents_recent_failure_penalty": 16,
                        "subagents_queue_wait_ms": 15000,
                        "subagents_queue_poll_ms": 200
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

    async fn handle_set_orchestration(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let teams_enabled = Self::parse_optional_bool(args, "teams_enabled")?;
        let teams_auto_activate = Self::parse_optional_bool(args, "teams_auto_activate")?;
        let max_team_agents_update = Self::parse_optional_usize_update(args, "max_team_agents")?;
        let teams_strategy_update =
            Self::parse_optional_load_strategy_update(args, "teams_strategy")?;
        let teams_load_window_secs_update =
            Self::parse_optional_usize_update(args, "teams_load_window_secs")?;
        let teams_inflight_penalty_update =
            Self::parse_optional_usize_update(args, "teams_inflight_penalty")?;
        let teams_recent_selection_penalty_update =
            Self::parse_optional_usize_update(args, "teams_recent_selection_penalty")?;
        let teams_recent_failure_penalty_update =
            Self::parse_optional_usize_update(args, "teams_recent_failure_penalty")?;

        let subagents_enabled = Self::parse_optional_bool(args, "subagents_enabled")?;
        let subagents_auto_activate = Self::parse_optional_bool(args, "subagents_auto_activate")?;
        let max_concurrent_subagents_update =
            Self::parse_optional_usize_update(args, "max_concurrent_subagents")?;
        let subagents_strategy_update =
            Self::parse_optional_load_strategy_update(args, "subagents_strategy")?;
        let subagents_load_window_secs_update =
            Self::parse_optional_usize_update(args, "subagents_load_window_secs")?;
        let subagents_inflight_penalty_update =
            Self::parse_optional_usize_update(args, "subagents_inflight_penalty")?;
        let subagents_recent_selection_penalty_update =
            Self::parse_optional_usize_update(args, "subagents_recent_selection_penalty")?;
        let subagents_recent_failure_penalty_update =
            Self::parse_optional_usize_update(args, "subagents_recent_failure_penalty")?;
        let subagents_queue_wait_ms_update =
            Self::parse_optional_usize_update(args, "subagents_queue_wait_ms")?;
        let subagents_queue_poll_ms_update =
            Self::parse_optional_usize_update(args, "subagents_queue_poll_ms")?;

        let any_update = teams_enabled.is_some()
            || teams_auto_activate.is_some()
            || subagents_enabled.is_some()
            || subagents_auto_activate.is_some()
            || !matches!(max_team_agents_update, MaybeSet::Unset)
            || !matches!(teams_strategy_update, MaybeSet::Unset)
            || !matches!(teams_load_window_secs_update, MaybeSet::Unset)
            || !matches!(teams_inflight_penalty_update, MaybeSet::Unset)
            || !matches!(teams_recent_selection_penalty_update, MaybeSet::Unset)
            || !matches!(teams_recent_failure_penalty_update, MaybeSet::Unset)
            || !matches!(max_concurrent_subagents_update, MaybeSet::Unset)
            || !matches!(subagents_strategy_update, MaybeSet::Unset)
            || !matches!(subagents_load_window_secs_update, MaybeSet::Unset)
            || !matches!(subagents_inflight_penalty_update, MaybeSet::Unset)
            || !matches!(subagents_recent_selection_penalty_update, MaybeSet::Unset)
            || !matches!(subagents_recent_failure_penalty_update, MaybeSet::Unset)
            || !matches!(subagents_queue_wait_ms_update, MaybeSet::Unset)
            || !matches!(subagents_queue_poll_ms_update, MaybeSet::Unset);
        if !any_update {
            anyhow::bail!(
                "set_orchestration requires at least one field: \
                 teams_enabled, teams_auto_activate, max_team_agents, \
                 teams_strategy, teams_load_window_secs, teams_inflight_penalty, \
                 teams_recent_selection_penalty, teams_recent_failure_penalty, \
                 subagents_enabled, subagents_auto_activate, max_concurrent_subagents, \
                 subagents_strategy, subagents_load_window_secs, subagents_inflight_penalty, \
                 subagents_recent_selection_penalty, subagents_recent_failure_penalty, \
                 subagents_queue_wait_ms, subagents_queue_poll_ms"
            );
        }

        let mut cfg = self.load_config_without_env()?;
        let team_defaults = AgentTeamsConfig::default();
        let subagent_defaults = SubAgentsConfig::default();

        if let Some(value) = teams_enabled {
            cfg.agent.teams.enabled = value;
        }
        if let Some(value) = teams_auto_activate {
            cfg.agent.teams.auto_activate = value;
        }
        match max_team_agents_update {
            MaybeSet::Set(value) => cfg.agent.teams.max_agents = value,
            MaybeSet::Null => cfg.agent.teams.max_agents = team_defaults.max_agents,
            MaybeSet::Unset => {}
        }
        match teams_strategy_update {
            MaybeSet::Set(value) => cfg.agent.teams.strategy = value,
            MaybeSet::Null => cfg.agent.teams.strategy = team_defaults.strategy,
            MaybeSet::Unset => {}
        }
        match teams_load_window_secs_update {
            MaybeSet::Set(value) => cfg.agent.teams.load_window_secs = value,
            MaybeSet::Null => cfg.agent.teams.load_window_secs = team_defaults.load_window_secs,
            MaybeSet::Unset => {}
        }
        match teams_inflight_penalty_update {
            MaybeSet::Set(value) => cfg.agent.teams.inflight_penalty = value,
            MaybeSet::Null => cfg.agent.teams.inflight_penalty = team_defaults.inflight_penalty,
            MaybeSet::Unset => {}
        }
        match teams_recent_selection_penalty_update {
            MaybeSet::Set(value) => cfg.agent.teams.recent_selection_penalty = value,
            MaybeSet::Null => {
                cfg.agent.teams.recent_selection_penalty = team_defaults.recent_selection_penalty
            }
            MaybeSet::Unset => {}
        }
        match teams_recent_failure_penalty_update {
            MaybeSet::Set(value) => cfg.agent.teams.recent_failure_penalty = value,
            MaybeSet::Null => {
                cfg.agent.teams.recent_failure_penalty = team_defaults.recent_failure_penalty
            }
            MaybeSet::Unset => {}
        }

        if let Some(value) = subagents_enabled {
            cfg.agent.subagents.enabled = value;
        }
        if let Some(value) = subagents_auto_activate {
            cfg.agent.subagents.auto_activate = value;
        }
        match max_concurrent_subagents_update {
            MaybeSet::Set(value) => cfg.agent.subagents.max_concurrent = value,
            MaybeSet::Null => cfg.agent.subagents.max_concurrent = subagent_defaults.max_concurrent,
            MaybeSet::Unset => {}
        }
        match subagents_strategy_update {
            MaybeSet::Set(value) => cfg.agent.subagents.strategy = value,
            MaybeSet::Null => cfg.agent.subagents.strategy = subagent_defaults.strategy,
            MaybeSet::Unset => {}
        }
        match subagents_load_window_secs_update {
            MaybeSet::Set(value) => cfg.agent.subagents.load_window_secs = value,
            MaybeSet::Null => {
                cfg.agent.subagents.load_window_secs = subagent_defaults.load_window_secs
            }
            MaybeSet::Unset => {}
        }
        match subagents_inflight_penalty_update {
            MaybeSet::Set(value) => cfg.agent.subagents.inflight_penalty = value,
            MaybeSet::Null => {
                cfg.agent.subagents.inflight_penalty = subagent_defaults.inflight_penalty
            }
            MaybeSet::Unset => {}
        }
        match subagents_recent_selection_penalty_update {
            MaybeSet::Set(value) => cfg.agent.subagents.recent_selection_penalty = value,
            MaybeSet::Null => {
                cfg.agent.subagents.recent_selection_penalty =
                    subagent_defaults.recent_selection_penalty
            }
            MaybeSet::Unset => {}
        }
        match subagents_recent_failure_penalty_update {
            MaybeSet::Set(value) => cfg.agent.subagents.recent_failure_penalty = value,
            MaybeSet::Null => {
                cfg.agent.subagents.recent_failure_penalty =
                    subagent_defaults.recent_failure_penalty
            }
            MaybeSet::Unset => {}
        }
        match subagents_queue_wait_ms_update {
            MaybeSet::Set(value) => cfg.agent.subagents.queue_wait_ms = value,
            MaybeSet::Null => cfg.agent.subagents.queue_wait_ms = subagent_defaults.queue_wait_ms,
            MaybeSet::Unset => {}
        }
        match subagents_queue_poll_ms_update {
            MaybeSet::Set(value) => cfg.agent.subagents.queue_poll_ms = value,
            MaybeSet::Null => cfg.agent.subagents.queue_poll_ms = subagent_defaults.queue_poll_ms,
            MaybeSet::Unset => {}
        }

        if cfg.agent.teams.max_agents == 0 {
            anyhow::bail!("'max_team_agents' must be greater than 0");
        }
        if cfg.agent.teams.load_window_secs == 0 {
            anyhow::bail!("'teams_load_window_secs' must be greater than 0");
        }
        if cfg.agent.subagents.max_concurrent == 0 {
            anyhow::bail!("'max_concurrent_subagents' must be greater than 0");
        }
        if cfg.agent.subagents.load_window_secs == 0 {
            anyhow::bail!("'subagents_load_window_secs' must be greater than 0");
        }
        if cfg.agent.subagents.queue_poll_ms == 0 {
            anyhow::bail!("'subagents_queue_poll_ms' must be greater than 0");
        }

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "Agent orchestration settings updated",
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
        let transport_update = Self::parse_optional_transport_update(args, "transport")?;

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
            max_tokens: None,
            api_key: None,
            transport: None,
        });

        next_route.hint = hint.clone();
        next_route.provider = provider;
        next_route.model = model;

        match api_key_update {
            MaybeSet::Set(api_key) => next_route.api_key = Some(api_key),
            MaybeSet::Null => next_route.api_key = None,
            MaybeSet::Unset => {}
        }

        match transport_update {
            MaybeSet::Set(transport) => next_route.transport = Some(transport),
            MaybeSet::Null => next_route.transport = None,
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
        let enabled_update = Self::parse_optional_bool(args, "enabled")?;
        let priority_update = Self::parse_optional_i32_update(args, "priority")?;

        let allowed_tools_update = if let Some(raw) = args.get("allowed_tools") {
            Some(Self::parse_string_list(raw, "allowed_tools")?)
        } else {
            None
        };
        let capabilities_update = if let Some(raw) = args.get("capabilities") {
            Some(Self::parse_string_list(raw, "capabilities")?)
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
                enabled: true,
                capabilities: Vec::new(),
                priority: 0,
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

        if let Some(enabled) = enabled_update {
            next_agent.enabled = enabled;
        }

        if let Some(capabilities) = capabilities_update {
            next_agent.capabilities = capabilities;
        }

        match priority_update {
            MaybeSet::Set(value) => next_agent.priority = value,
            MaybeSet::Null => next_agent.priority = 0,
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
        "Manage default model settings, scenario routes, classification rules, delegate profiles, and agent team/subagent orchestration switches"
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
                        "set_orchestration",
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
                "transport": {
                    "type": ["string", "null"],
                    "enum": ["auto", "websocket", "sse", "ws", "http", null],
                    "description": "Optional route transport override for upsert_scenario (auto, websocket, sse)"
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
                    "description": "Priority value. For scenarios: classifier order (higher runs first). For upsert_agent: delegate selection priority."
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
                "enabled": {
                    "type": "boolean",
                    "description": "Enable or disable a delegate profile for selection/invocation"
                },
                "capabilities": {
                    "description": "Capability tags for automatic agent selection (string or string array)",
                    "oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
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
                },
                "teams_enabled": {
                    "type": "boolean",
                    "description": "Enable/disable synchronous agent-team delegation tools"
                },
                "teams_auto_activate": {
                    "type": "boolean",
                    "description": "Enable/disable automatic team-agent selection when agent is omitted or 'auto'"
                },
                "max_team_agents": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Maximum number of delegate profiles activated for teams"
                },
                "teams_strategy": {
                    "type": ["string", "null"],
                    "enum": ["semantic", "adaptive", "least_loaded", null],
                    "description": "Team auto-selection strategy"
                },
                "teams_load_window_secs": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Recent-event window for team load balancing (seconds)"
                },
                "teams_inflight_penalty": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Team score penalty per in-flight task"
                },
                "teams_recent_selection_penalty": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Team score penalty per recent assignment in the load window"
                },
                "teams_recent_failure_penalty": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Team score penalty per recent failure in the load window"
                },
                "subagents_enabled": {
                    "type": "boolean",
                    "description": "Enable/disable background sub-agent tools"
                },
                "subagents_auto_activate": {
                    "type": "boolean",
                    "description": "Enable/disable automatic sub-agent selection when agent is omitted or 'auto'"
                },
                "max_concurrent_subagents": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Maximum number of concurrently running background sub-agents"
                },
                "subagents_strategy": {
                    "type": ["string", "null"],
                    "enum": ["semantic", "adaptive", "least_loaded", null],
                    "description": "Sub-agent auto-selection strategy"
                },
                "subagents_load_window_secs": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Recent-event window for sub-agent load balancing (seconds)"
                },
                "subagents_inflight_penalty": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Sub-agent score penalty per in-flight task"
                },
                "subagents_recent_selection_penalty": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Sub-agent score penalty per recent assignment in the load window"
                },
                "subagents_recent_failure_penalty": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "Sub-agent score penalty per recent failure in the load window"
                },
                "subagents_queue_wait_ms": {
                    "type": ["integer", "null"],
                    "minimum": 0,
                    "description": "How long to wait for sub-agent capacity before failing (milliseconds)"
                },
                "subagents_queue_poll_ms": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Poll interval while waiting for sub-agent capacity (milliseconds)"
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
            | "set_orchestration"
            | "upsert_scenario"
            | "remove_scenario"
            | "upsert_agent"
            | "remove_agent" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }

                match action.as_str() {
                    "set_default" => self.handle_set_default(&args).await,
                    "set_orchestration" => self.handle_set_orchestration(&args).await,
                    "upsert_scenario" => self.handle_upsert_scenario(&args).await,
                    "remove_scenario" => self.handle_remove_scenario(&args).await,
                    "upsert_agent" => self.handle_upsert_agent(&args).await,
                    "remove_agent" => self.handle_remove_agent(&args).await,
                    _ => unreachable!("validated above"),
                }
            }
            _ => anyhow::bail!(
                "Unknown action '{action}'. Valid: get, list_hints, set_default, set_orchestration, upsert_scenario, remove_scenario, upsert_agent, remove_agent"
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
    use std::sync::OnceLock;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

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

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                Some(next) => std::env::set_var(key, next),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(original) = self.original.as_deref() {
                std::env::set_var(self.key, original);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    async fn env_lock() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().await
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
                "transport": "websocket",
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
                && item["transport"] == json!("websocket")
        }));
    }

    #[tokio::test]
    async fn upsert_scenario_transport_alias_is_canonicalized() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let result = tool
            .execute(json!({
                "action": "upsert_scenario",
                "hint": "analysis",
                "provider": "openai",
                "model": "gpt-5.3-codex",
                "transport": "WS"
            }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        assert!(get_result.success);
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        let scenarios = output["scenarios"].as_array().unwrap();
        assert!(scenarios.iter().any(|item| {
            item["hint"] == json!("analysis") && item["transport"] == json!("websocket")
        }));
    }

    #[tokio::test]
    async fn upsert_scenario_rejects_invalid_transport() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let result = tool
            .execute(json!({
                "action": "upsert_scenario",
                "hint": "analysis",
                "provider": "openai",
                "model": "gpt-5.3-codex",
                "transport": "udp"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("'transport' must be one of: auto, websocket, sse"));
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
    async fn upsert_agent_persists_selection_metadata() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let upsert = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "planner",
                "provider": "openai",
                "model": "gpt-5.3-codex",
                "enabled": false,
                "capabilities": ["planning", "analysis"],
                "priority": 7
            }))
            .await
            .unwrap();
        assert!(upsert.success, "{:?}", upsert.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(output["agents"]["planner"]["enabled"], json!(false));
        assert_eq!(
            output["agents"]["planner"]["capabilities"],
            json!(["planning", "analysis"])
        );
        assert_eq!(output["agents"]["planner"]["priority"], json!(7));

        let reset = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "planner",
                "provider": "openai",
                "model": "gpt-5.3-codex",
                "priority": null
            }))
            .await
            .unwrap();
        assert!(reset.success, "{:?}", reset.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(output["agents"]["planner"]["priority"], json!(0));
    }

    #[tokio::test]
    async fn set_orchestration_updates_team_and_subagent_controls() {
        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let updated = tool
            .execute(json!({
                "action": "set_orchestration",
                "teams_enabled": false,
                "teams_auto_activate": false,
                "max_team_agents": 5,
                "teams_strategy": "least_loaded",
                "teams_load_window_secs": 90,
                "teams_inflight_penalty": 6,
                "teams_recent_selection_penalty": 2,
                "teams_recent_failure_penalty": 14,
                "subagents_enabled": true,
                "subagents_auto_activate": false,
                "max_concurrent_subagents": 3,
                "subagents_strategy": "semantic",
                "subagents_load_window_secs": 60,
                "subagents_inflight_penalty": 4,
                "subagents_recent_selection_penalty": 1,
                "subagents_recent_failure_penalty": 9,
                "subagents_queue_wait_ms": 500,
                "subagents_queue_poll_ms": 20
            }))
            .await
            .unwrap();
        assert!(updated.success, "{:?}", updated.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        assert!(get_result.success);
        let output: Value = serde_json::from_str(&get_result.output).unwrap();

        assert_eq!(
            output["agent_orchestration"]["teams"]["enabled"],
            json!(false)
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["auto_activate"],
            json!(false)
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["max_agents"],
            json!(5)
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["strategy"],
            json!("least_loaded")
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["load_window_secs"],
            json!(90)
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["inflight_penalty"],
            json!(6)
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["recent_selection_penalty"],
            json!(2)
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["recent_failure_penalty"],
            json!(14)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["enabled"],
            json!(true)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["auto_activate"],
            json!(false)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["max_concurrent"],
            json!(3)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["strategy"],
            json!("semantic")
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["load_window_secs"],
            json!(60)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["inflight_penalty"],
            json!(4)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["recent_selection_penalty"],
            json!(1)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["recent_failure_penalty"],
            json!(9)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["queue_wait_ms"],
            json!(500)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["queue_poll_ms"],
            json!(20)
        );

        let reset = tool
            .execute(json!({
                "action": "set_orchestration",
                "max_team_agents": null,
                "teams_strategy": null,
                "teams_load_window_secs": null,
                "teams_inflight_penalty": null,
                "teams_recent_selection_penalty": null,
                "teams_recent_failure_penalty": null,
                "max_concurrent_subagents": null,
                "subagents_strategy": null,
                "subagents_load_window_secs": null,
                "subagents_inflight_penalty": null,
                "subagents_recent_selection_penalty": null,
                "subagents_recent_failure_penalty": null,
                "subagents_queue_wait_ms": null,
                "subagents_queue_poll_ms": null
            }))
            .await
            .unwrap();
        assert!(reset.success, "{:?}", reset.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(
            output["agent_orchestration"]["teams"]["max_agents"],
            json!(32)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["max_concurrent"],
            json!(10)
        );
        assert_eq!(
            output["agent_orchestration"]["teams"]["strategy"],
            json!("adaptive")
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["strategy"],
            json!("adaptive")
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["queue_wait_ms"],
            json!(15000)
        );
        assert_eq!(
            output["agent_orchestration"]["subagents"]["queue_poll_ms"],
            json!(200)
        );
    }

    #[tokio::test]
    async fn set_orchestration_rejects_invalid_strategy() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp).await;
        let tool = ModelRoutingConfigTool::new(config, test_security());

        let result = tool
            .execute(json!({
                "action": "set_orchestration",
                "teams_strategy": "randomized"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("teams_strategy"));
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

    #[tokio::test]
    async fn get_reports_env_backed_credentials_for_routes_and_agents() {
        let _env_lock = env_lock().await;
        let _provider_guard = EnvGuard::set("TELNYX_API_KEY", Some("test-telnyx-key"));
        let _generic_guard = EnvGuard::set("ZEROCLAW_API_KEY", None);
        let _api_key_guard = EnvGuard::set("API_KEY", None);

        let tmp = TempDir::new().unwrap();
        let tool = ModelRoutingConfigTool::new(test_config(&tmp).await, test_security());

        let upsert_route = tool
            .execute(json!({
                "action": "upsert_scenario",
                "hint": "voice",
                "provider": "telnyx",
                "model": "telnyx-conversation"
            }))
            .await
            .unwrap();
        assert!(upsert_route.success, "{:?}", upsert_route.error);

        let upsert_agent = tool
            .execute(json!({
                "action": "upsert_agent",
                "name": "voice_helper",
                "provider": "telnyx",
                "model": "telnyx-conversation"
            }))
            .await
            .unwrap();
        assert!(upsert_agent.success, "{:?}", upsert_agent.error);

        let get_result = tool.execute(json!({"action": "get"})).await.unwrap();
        assert!(get_result.success);
        let output: Value = serde_json::from_str(&get_result.output).unwrap();

        let route = output["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["hint"] == json!("voice"))
            .unwrap();
        assert_eq!(route["api_key_configured"], json!(true));

        assert_eq!(
            output["agents"]["voice_helper"]["api_key_configured"],
            json!(true)
        );
    }
}
