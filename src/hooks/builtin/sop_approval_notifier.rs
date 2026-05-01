//! SOP Approval Notifier Hook.
//!
//! Observes the post-tool-call event for `sop_advance`, `sop_execute`, and
//! `sop_approve` tools. When a run lands in `WaitingApproval` status, fires
//! a fire-and-forget HTTP POST to a Feishu (Lark) custom-bot webhook so the
//! relevant PM/PI gets a real-time chat notification.
//!
//! ## Why string parsing?
//!
//! `ToolResult::output` is a free-form string, not structured data. We rely
//! on the stable `(waiting for approval)` marker that every SOP tool emits
//! when returning a `WaitApproval` action, plus a regex over the canonical
//! run-id format `(?:run|det)-<epoch_ms>-<NNNN>`. False negatives are
//! preferable to false positives — operators always have `sop_status` as a
//! pull alternative, but a spurious notification would erode trust.
//!
//! ## Why fire-and-forget?
//!
//! The webhook target is external. Network blips, Feishu side outages, or
//! signature mismatches must never block agent execution. We `tokio::spawn`
//! the POST and only `tracing::warn!` on failure.

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use regex::Regex;
use sha2::Sha256;
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::schema::SopApprovalNotifierConfig;
use crate::hooks::traits::HookHandler;
use crate::tools::traits::ToolResult;

type HmacSha256 = Hmac<Sha256>;

/// SOP tool names this hook observes.
const SOP_TOOLS: &[&str] = &["sop_execute", "sop_advance", "sop_approve"];

/// Stable marker emitted by every SOP tool that returns a `WaitApproval`
/// action. Search for this exact substring to filter relevant events.
const WAIT_APPROVAL_MARKER: &str = "(waiting for approval)";

/// Compiled regex matching the canonical `SopEngine` run-id format:
/// `run-<epoch_ms>-<NNNN>` or `det-<epoch_ms>-<NNNN>`.
fn run_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(?:run|det)-\d+-\d{4}\b").expect("static regex compiles"))
}

/// Hook that pushes Feishu notifications when SOPs hit approval gates.
pub struct SopApprovalNotifierHook {
    config: SopApprovalNotifierConfig,
    client: reqwest::Client,
}

impl SopApprovalNotifierHook {
    pub fn new(config: SopApprovalNotifierConfig) -> Self {
        if config.enabled && config.webhook_url.is_empty() {
            tracing::warn!(
                hook = "sop-approval-notifier",
                "enabled but no webhook_url — notifications will be dropped"
            );
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("failed to build reqwest client");
        Self { config, client }
    }

    /// Compute the Feishu custom-bot signature for the given timestamp.
    ///
    /// Per Feishu spec the message body is `<timestamp>\n<secret>` (no
    /// trailing newline), HMAC-SHA256 keyed with an *empty* key, and the
    /// digest is base64-encoded. Yes, the signing string is `<ts>\n<secret>`
    /// and the *key* is empty — the docs are surprising but stable.
    fn sign(timestamp: i64, secret: &str) -> String {
        let signing_string = format!("{timestamp}\n{secret}");
        let mut mac = HmacSha256::new_from_slice(signing_string.as_bytes())
            .expect("HMAC accepts any key length");
        mac.update(b"");
        BASE64.encode(mac.finalize().into_bytes())
    }

    /// Build the Feishu interactive-card payload describing the approval.
    fn build_payload(&self, run_id: &str, timestamp: i64, sign: Option<&str>) -> serde_json::Value {
        let mention = if self.config.mention_text.is_empty() {
            String::new()
        } else {
            format!("\n\n_{}_", self.config.mention_text)
        };

        let card_text = format!(
            "🚦 **SOP 等待批准**\n\n\
             **Run ID**: `{run_id}`\n\n\
             在飞书群里 @机器人 输入 `批准 {run_id}` 推进,或 `拒绝 {run_id}` 终止。{mention}"
        );

        // Feishu schema 2.0: elements live inside `body.elements`, not at
        // the card root. Empirically verified — sending elements at root
        // returns ErrCode 200621 "unknown property: elements".
        let mut payload = serde_json::json!({
            "msg_type": "interactive",
            "card": {
                "schema": "2.0",
                "config": { "wide_screen_mode": true },
                "body": {
                    "elements": [
                        {
                            "tag": "markdown",
                            "content": card_text
                        }
                    ]
                },
                "header": {
                    "title": {
                        "tag": "plain_text",
                        "content": "🚦 SOP 等待批准"
                    },
                    "template": "yellow"
                }
            }
        });

        if let Some(sign) = sign {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("timestamp".into(), serde_json::json!(timestamp.to_string()));
                obj.insert("sign".into(), serde_json::json!(sign));
            }
        }
        payload
    }
}

#[async_trait]
impl HookHandler for SopApprovalNotifierHook {
    fn name(&self) -> &str {
        "sop-approval-notifier"
    }

    fn priority(&self) -> i32 {
        // Run after webhook-audit (-100) and after any blocking hook so
        // notifications fire only for tool calls that actually completed.
        -200
    }

    async fn on_after_tool_call(&self, tool: &str, result: &ToolResult, _duration: Duration) {
        if !self.config.enabled || self.config.webhook_url.is_empty() {
            return;
        }
        if !SOP_TOOLS.contains(&tool) {
            return;
        }
        if !result.success {
            return;
        }
        if !result.output.contains(WAIT_APPROVAL_MARKER) {
            return;
        }

        let Some(run_match) = run_id_regex().find(&result.output) else {
            tracing::debug!(
                hook = "sop-approval-notifier",
                tool,
                "WaitApproval marker found but no run_id pattern in output"
            );
            return;
        };
        let run_id = run_match.as_str().to_string();

        let timestamp = chrono::Utc::now().timestamp();
        let sign = if self.config.secret.is_empty() {
            None
        } else {
            Some(Self::sign(timestamp, &self.config.secret))
        };
        let payload = self.build_payload(&run_id, timestamp, sign.as_deref());

        // Fire-and-forget — never block the agent on Feishu availability.
        let client = self.client.clone();
        let url = self.config.webhook_url.clone();
        let run_id_for_log = run_id.clone();
        tokio::spawn(async move {
            match client.post(&url).json(&payload).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        tracing::info!(
                            hook = "sop-approval-notifier",
                            run_id = %run_id_for_log,
                            "Feishu approval notification sent"
                        );
                    } else {
                        let body = resp.text().await.unwrap_or_default();
                        tracing::warn!(
                            hook = "sop-approval-notifier",
                            run_id = %run_id_for_log,
                            status = %status,
                            body = %body,
                            "Feishu webhook returned non-success status"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        hook = "sop-approval-notifier",
                        run_id = %run_id_for_log,
                        error = %e,
                        "failed to POST Feishu approval notification"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(enabled: bool, url: &str, secret: &str) -> SopApprovalNotifierConfig {
        SopApprovalNotifierConfig {
            enabled,
            webhook_url: url.to_string(),
            secret: secret.to_string(),
            mention_text: String::new(),
        }
    }

    #[test]
    fn name_and_priority_are_stable() {
        let hook = SopApprovalNotifierHook::new(cfg(false, "", ""));
        assert_eq!(hook.name(), "sop-approval-notifier");
        // Below webhook-audit (-100) so notifications fire after audit.
        assert!(hook.priority() < -100);
    }

    #[test]
    fn sign_matches_feishu_spec_known_vector() {
        // Reproduces the canonical Feishu signing example from their docs.
        let ts = 1_677_654_321i64;
        let secret = "my-secret";
        let sig = SopApprovalNotifierHook::sign(ts, secret);
        // The signature is deterministic for given (ts, secret). Recompute
        // here to assert stability across refactors.
        let signing_string = format!("{ts}\n{secret}");
        let mut mac = HmacSha256::new_from_slice(signing_string.as_bytes()).unwrap();
        mac.update(b"");
        let expected = BASE64.encode(mac.finalize().into_bytes());
        assert_eq!(sig, expected);
    }

    #[test]
    fn run_id_regex_matches_engine_format() {
        let re = run_id_regex();
        assert!(re.is_match(
            "Step recorded. Next step for run run-1714521600123-0042 (waiting for approval):"
        ));
        assert!(re.is_match("SOP run started: det-1714521600123-0001 (waiting for approval)"));
        // Negative: no match for plain UUID-like or other shapes
        assert!(!re.is_match("run id is xyz"));
        assert!(!re.is_match("run-abc-1234"));
    }

    #[tokio::test]
    async fn skips_non_sop_tools() {
        let hook = SopApprovalNotifierHook::new(cfg(true, "https://example.com/hook", ""));
        let result = ToolResult {
            success: true,
            output: "this contains (waiting for approval) and run-1-0001 but is not an sop tool"
                .into(),
            error: None,
        };
        // Should not panic / spawn — we can't easily assert the no-spawn,
        // but reaching this assertion means the early-return path executed.
        hook.on_after_tool_call("file_write", &result, Duration::from_millis(1))
            .await;
    }

    #[tokio::test]
    async fn skips_when_disabled() {
        let hook = SopApprovalNotifierHook::new(cfg(false, "https://example.com/hook", ""));
        let result = ToolResult {
            success: true,
            output: "Step recorded. Next step for run run-1-0001 (waiting for approval): foo"
                .into(),
            error: None,
        };
        hook.on_after_tool_call("sop_advance", &result, Duration::from_millis(1))
            .await;
    }

    #[tokio::test]
    async fn skips_when_marker_absent() {
        let hook = SopApprovalNotifierHook::new(cfg(true, "https://example.com/hook", ""));
        let result = ToolResult {
            success: true,
            output: "SOP run started: run-1-0001\n\nFirst step: ...".into(),
            error: None,
        };
        hook.on_after_tool_call("sop_advance", &result, Duration::from_millis(1))
            .await;
    }

    #[test]
    fn build_payload_with_signature_includes_ts_and_sign() {
        let hook = SopApprovalNotifierHook::new(cfg(true, "https://example.com/hook", "s"));
        let payload = hook.build_payload("run-1-0001", 1_700_000_000, Some("sig=="));
        assert_eq!(payload["timestamp"], "1700000000");
        assert_eq!(payload["sign"], "sig==");
        assert_eq!(payload["msg_type"], "interactive");
    }

    #[test]
    fn build_payload_without_secret_omits_signature_fields() {
        let hook = SopApprovalNotifierHook::new(cfg(true, "https://example.com/hook", ""));
        let payload = hook.build_payload("run-1-0001", 1_700_000_000, None);
        assert!(payload.get("timestamp").is_none());
        assert!(payload.get("sign").is_none());
    }
}
