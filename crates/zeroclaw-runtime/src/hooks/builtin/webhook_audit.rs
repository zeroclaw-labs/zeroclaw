use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::hooks::traits::{HookHandler, HookResult};
use zeroclaw_api::tool::ToolResult;
use zeroclaw_config::schema::WebhookAuditConfig;

/// Validate a webhook URL against SSRF attacks.
///
/// Rejects URLs with:
/// - Non-HTTPS schemes (HTTP is allowed for localhost in debug builds only)
/// - Loopback addresses (127.0.0.0/8, ::1)
/// - Link-local addresses (169.254.0.0/16, fe80::/10)
/// - RFC1918 private addresses (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16)
fn validate_webhook_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid webhook URL: {e}"))?;

    let scheme = parsed.scheme();
    let host_str = parsed.host_str().unwrap_or("");

    // Scheme check: require https, allow http only for localhost in debug builds.
    let is_localhost = host_str == "localhost" || host_str == "127.0.0.1" || host_str == "::1";

    if scheme != "https" {
        if scheme == "http" && is_localhost && cfg!(debug_assertions) {
            // Allow http://localhost in dev/debug builds.
        } else {
            return Err(format!(
                "webhook URL must use https:// scheme (got {scheme}://)"
            ));
        }
    }

    // Resolve the host to check for private/loopback/link-local IPs.
    if let Some(host) = parsed.host_str() {
        // Strip brackets from IPv6 literals.
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = bare.parse::<IpAddr>() {
            reject_private_ip(ip)?;
        } else {
            // Domain name — check for well-known loopback domains.
            if bare == "localhost" && !(cfg!(debug_assertions) && scheme == "http") {
                return Err("webhook URL must not target localhost".to_string());
            }
        }
    }

    Ok(())
}

fn reject_private_ip(addr: IpAddr) -> Result<(), String> {
    match addr {
        IpAddr::V4(ip) => {
            if ip.is_loopback() {
                return Err(format!(
                    "webhook URL must not target loopback address ({ip})"
                ));
            }
            let octets = ip.octets();
            // 10.0.0.0/8
            if octets[0] == 10 {
                return Err(format!(
                    "webhook URL must not target private address ({ip})"
                ));
            }
            // 172.16.0.0/12
            if octets[0] == 172 && (octets[1] & 0xf0) == 16 {
                return Err(format!(
                    "webhook URL must not target private address ({ip})"
                ));
            }
            // 192.168.0.0/16
            if octets[0] == 192 && octets[1] == 168 {
                return Err(format!(
                    "webhook URL must not target private address ({ip})"
                ));
            }
            // 169.254.0.0/16 (link-local)
            if octets[0] == 169 && octets[1] == 254 {
                return Err(format!(
                    "webhook URL must not target link-local address ({ip})"
                ));
            }
        }
        IpAddr::V6(ip) => {
            if ip.is_loopback() {
                return Err(format!(
                    "webhook URL must not target loopback address ({ip})"
                ));
            }
            let segments = ip.segments();
            // fe80::/10 (link-local)
            if (segments[0] & 0xffc0) == 0xfe80 {
                return Err(format!(
                    "webhook URL must not target link-local address ({ip})"
                ));
            }
        }
    }
    Ok(())
}

/// Sends an HTTP POST with a JSON audit payload for matching tool calls.
pub struct WebhookAuditHook {
    config: WebhookAuditConfig,
    client: reqwest::Client,
    pending_args: Arc<Mutex<HashMap<String, Vec<Value>>>>,
}

impl WebhookAuditHook {
    pub fn new(config: WebhookAuditConfig) -> Self {
        // Warn if enabled but no URL configured.
        if config.enabled && config.url.is_empty() {
            tracing::warn!(
                hook = "webhook-audit",
                "webhook-audit hook is enabled but no URL is configured — audit events will be dropped"
            );
        }

        // Validate URL against SSRF if one is provided.
        if !config.url.is_empty()
            && let Err(e) = validate_webhook_url(&config.url)
        {
            tracing::error!(hook = "webhook-audit", error = %e, "webhook URL validation failed");
            panic!("webhook-audit: {e}");
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build webhook HTTP client");
        Self {
            config,
            client,
            pending_args: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Simple glob matching: `*` matches any sequence of characters.
fn glob_matches(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }

    let parts: Vec<&str> = pattern.split('*').collect();

    // Edge case: pattern is just "*" (already handled above) or multiple stars
    let mut pos = 0usize;

    // The first segment must match the beginning of the text (unless pattern starts with *)
    if !pattern.starts_with('*') {
        let first = parts[0];
        if !text.starts_with(first) {
            return false;
        }
        pos = first.len();
    }

    // The last segment must match the end of the text (unless pattern ends with *)
    if !pattern.ends_with('*') {
        let last = parts[parts.len() - 1];
        if !text.ends_with(last) {
            return false;
        }
        // Ensure no overlap with the prefix we already consumed
        if text.len() < pos + last.len() {
            // Check for overlap case: e.g. pattern "ab*b" text "ab"
            // pos would be 2 (after "ab"), last is "b", text.len()=2, 2 < 2+1=3 -> false
            return false;
        }
    }

    // Now check that the middle segments appear in order between pos and
    // the end boundary.
    let end_boundary = if pattern.ends_with('*') {
        text.len()
    } else {
        text.len() - parts[parts.len() - 1].len()
    };

    let start_idx = if pattern.starts_with('*') { 0 } else { 1 };
    let end_idx = if pattern.ends_with('*') {
        parts.len()
    } else {
        parts.len() - 1
    };

    for part in &parts[start_idx..end_idx] {
        if part.is_empty() {
            continue;
        }
        if let Some(found) = text[pos..end_boundary].find(part) {
            pos += found + part.len();
        } else {
            return false;
        }
    }

    true
}

/// Returns true if `tool` matches any of the given glob patterns.
fn matches_any_pattern(patterns: &[String], tool: &str) -> bool {
    patterns.iter().any(|p| glob_matches(p, tool))
}

/// Truncate serialised args to `max_bytes`. If 0, no truncation.
///
/// Uses byte-oriented slicing with char-boundary alignment to avoid
/// mixing byte length comparisons with char-count truncation.
#[allow(clippy::cast_possible_truncation)]
fn truncate_args(args: Value, max_bytes: u64) -> Value {
    if max_bytes == 0 {
        return args;
    }
    let serialised = match serde_json::to_string(&args) {
        Ok(s) => s,
        Err(_) => return args,
    };
    if serialised.len() <= max_bytes as usize {
        args
    } else {
        let mut end = max_bytes as usize;
        while end > 0 && !serialised.is_char_boundary(end) {
            end -= 1;
        }
        Value::String(format!("{}...[truncated]", &serialised[..end]))
    }
}

#[async_trait]
impl HookHandler for WebhookAuditHook {
    fn name(&self) -> &str {
        "webhook-audit"
    }

    fn priority(&self) -> i32 {
        -100
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        if self.config.include_args && matches_any_pattern(&self.config.tool_patterns, &name) {
            tracing::debug!(hook = "webhook-audit", tool = %name, "capturing args for audit");
            self.pending_args
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(name.clone())
                .or_default()
                .push(args.clone());
        }
        HookResult::Continue((name, args))
    }

    async fn on_after_tool_call(&self, tool: &str, result: &ToolResult, duration: Duration) {
        // Skip if no URL configured.
        if self.config.url.is_empty() {
            return;
        }

        // Skip tools that don't match the configured patterns.
        if !matches_any_pattern(&self.config.tool_patterns, tool) {
            return;
        }

        // Pop the first captured args entry for this tool (FIFO) and optionally truncate.
        let args_value: Value = if self.config.include_args {
            let raw = {
                let mut map = self.pending_args.lock().unwrap_or_else(|e| e.into_inner());
                let entry = map.get_mut(tool).and_then(|v| {
                    if v.is_empty() {
                        None
                    } else {
                        Some(v.remove(0))
                    }
                });
                // Clean up empty entries.
                if map.get(tool).is_some_and(|v| v.is_empty()) {
                    map.remove(tool);
                }
                entry
            };
            match raw {
                Some(a) => truncate_args(a, self.config.max_args_bytes),
                None => Value::Null,
            }
        } else {
            Value::Null
        };

        #[allow(clippy::cast_possible_truncation)]
        let duration_ms = duration.as_millis() as u64;

        let payload = serde_json::json!({
            "event": "tool_call",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "tool": tool,
            "success": result.success,
            "duration_ms": duration_ms,
            "error": result.error,
            "args": args_value,
        });

        let client = self.client.clone();
        let url = self.config.url.clone();

        // Fire-and-forget — never block the agent loop.
        tokio::spawn(async move {
            match client.post(&url).json(&payload).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        tracing::error!(
                            hook = "webhook-audit",
                            url = %url,
                            status = %resp.status(),
                            "webhook endpoint returned non-success status"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        hook = "webhook-audit",
                        url = %url,
                        error = %e,
                        "failed to POST audit payload"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Glob matching tests ──────────────────────────────────────

    #[test]
    fn glob_exact_match() {
        assert!(glob_matches("file_write", "file_write"));
        assert!(!glob_matches("file_write", "file_read"));
    }

    #[test]
    fn glob_wildcard_suffix() {
        assert!(glob_matches("mcp__*", "mcp__github"));
        assert!(glob_matches("mcp__*", "mcp__"));
        assert!(!glob_matches("mcp__*", "mcp_github"));
    }

    #[test]
    fn glob_wildcard_prefix() {
        assert!(glob_matches("*_write", "file_write"));
        assert!(glob_matches("*_write", "_write"));
        assert!(!glob_matches("*_write", "file_read"));
    }

    #[test]
    fn glob_wildcard_middle() {
        assert!(glob_matches("mcp__*__create", "mcp__github__create"));
        assert!(glob_matches("mcp__*__create", "mcp____create"));
        assert!(!glob_matches("mcp__*__create", "mcp__github__delete"));
    }

    #[test]
    fn glob_star_matches_everything() {
        assert!(glob_matches("*", "anything_at_all"));
        assert!(glob_matches("*", ""));
    }

    #[test]
    fn glob_empty_pattern() {
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "something"));
    }

    // ── matches_any_pattern ──────────────────────────────────────

    #[test]
    fn matches_any_pattern_works() {
        let patterns = vec!["Bash".to_string(), "mcp__*".to_string()];
        assert!(matches_any_pattern(&patterns, "Bash"));
        assert!(matches_any_pattern(&patterns, "mcp__github"));
        assert!(!matches_any_pattern(&patterns, "Write"));
    }

    #[test]
    fn empty_patterns_matches_nothing() {
        let patterns: Vec<String> = vec![];
        assert!(!matches_any_pattern(&patterns, "anything"));
    }

    // ── before_tool_call tests ────────────────────────────────────

    fn make_hook(patterns: Vec<&str>, include_args: bool) -> WebhookAuditHook {
        // Use https URL for tests to pass URL validation; localhost with http
        // is only allowed in debug builds, but use https to be safe.
        WebhookAuditHook::new(WebhookAuditConfig {
            enabled: true,
            url: "https://audit.example.com/webhook".to_string(),
            tool_patterns: patterns.into_iter().map(String::from).collect(),
            include_args,
            max_args_bytes: 4096,
        })
    }

    #[tokio::test]
    async fn before_tool_call_captures_args_when_enabled() {
        let hook = make_hook(vec!["Bash", "mcp__*"], true);
        let args = serde_json::json!({"command": "ls"});
        let result = hook.before_tool_call("Bash".into(), args.clone()).await;
        assert!(!result.is_cancel());

        let pending = hook.pending_args.lock().unwrap();
        assert_eq!(pending.get("Bash"), Some(&vec![args]));
    }

    #[tokio::test]
    async fn before_tool_call_concurrent_same_tool_no_data_loss() {
        let hook = make_hook(vec!["Bash"], true);
        let args1 = serde_json::json!({"command": "ls"});
        let args2 = serde_json::json!({"command": "pwd"});
        hook.before_tool_call("Bash".into(), args1.clone()).await;
        hook.before_tool_call("Bash".into(), args2.clone()).await;

        let pending = hook.pending_args.lock().unwrap();
        let bash_args = pending.get("Bash").unwrap();
        assert_eq!(bash_args.len(), 2);
        assert_eq!(bash_args[0], args1);
        assert_eq!(bash_args[1], args2);
    }

    #[tokio::test]
    async fn before_tool_call_skips_non_matching_tools() {
        let hook = make_hook(vec!["Bash"], true);
        let args = serde_json::json!({"path": "/tmp"});
        let result = hook.before_tool_call("Write".into(), args).await;
        assert!(!result.is_cancel());

        let pending = hook.pending_args.lock().unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn before_tool_call_skips_when_include_args_false() {
        let hook = make_hook(vec!["Bash"], false);
        let args = serde_json::json!({"command": "ls"});
        let result = hook.before_tool_call("Bash".into(), args).await;
        assert!(!result.is_cancel());

        let pending = hook.pending_args.lock().unwrap();
        assert!(pending.is_empty());
    }

    // ── Truncation tests ─────────────────────────────────────────

    #[test]
    fn truncate_args_within_limit() {
        let args = serde_json::json!({"key": "val"});
        let result = truncate_args(args.clone(), 1000);
        assert_eq!(result, args);
    }

    #[test]
    fn truncate_args_over_limit() {
        let args = serde_json::json!({"key": "a]long value that exceeds limit"});
        let result = truncate_args(args, 10);
        assert!(result.is_string());
        let s = result.as_str().unwrap();
        assert!(s.ends_with("...[truncated]"));
    }

    #[test]
    fn truncate_args_zero_means_no_limit() {
        let args = serde_json::json!({"key": "value"});
        let result = truncate_args(args.clone(), 0);
        assert_eq!(result, args);
    }

    // ── on_after_tool_call tests ─────────────────────────────────

    #[tokio::test]
    async fn on_after_tool_call_skips_non_matching() {
        let hook = make_hook(vec!["Bash"], true);
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };
        // Call with a non-matching tool — should not panic or do anything.
        hook.on_after_tool_call("Write", &result, Duration::from_millis(10))
            .await;
        // No assertion needed beyond "doesn't panic"; args map stays empty.
        let pending = hook.pending_args.lock().unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn on_after_tool_call_skips_empty_url() {
        // Empty URL + enabled triggers a warning, but should not panic.
        let hook = WebhookAuditHook::new(WebhookAuditConfig {
            enabled: true,
            url: String::new(),
            tool_patterns: vec!["Bash".to_string()],
            include_args: false,
            max_args_bytes: 4096,
        });
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };
        // Should return immediately without spawning any HTTP request.
        hook.on_after_tool_call("Bash", &result, Duration::from_millis(5))
            .await;
    }

    // ── URL validation tests ─────────────────────────────────────

    #[test]
    fn validate_url_rejects_loopback_ipv4() {
        assert!(validate_webhook_url("https://127.0.0.1/hook").is_err());
        assert!(validate_webhook_url("https://127.0.0.100/hook").is_err());
    }

    #[test]
    fn validate_url_rejects_loopback_ipv6() {
        assert!(validate_webhook_url("https://[::1]/hook").is_err());
    }

    #[test]
    fn validate_url_rejects_private_rfc1918() {
        assert!(validate_webhook_url("https://10.0.0.1/hook").is_err());
        assert!(validate_webhook_url("https://172.16.5.1/hook").is_err());
        assert!(validate_webhook_url("https://192.168.1.1/hook").is_err());
    }

    #[test]
    fn validate_url_rejects_link_local() {
        assert!(validate_webhook_url("https://169.254.1.1/hook").is_err());
        assert!(validate_webhook_url("https://[fe80::1]/hook").is_err());
    }

    #[test]
    fn validate_url_rejects_http_non_localhost() {
        assert!(validate_webhook_url("http://example.com/hook").is_err());
    }

    #[test]
    fn validate_url_accepts_https_public() {
        assert!(validate_webhook_url("https://audit.example.com/webhook").is_ok());
        assert!(validate_webhook_url("https://8.8.8.8/hook").is_ok());
    }

    #[test]
    fn validate_url_rejects_non_http_scheme() {
        assert!(validate_webhook_url("ftp://example.com/hook").is_err());
    }
}
