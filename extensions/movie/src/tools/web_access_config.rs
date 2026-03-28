use super::traits::{Tool, ToolResult};
use super::url_validation::{
    normalize_allowed_domains, validate_url, DomainPolicy, UrlSchemePolicy,
};
use crate::config::{Config, UrlAccessConfig};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::sync::Arc;

pub struct WebAccessConfigTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl WebAccessConfigTool {
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

    fn parse_optional_bool(args: &Value, field: &str) -> anyhow::Result<Option<bool>> {
        let Some(raw) = args.get(field) else {
            return Ok(None);
        };

        let value = raw
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a boolean"))?;
        Ok(Some(value))
    }

    fn normalize_cidrs(values: Vec<String>) -> Vec<String> {
        let mut normalized = values
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        normalized.sort_unstable();
        normalized.dedup();
        normalized
    }

    fn merge_domains(base: &mut Vec<String>, additions: Vec<String>) {
        let mut merged = std::mem::take(base);
        merged.extend(additions);
        *base = normalize_allowed_domains(merged);
    }

    fn remove_domains(base: &mut Vec<String>, removals: Vec<String>) {
        let removal_set: HashSet<String> =
            normalize_allowed_domains(removals).into_iter().collect();
        base.retain(|entry| !removal_set.contains(entry));
    }

    fn snapshot(cfg: &UrlAccessConfig) -> Value {
        json!({
            "block_private_ip": cfg.block_private_ip,
            "allow_cidrs": cfg.allow_cidrs,
            "allow_domains": cfg.allow_domains,
            "allow_loopback": cfg.allow_loopback,
            "require_first_visit_approval": cfg.require_first_visit_approval,
            "enforce_domain_allowlist": cfg.enforce_domain_allowlist,
            "domain_allowlist": cfg.domain_allowlist,
            "domain_blocklist": cfg.domain_blocklist,
            "approved_domains": cfg.approved_domains,
        })
    }

    fn handle_get(&self) -> anyhow::Result<ToolResult> {
        let cfg = self.load_config_without_env()?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&Self::snapshot(&cfg.security.url_access))?,
            error: None,
        })
    }

    fn handle_check_url(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: url"))?;

        let cfg = self.load_config_without_env()?;
        let wildcard = vec!["*".to_string()];
        let policy = DomainPolicy {
            allowed_domains: &wildcard,
            blocked_domains: &[],
            allowed_field_name: "web_access_config.check_url.allowed_domains",
            blocked_field_name: None,
            empty_allowed_message: "internal error: wildcard allowlist missing",
            scheme_policy: UrlSchemePolicy::HttpOrHttps,
            ipv6_error_context: "web_access_config.check_url",
            url_access: Some(&cfg.security.url_access),
        };

        let result = validate_url(url, &policy);
        match result {
            Ok(valid_url) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "allowed": true,
                    "url": valid_url,
                    "message": "URL passes shared security.url_access policy"
                }))?,
                error: None,
            }),
            Err(error) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "allowed": false,
                    "url": url,
                    "reason": error.to_string()
                }))?,
                error: None,
            }),
        }
    }

    async fn handle_set(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let mut cfg = self.load_config_without_env()?;
        let policy = &mut cfg.security.url_access;

        if let Some(value) = Self::parse_optional_bool(args, "block_private_ip")? {
            policy.block_private_ip = value;
        }
        if let Some(value) = Self::parse_optional_bool(args, "allow_loopback")? {
            policy.allow_loopback = value;
        }
        if let Some(value) = Self::parse_optional_bool(args, "require_first_visit_approval")? {
            policy.require_first_visit_approval = value;
        }
        if let Some(value) = Self::parse_optional_bool(args, "enforce_domain_allowlist")? {
            policy.enforce_domain_allowlist = value;
        }

        if let Some(raw) = args.get("allow_cidrs") {
            policy.allow_cidrs =
                Self::normalize_cidrs(Self::parse_string_list(raw, "allow_cidrs")?);
        }

        if let Some(raw) = args.get("allow_domains") {
            policy.allow_domains =
                normalize_allowed_domains(Self::parse_string_list(raw, "allow_domains")?);
        }

        if let Some(raw) = args.get("domain_allowlist") {
            policy.domain_allowlist =
                normalize_allowed_domains(Self::parse_string_list(raw, "domain_allowlist")?);
        }

        if let Some(raw) = args.get("domain_blocklist") {
            policy.domain_blocklist =
                normalize_allowed_domains(Self::parse_string_list(raw, "domain_blocklist")?);
        }

        if let Some(raw) = args.get("approved_domains") {
            policy.approved_domains =
                normalize_allowed_domains(Self::parse_string_list(raw, "approved_domains")?);
        }

        if let Some(raw) = args.get("add_domain_allowlist") {
            Self::merge_domains(
                &mut policy.domain_allowlist,
                Self::parse_string_list(raw, "add_domain_allowlist")?,
            );
        }

        if let Some(raw) = args.get("remove_domain_allowlist") {
            Self::remove_domains(
                &mut policy.domain_allowlist,
                Self::parse_string_list(raw, "remove_domain_allowlist")?,
            );
        }

        if let Some(raw) = args.get("add_domain_blocklist") {
            Self::merge_domains(
                &mut policy.domain_blocklist,
                Self::parse_string_list(raw, "add_domain_blocklist")?,
            );
        }

        if let Some(raw) = args.get("remove_domain_blocklist") {
            Self::remove_domains(
                &mut policy.domain_blocklist,
                Self::parse_string_list(raw, "remove_domain_blocklist")?,
            );
        }

        if let Some(raw) = args.get("add_approved_domains") {
            Self::merge_domains(
                &mut policy.approved_domains,
                Self::parse_string_list(raw, "add_approved_domains")?,
            );
        }

        if let Some(raw) = args.get("remove_approved_domains") {
            Self::remove_domains(
                &mut policy.approved_domains,
                Self::parse_string_list(raw, "remove_approved_domains")?,
            );
        }

        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": "security.url_access configuration updated",
                "url_access": Self::snapshot(&cfg.security.url_access)
            }))?,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for WebAccessConfigTool {
    fn name(&self) -> &str {
        "web_access_config"
    }

    fn description(&self) -> &str {
        "Inspect and update shared network URL access policy ([security.url_access]) including first-visit approval, global allowlist/blocklist, and approved domains."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "check_url"],
                    "description": "Operation to perform"
                },
                "url": {"type": "string"},
                "block_private_ip": {"type": "boolean"},
                "allow_loopback": {"type": "boolean"},
                "require_first_visit_approval": {"type": "boolean"},
                "enforce_domain_allowlist": {"type": "boolean"},
                "allow_cidrs": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "allow_domains": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "domain_allowlist": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "domain_blocklist": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "approved_domains": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "add_domain_allowlist": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "remove_domain_allowlist": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "add_domain_blocklist": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "remove_domain_blocklist": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "add_approved_domains": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}}
                    ]
                },
                "remove_approved_domains": {
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
            "check_url" => self.handle_check_url(&args),
            "set" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }
                self.handle_set(&args).await
            }
            other => anyhow::bail!("Unsupported action '{other}'. Use get|set|check_url"),
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
    async fn check_url_reports_first_visit_approval_requirement() {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.security.url_access.require_first_visit_approval = true;
        config.save().await.unwrap();

        let tool = WebAccessConfigTool::new(Arc::new(config), test_security());
        let result = tool
            .execute(json!({
                "action": "check_url",
                "url": "https://docs.rs"
            }))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let output: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["allowed"], json!(false));
        assert!(output["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("First-time domain approval required")));
    }

    #[tokio::test]
    async fn set_supports_add_and_remove_domain_lists() {
        let tmp = TempDir::new().unwrap();
        let tool = WebAccessConfigTool::new(test_config(&tmp).await, test_security());

        let first = tool
            .execute(json!({
                "action": "set",
                "add_domain_allowlist": ["github.com", "*.rust-lang.org"],
                "add_domain_blocklist": "evil.example",
                "add_approved_domains": "docs.rs",
                "allow_loopback": true
            }))
            .await
            .unwrap();
        assert!(first.success, "{:?}", first.error);

        let second = tool
            .execute(json!({
                "action": "set",
                "remove_domain_allowlist": ["github.com"],
                "remove_domain_blocklist": ["evil.example"],
                "remove_approved_domains": "docs.rs"
            }))
            .await
            .unwrap();
        assert!(second.success, "{:?}", second.error);

        let output: Value = serde_json::from_str(&second.output).unwrap();
        let url_access = &output["url_access"];
        assert_eq!(url_access["allow_loopback"], json!(true));
        assert_eq!(url_access["domain_allowlist"], json!(["*.rust-lang.org"]));
        assert_eq!(url_access["domain_blocklist"], json!([]));
        assert_eq!(url_access["approved_domains"], json!([]));
    }
}
