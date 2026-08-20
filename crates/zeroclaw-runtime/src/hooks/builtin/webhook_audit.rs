use async_trait::async_trait;
use lru::LruCache;
use serde_json::Value;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::loop_::scrub_for_export;
use crate::agent::turn::redact::scrub_credentials_value;
use crate::hooks::traits::{HookHandler, HookResult};
use zeroclaw_api::hook::ToolCallHookContext;
use zeroclaw_api::tool::ToolResult;
use zeroclaw_config::schema::WebhookAuditConfig;

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
    pending_args: Arc<Mutex<LruCache<String, Value>>>,
}

const MAX_PENDING_ARGS: usize = 1024;

impl WebhookAuditHook {
    pub fn new(config: WebhookAuditConfig) -> Result<Self, String> {
        if config.url.is_empty() {
            return Err("webhook URL is required when webhook audit is enabled".to_string());
        }
        validate_webhook_url(&config.url)?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("failed to build webhook HTTP client: {e}"))?;
        Ok(Self {
            config,
            client,
            pending_args: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(MAX_PENDING_ARGS).expect("pending audit capacity is non-zero"),
            ))),
        })
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

fn scrub_export_string_leaves(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for value in map.values_mut() {
                scrub_export_string_leaves(value);
            }
        }
        Value::Array(items) => {
            for value in items {
                scrub_export_string_leaves(value);
            }
        }
        Value::String(value) => *value = scrub_for_export(value),
        _ => {}
    }
}

fn prepare_args_for_export(args: Value, max_bytes: u64) -> Value {
    let mut scrubbed = scrub_credentials_value(args);
    scrub_export_string_leaves(&mut scrubbed);
    truncate_args(scrubbed, max_bytes)
}

impl WebhookAuditHook {
    fn build_payload(
        &self,
        context: &ToolCallHookContext,
        tool: &str,
        result: &ToolResult,
        duration: Duration,
    ) -> Option<Value> {
        let raw_args = if self.config.include_args && context.is_correlated() {
            self.pending_args
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop(context.invocation_id())
        } else {
            None
        };

        if !matches_any_pattern(&self.config.tool_patterns, tool) {
            return None;
        }

        let args_value = if self.config.include_args {
            raw_args
                .map(|args| prepare_args_for_export(args, self.config.max_args_bytes))
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        };

        #[allow(clippy::cast_possible_truncation)]
        let duration_ms = duration.as_millis() as u64;

        Some(serde_json::json!({
            "event": "tool_call",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "tool": tool,
            "success": result.success,
            "duration_ms": duration_ms,
            "error": result.error,
            "args": args_value,
        }))
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
        let context = ToolCallHookContext::uncorrelated("webhook-audit:legacy-before");
        self.before_tool_call_with_context(&context, name, args)
            .await
    }

    async fn on_after_tool_call(&self, tool: &str, result: &ToolResult, duration: Duration) {
        let context = ToolCallHookContext::uncorrelated("webhook-audit:legacy-after");
        self.on_after_tool_call_with_context(&context, tool, result, duration)
            .await;
    }

    async fn before_tool_call_with_context(
        &self,
        context: &ToolCallHookContext,
        name: String,
        args: Value,
    ) -> HookResult<(String, Value)> {
        if self.config.include_args
            && context.is_correlated()
            && matches_any_pattern(&self.config.tool_patterns, &name)
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"hook": "webhook-audit", "tool": name})),
                "capturing args for audit"
            );
            let invocation_id = context.invocation_id().to_string();
            let args_snapshot = args.clone();
            let evicted = self
                .pending_args
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(invocation_id, args_snapshot);
            drop(evicted);
        }
        HookResult::Continue((name, args))
    }

    async fn on_after_tool_call_with_context(
        &self,
        context: &ToolCallHookContext,
        tool: &str,
        result: &ToolResult,
        duration: Duration,
    ) {
        let Some(payload) = self.build_payload(context, tool, result, duration) else {
            return;
        };

        let client = self.client.clone();
        let url = self.config.url.clone();

        // Fire-and-forget — never block the agent loop.
        zeroclaw_spawn::spawn!(async move {
            match client.post(&url).json(&payload).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"hook": "webhook-audit", "url": url, "status": resp.status().to_string()})), "webhook endpoint returned non-success status");
                    }
                }
                Err(e) => {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"hook": "webhook-audit", "url": url, "error": format!("{}", e)})), "failed to POST audit payload");
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
        .expect("valid webhook audit fixture")
    }

    fn hook_context(invocation_id: &str) -> ToolCallHookContext {
        ToolCallHookContext::new(invocation_id)
    }

    #[tokio::test]
    async fn before_tool_call_captures_args_when_enabled() {
        let hook = make_hook(vec!["Bash", "mcp__*"], true);
        let args = serde_json::json!({"command": "ls"});
        let result = hook
            .before_tool_call_with_context(&hook_context("call-a"), "Bash".into(), args.clone())
            .await;
        assert!(!result.is_cancel());

        let pending = hook.pending_args.lock().unwrap();
        assert_eq!(pending.peek("call-a"), Some(&args));
    }

    #[tokio::test]
    async fn before_tool_call_concurrent_same_tool_no_data_loss() {
        let hook = make_hook(vec!["Bash"], true);
        let args1 = serde_json::json!({"command": "ls"});
        let args2 = serde_json::json!({"command": "pwd"});
        hook.before_tool_call_with_context(&hook_context("call-a"), "Bash".into(), args1.clone())
            .await;
        hook.before_tool_call_with_context(&hook_context("call-b"), "Bash".into(), args2.clone())
            .await;

        let pending = hook.pending_args.lock().unwrap();
        assert_eq!(pending.peek("call-a"), Some(&args1));
        assert_eq!(pending.peek("call-b"), Some(&args2));
    }

    #[tokio::test]
    async fn before_tool_call_skips_non_matching_tools() {
        let hook = make_hook(vec!["Bash"], true);
        let args = serde_json::json!({"path": "/tmp"});
        let result = hook
            .before_tool_call_with_context(&hook_context("call-a"), "Write".into(), args)
            .await;
        assert!(!result.is_cancel());

        let pending = hook.pending_args.lock().unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn before_tool_call_skips_when_include_args_false() {
        let hook = make_hook(vec!["Bash"], false);
        let args = serde_json::json!({"command": "ls"});
        let result = hook
            .before_tool_call_with_context(&hook_context("call-a"), "Bash".into(), args)
            .await;
        assert!(!result.is_cancel());

        let pending = hook.pending_args.lock().unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn legacy_runner_dispatches_webhook_audit_without_capturing_args() {
        let hook = make_hook(vec!["Bash"], true);
        let pending_args = Arc::clone(&hook.pending_args);
        let mut runner = crate::hooks::HookRunner::new();
        runner.register(Box::new(hook));

        let before = runner
            .run_before_tool_call(
                "Bash".into(),
                serde_json::json!({"command": "printf compatibility"}),
            )
            .await;
        assert!(!before.is_cancel());
        assert!(pending_args.lock().unwrap().is_empty());

        runner
            .fire_after_tool_call(
                "Bash",
                &ToolResult {
                    success: true,
                    output: "ok".into(),
                    error: None,
                },
                Duration::ZERO,
            )
            .await;

        assert!(pending_args.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn direct_legacy_handler_calls_remain_fail_closed() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let mut hook = make_hook(vec!["Bash"], true);
        hook.config.url = server.uri();
        let before = hook
            .before_tool_call(
                "Bash".into(),
                serde_json::json!({"command": "printf compatibility"}),
            )
            .await;
        assert!(!before.is_cancel());
        assert!(hook.pending_args.lock().unwrap().is_empty());

        hook.on_after_tool_call(
            "Bash",
            &ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            },
            Duration::ZERO,
        )
        .await;

        let request = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(request) = server
                    .received_requests()
                    .await
                    .expect("request recording enabled")
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("legacy direct completion must dispatch an audit event");
        let payload: Value = serde_json::from_slice(&request.body).expect("JSON audit payload");
        assert_eq!(payload["tool"], "Bash");
        assert_eq!(payload["args"], Value::Null);
        assert!(hook.pending_args.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn uncorrelated_completion_cannot_consume_same_id_args() {
        let hook = make_hook(vec!["Bash"], true);
        let exact = hook_context("shared-id");
        let args = serde_json::json!({"command": "private"});
        hook.before_tool_call_with_context(&exact, "Bash".into(), args.clone())
            .await;
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };

        let uncorrelated = ToolCallHookContext::uncorrelated("shared-id");
        let uncorrelated_payload = hook
            .build_payload(&uncorrelated, "Bash", &result, Duration::ZERO)
            .expect("matching uncorrelated payload");
        assert_eq!(uncorrelated_payload["args"], Value::Null);
        assert_eq!(
            hook.pending_args.lock().unwrap().peek("shared-id"),
            Some(&args)
        );

        let exact_payload = hook
            .build_payload(&exact, "Bash", &result, Duration::ZERO)
            .expect("matching exact payload");
        assert_eq!(exact_payload["args"], args);
        assert!(hook.pending_args.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn pending_args_follow_identity_when_same_tool_completes_in_reverse() {
        let hook = make_hook(vec!["Bash"], true);
        let args_a = serde_json::json!({"command": "first"});
        let args_b = serde_json::json!({"command": "second"});
        hook.before_tool_call_with_context(&hook_context("call-a"), "Bash".into(), args_a.clone())
            .await;
        hook.before_tool_call_with_context(&hook_context("call-b"), "Bash".into(), args_b.clone())
            .await;

        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };
        let payload_b = hook
            .build_payload(&hook_context("call-b"), "Bash", &result, Duration::ZERO)
            .expect("matching call-b payload");
        let payload_a = hook
            .build_payload(&hook_context("call-a"), "Bash", &result, Duration::ZERO)
            .expect("matching call-a payload");

        assert_eq!(payload_b["args"], args_b);
        assert_eq!(payload_a["args"], args_a);
        assert!(hook.pending_args.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn completion_with_nonmatching_final_name_clears_matching_identity() {
        let hook = make_hook(vec!["Bash"], true);
        let context = hook_context("call-a");
        hook.before_tool_call_with_context(
            &context,
            "Bash".into(),
            serde_json::json!({"command": "ls"}),
        )
        .await;
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };

        hook.on_after_tool_call_with_context(&context, "renamed_nonmatch", &result, Duration::ZERO)
            .await;

        let pending = hook.pending_args.lock().unwrap();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn pending_args_evict_oldest_identity_without_reassignment() {
        let hook = make_hook(vec!["Bash"], true);
        *hook.pending_args.lock().unwrap() = LruCache::new(NonZeroUsize::new(2).unwrap());
        for (id, value) in [("call-a", "a"), ("call-b", "b"), ("call-c", "c")] {
            hook.before_tool_call_with_context(
                &hook_context(id),
                "Bash".into(),
                serde_json::json!({"call": value}),
            )
            .await;
        }
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };

        let evicted = hook
            .build_payload(&hook_context("call-a"), "Bash", &result, Duration::ZERO)
            .expect("matching evicted payload");
        let retained_b = hook
            .build_payload(&hook_context("call-b"), "Bash", &result, Duration::ZERO)
            .expect("matching retained payload");
        let retained_c = hook
            .build_payload(&hook_context("call-c"), "Bash", &result, Duration::ZERO)
            .expect("matching retained payload");

        assert_eq!(evicted["args"], Value::Null);
        assert_eq!(retained_b["args"], serde_json::json!({"call": "b"}));
        assert_eq!(retained_c["args"], serde_json::json!({"call": "c"}));
        assert!(hook.pending_args.lock().unwrap().is_empty());
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

    #[test]
    fn prepare_args_for_export_scrubs_credentials() {
        let secret = "SUPERSECRETVALUE123";
        let args = serde_json::json!({"command": format!("echo api_key={secret}")});

        let exported = prepare_args_for_export(args, 0);

        assert!(!exported.to_string().contains(secret));
    }

    #[test]
    fn prepare_args_for_export_preserves_structure_for_sensitive_keys() {
        let secret = "SYNTHETIC-CREDENTIAL-VALUE";
        let args = serde_json::json!({
            "request": {
                "access_token": secret,
                "status": "ok"
            },
            "count": 3
        });

        let exported = prepare_args_for_export(args, 0);

        assert!(exported.is_object());
        assert_eq!(exported["request"]["status"], "ok");
        assert_eq!(exported["count"], 3);
        assert!(exported["request"]["access_token"].is_string());
        assert!(!exported.to_string().contains(secret));
    }

    #[test]
    fn prepare_args_for_export_scrubs_provider_tokens_and_image_markers() {
        let provider_token = ["ghp_", "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"].concat();
        let args = serde_json::json!({
            "request": {
                "command": format!("deploy with {provider_token}"),
                "attachment": "[IMAGE:data:image/png;base64,AAAA]"
            }
        });

        let exported = prepare_args_for_export(args, 0);

        assert!(exported.is_object());
        assert!(!exported.to_string().contains(&provider_token));
        assert_eq!(
            exported["request"]["attachment"],
            "[IMAGE:<image data elided>]"
        );
    }

    #[test]
    fn prepare_args_for_export_scrubs_before_truncation() {
        let secret = "SUPERSECRETVALUE123";
        let args = serde_json::json!({"command": format!("echo api_key={secret} and continue")});

        let exported = prepare_args_for_export(args, 24);

        assert!(exported.is_string());
        assert!(!exported.as_str().unwrap().contains(secret));
        assert!(exported.as_str().unwrap().ends_with("...[truncated]"));
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
        hook.on_after_tool_call_with_context(
            &hook_context("call-a"),
            "Write",
            &result,
            Duration::from_millis(10),
        )
        .await;
        // No assertion needed beyond "doesn't panic"; args map stays empty.
        let pending = hook.pending_args.lock().unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn constructor_rejects_empty_url_without_panicking() {
        let result = WebhookAuditHook::new(WebhookAuditConfig {
            enabled: true,
            url: String::new(),
            tool_patterns: vec!["Bash".to_string()],
            include_args: false,
            max_args_bytes: 4096,
        });

        let error = result.err().expect("empty URL must be rejected");
        assert!(error.contains("URL is required"));
    }

    #[test]
    fn constructor_rejects_invalid_url_without_panicking() {
        let result = WebhookAuditHook::new(WebhookAuditConfig {
            enabled: true,
            url: "http://example.com/audit".to_string(),
            tool_patterns: vec!["Bash".to_string()],
            include_args: false,
            max_args_bytes: 4096,
        });

        let error = result.err().expect("insecure URL must be rejected");
        assert!(error.contains("must use https"));
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
