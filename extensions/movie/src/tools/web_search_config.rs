use super::traits::{Tool, ToolResult};
use crate::config::{Config, WebSearchConfig};
use crate::security::SecurityPolicy;
use crate::util::MaybeSet;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

pub struct WebSearchConfigTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl WebSearchConfigTool {
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

    fn supported_providers() -> &'static [&'static str] {
        &[
            "duckduckgo",
            "brave",
            "firecrawl",
            "tavily",
            "perplexity",
            "exa",
            "jina",
        ]
    }

    fn normalize_provider(raw: &str) -> Option<&'static str> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Some("duckduckgo"),
            "brave" => Some("brave"),
            "firecrawl" => Some("firecrawl"),
            "tavily" => Some("tavily"),
            "perplexity" => Some("perplexity"),
            "exa" => Some("exa"),
            "jina" => Some("jina"),
            _ => None,
        }
    }

    fn normalize_provider_list(raw: Vec<String>, field: &str) -> anyhow::Result<Vec<String>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for entry in raw {
            let provider = Self::normalize_provider(&entry).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid provider '{entry}' in {field}. Supported providers: {}",
                    Self::supported_providers().join(", ")
                )
            })?;
            if seen.insert(provider) {
                out.push(provider.to_string());
            }
        }
        Ok(out)
    }

    fn snapshot(cfg: &WebSearchConfig) -> Value {
        json!({
            "enabled": cfg.enabled,
            "provider": cfg.provider,
            "fallback_providers": cfg.fallback_providers,
            "max_results": cfg.max_results,
            "timeout_secs": cfg.timeout_secs,
            "retries_per_provider": cfg.retries_per_provider,
            "retry_backoff_ms": cfg.retry_backoff_ms,
            "domain_filter": cfg.domain_filter,
            "language_filter": cfg.language_filter,
            "country": cfg.country,
            "recency_filter": cfg.recency_filter,
            "max_tokens": cfg.max_tokens,
            "max_tokens_per_page": cfg.max_tokens_per_page,
            "exa_search_type": cfg.exa_search_type,
            "exa_include_text": cfg.exa_include_text,
            "jina_site_filters": cfg.jina_site_filters,
            "api_keys_configured": {
                "api_key": cfg.api_key.as_ref().is_some_and(|v| !v.trim().is_empty()),
                "brave_api_key": cfg.brave_api_key.as_ref().is_some_and(|v| !v.trim().is_empty()),
                "perplexity_api_key": cfg.perplexity_api_key.as_ref().is_some_and(|v| !v.trim().is_empty()),
                "exa_api_key": cfg.exa_api_key.as_ref().is_some_and(|v| !v.trim().is_empty()),
                "jina_api_key": cfg.jina_api_key.as_ref().is_some_and(|v| !v.trim().is_empty())
            }
        })
    }

    fn handle_get(&self) -> anyhow::Result<ToolResult> {
        let cfg = self.load_config_without_env()?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&Self::snapshot(&cfg.web_search))?,
            error: None,
        })
    }

    fn handle_list_providers(&self) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "providers": Self::supported_providers(),
                "aliases": {
                    "ddg": "duckduckgo"
                },
                "examples": {
                    "set_primary_and_fallbacks": {
                        "action": "set",
                        "provider": "perplexity",
                        "fallback_providers": ["exa", "jina", "duckduckgo"]
                    },
                    "set_exa": {
                        "action": "set",
                        "provider": "exa",
                        "exa_search_type": "neural",
                        "exa_include_text": true
                    }
                }
            }))?,
            error: None,
        })
    }

    async fn handle_set(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let mut cfg = self.load_config_without_env()?;

        if let Some(enabled) = args.get("enabled") {
            cfg.web_search.enabled = enabled
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("'enabled' must be a boolean"))?;
        }

        if let Some(provider) = args.get("provider") {
            let provider = provider
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'provider' must be a string"))?;
            let normalized = Self::normalize_provider(provider).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid provider '{}'. Supported providers: {}",
                    provider,
                    Self::supported_providers().join(", ")
                )
            })?;
            cfg.web_search.provider = normalized.to_string();
        }

        if let Some(raw) = args.get("fallback_providers") {
            let list = Self::parse_string_list(raw, "fallback_providers")?;
            cfg.web_search.fallback_providers =
                Self::normalize_provider_list(list, "fallback_providers")?;
        }

        if let Some(raw) = args.get("domain_filter") {
            cfg.web_search.domain_filter = Self::parse_string_list(raw, "domain_filter")?;
        }

        if let Some(raw) = args.get("language_filter") {
            cfg.web_search.language_filter = Self::parse_string_list(raw, "language_filter")?;
        }

        if let Some(raw) = args.get("jina_site_filters") {
            cfg.web_search.jina_site_filters = Self::parse_string_list(raw, "jina_site_filters")?;
        }

        if let Some(max_results) = args.get("max_results") {
            let value = max_results
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("'max_results' must be a non-negative integer"))?;
            let value = usize::try_from(value)
                .map_err(|_| anyhow::anyhow!("'max_results' is too large"))?;
            if !(1..=10).contains(&value) {
                anyhow::bail!("'max_results' must be between 1 and 10")
            }
            cfg.web_search.max_results = value;
        }

        if let Some(timeout_secs) = args.get("timeout_secs") {
            let value = timeout_secs
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("'timeout_secs' must be a non-negative integer"))?;
            if value == 0 {
                anyhow::bail!("'timeout_secs' must be > 0")
            }
            cfg.web_search.timeout_secs = value;
        }

        if let Some(retries) = args.get("retries_per_provider") {
            let value = retries.as_u64().ok_or_else(|| {
                anyhow::anyhow!("'retries_per_provider' must be a non-negative integer")
            })?;
            let value = u32::try_from(value)
                .map_err(|_| anyhow::anyhow!("'retries_per_provider' must fit in u32"))?;
            if value > 5 {
                anyhow::bail!("'retries_per_provider' must be <= 5")
            }
            cfg.web_search.retries_per_provider = value;
        }

        if let Some(backoff) = args.get("retry_backoff_ms") {
            let value = backoff.as_u64().ok_or_else(|| {
                anyhow::anyhow!("'retry_backoff_ms' must be a non-negative integer")
            })?;
            if value == 0 {
                anyhow::bail!("'retry_backoff_ms' must be > 0")
            }
            cfg.web_search.retry_backoff_ms = value;
        }

        if let Some(raw) = args.get("exa_include_text") {
            cfg.web_search.exa_include_text = raw
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("'exa_include_text' must be a boolean"))?;
        }

        if let Some(search_type) = args.get("exa_search_type") {
            let value = search_type
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("'exa_search_type' must be a string"))?
                .trim()
                .to_ascii_lowercase();
            if !matches!(value.as_str(), "auto" | "keyword" | "neural") {
                anyhow::bail!("'exa_search_type' must be one of: auto, keyword, neural")
            }
            cfg.web_search.exa_search_type = value;
        }

        match Self::parse_optional_string_update(args, "country")? {
            MaybeSet::Set(value) => cfg.web_search.country = Some(value),
            MaybeSet::Null => cfg.web_search.country = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_string_update(args, "recency_filter")? {
            MaybeSet::Set(value) => cfg.web_search.recency_filter = Some(value),
            MaybeSet::Null => cfg.web_search.recency_filter = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_u32_update(args, "max_tokens")? {
            MaybeSet::Set(value) => cfg.web_search.max_tokens = Some(value),
            MaybeSet::Null => cfg.web_search.max_tokens = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_u32_update(args, "max_tokens_per_page")? {
            MaybeSet::Set(value) => cfg.web_search.max_tokens_per_page = Some(value),
            MaybeSet::Null => cfg.web_search.max_tokens_per_page = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_string_update(args, "api_key")? {
            MaybeSet::Set(value) => cfg.web_search.api_key = Some(value),
            MaybeSet::Null => cfg.web_search.api_key = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_string_update(args, "brave_api_key")? {
            MaybeSet::Set(value) => cfg.web_search.brave_api_key = Some(value),
            MaybeSet::Null => cfg.web_search.brave_api_key = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_string_update(args, "perplexity_api_key")? {
            MaybeSet::Set(value) => cfg.web_search.perplexity_api_key = Some(value),
            MaybeSet::Null => cfg.web_search.perplexity_api_key = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_string_update(args, "exa_api_key")? {
            MaybeSet::Set(value) => cfg.web_search.exa_api_key = Some(value),
            MaybeSet::Null => cfg.web_search.exa_api_key = None,
            MaybeSet::Unset => {}
        }

        match Self::parse_optional_string_update(args, "jina_api_key")? {
            MaybeSet::Set(value) => cfg.web_search.jina_api_key = Some(value),
            MaybeSet::Null => cfg.web_search.jina_api_key = None,
            MaybeSet::Unset => {}
        }

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "web_search configuration updated",
                "web_search": Self::snapshot(&cfg.web_search)
            }))?,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for WebSearchConfigTool {
    fn name(&self) -> &str {
        "web_search_config"
    }

    fn description(&self) -> &str {
        "Inspect and update [web_search] configuration at runtime (providers, fallbacks, retries, provider-specific keys/options)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "list_providers"],
                    "description": "Operation to perform"
                },
                "enabled": {"type": "boolean"},
                "provider": {"type": "string"},
                "fallback_providers": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "api_key": {"type": ["string", "null"]},
                "brave_api_key": {"type": ["string", "null"]},
                "perplexity_api_key": {"type": ["string", "null"]},
                "exa_api_key": {"type": ["string", "null"]},
                "jina_api_key": {"type": ["string", "null"]},
                "max_results": {"type": "integer", "minimum": 1, "maximum": 10},
                "timeout_secs": {"type": "integer", "minimum": 1},
                "retries_per_provider": {"type": "integer", "minimum": 0, "maximum": 5},
                "retry_backoff_ms": {"type": "integer", "minimum": 1},
                "domain_filter": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "language_filter": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "country": {"type": ["string", "null"]},
                "recency_filter": {"type": ["string", "null"]},
                "max_tokens": {"type": ["integer", "null"], "minimum": 0},
                "max_tokens_per_page": {"type": ["integer", "null"], "minimum": 0},
                "exa_search_type": {"type": "string", "enum": ["auto", "keyword", "neural"]},
                "exa_include_text": {"type": "boolean"},
                "jina_site_filters": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: action"))?;

        match action {
            "get" => self.handle_get(),
            "list_providers" => self.handle_list_providers(),
            "set" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }
                self.handle_set(&args).await
            }
            other => anyhow::bail!("Unsupported action '{other}'. Use get|set|list_providers"),
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
    async fn list_providers_includes_extended_providers() {
        let tmp = TempDir::new().unwrap();
        let tool = WebSearchConfigTool::new(test_config(&tmp).await, test_security());

        let result = tool
            .execute(json!({
                "action": "list_providers"
            }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        let output: Value = serde_json::from_str(&result.output).unwrap();
        let providers = output["providers"].as_array().unwrap();
        let values: Vec<&str> = providers.iter().filter_map(Value::as_str).collect();
        assert!(values.contains(&"perplexity"));
        assert!(values.contains(&"exa"));
        assert!(values.contains(&"jina"));
    }

    #[tokio::test]
    async fn set_normalizes_provider_and_deduplicates_fallbacks() {
        let tmp = TempDir::new().unwrap();
        let tool = WebSearchConfigTool::new(test_config(&tmp).await, test_security());

        let result = tool
            .execute(json!({
                "action": "set",
                "provider": "DDG",
                "fallback_providers": ["EXA", "jina", "exa", "perplexity", "ddg"],
                "exa_search_type": "neural",
                "exa_include_text": true,
                "jina_site_filters": "docs.rs,github.com"
            }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        let output: Value = serde_json::from_str(&result.output).unwrap();
        let web_search = &output["web_search"];
        assert_eq!(web_search["provider"], json!("duckduckgo"));
        assert_eq!(
            web_search["fallback_providers"],
            json!(["exa", "jina", "perplexity", "duckduckgo"])
        );
        assert_eq!(web_search["exa_search_type"], json!("neural"));
        assert_eq!(web_search["exa_include_text"], json!(true));
        assert_eq!(
            web_search["jina_site_filters"],
            json!(["docs.rs", "github.com"])
        );
    }

    #[tokio::test]
    async fn set_rejects_unknown_provider() {
        let tmp = TempDir::new().unwrap();
        let tool = WebSearchConfigTool::new(test_config(&tmp).await, test_security());

        let err = tool
            .execute(json!({
                "action": "set",
                "provider": "unknown_provider"
            }))
            .await
            .expect_err("unknown provider should fail");
        assert!(err.to_string().contains("Invalid provider"));
    }
}
