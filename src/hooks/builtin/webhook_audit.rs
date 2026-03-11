use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::schema::WebhookAuditConfig;
use crate::hooks::traits::{HookHandler, HookResult};
use crate::tools::traits::ToolResult;

/// Sends an HTTP POST with a JSON audit payload for matching tool calls.
pub struct WebhookAuditHook {
    config: WebhookAuditConfig,
    client: reqwest::Client,
    pending_args: Arc<Mutex<HashMap<String, Value>>>,
}

impl WebhookAuditHook {
    pub fn new(config: WebhookAuditConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
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
    let end_boundary = if !pattern.ends_with('*') {
        text.len() - parts[parts.len() - 1].len()
    } else {
        text.len()
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
fn truncate_args(args: Value, max_bytes: u64) -> Value {
    if max_bytes == 0 {
        return args;
    }
    let serialised = match serde_json::to_string(&args) {
        Ok(s) => s,
        Err(_) => return args,
    };
    if (serialised.len() as u64) <= max_bytes {
        args
    } else {
        let truncated: String = serialised
            .chars()
            .take(max_bytes as usize)
            .collect();
        Value::String(format!("{}...[truncated]", truncated))
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
                .insert(name.clone(), args.clone());
        }
        HookResult::Continue((name, args))
    }

    async fn on_after_tool_call(&self, _tool: &str, _result: &ToolResult, _duration: Duration) {
        // Placeholder — will be implemented in a later commit.
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
        WebhookAuditHook::new(WebhookAuditConfig {
            enabled: true,
            url: "http://localhost:9999/audit".to_string(),
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
        assert_eq!(pending.get("Bash"), Some(&args));
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
}
