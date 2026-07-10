//! `forge.comment` deterministic SOP capability.
//!
//! Posts a comment to a git-forge issue/PR as the work product of a SOP step —
//! the write-back primitive a triage SOP uses after a human approval gate clears.
//!
//! Like `shell.exec` / `notify.channel`, this is **fail-closed** until a real
//! [`ForgeCommentAdapter`] is injected at engine-build time (the daemon supplies
//! [`ChannelForgeAdapter`] over its channel map; CLI / offline paths leave it
//! `None`, so the capability reports a clear failure instead of a silent no-op).
//! Because it runs on the deterministic executor it can execute headlessly after
//! an out-of-band approval, without a live agent turn.
//!
//! Layering mirrors `approval::channel_route`: this module needs only
//! [`zeroclaw_api::channel::Channel`], never `zeroclaw-channels` — the daemon
//! builds the concrete channel map and injects it, so there is no
//! runtime -> channels inversion.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use zeroclaw_api::channel::{Channel, SendMessage};

use super::types::{CapabilityContext, CapabilityInfo, CapabilityResult, SopCapability};

/// Upper bound on one forge write (a single HTTP POST). The capability blocks the
/// engine lock while waiting, so this must stay small; a forge that can't accept a
/// comment in this window is treated as failed and the step's `on_failure` policy
/// takes over.
const POST_TIMEOUT: Duration = Duration::from_secs(30);

/// Injected seam that performs the actual forge write. Kept **synchronous** because
/// [`SopCapability::execute`] is sync (called under the engine mutex); implementations
/// bridge to their async forge client themselves (see [`ChannelForgeAdapter`]).
pub trait ForgeCommentAdapter: Send + Sync {
    /// Post `body` as a comment on `repo` issue/PR `number`.
    ///
    /// `channel` optionally names the channel-map key to post through
    /// (`git.<alias>`); `None` = the adapter's single configured git channel.
    /// Returns a human-readable error on failure.
    fn post_comment(
        &self,
        channel: Option<&str>,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<(), String>;
}

/// `forge.comment` capability. Holds an optional adapter; `None` = fail-closed.
pub struct ForgeCommentCapability {
    adapter: Option<Arc<dyn ForgeCommentAdapter>>,
}

impl ForgeCommentCapability {
    pub fn new(adapter: Option<Arc<dyn ForgeCommentAdapter>>) -> Self {
        Self { adapter }
    }
}

impl SopCapability for ForgeCommentCapability {
    fn id(&self) -> &'static str {
        "forge.comment"
    }

    fn describe(&self) -> CapabilityInfo {
        CapabilityInfo {
            id: self.id(),
            description: "Post a comment to a git-forge issue/PR (fail-closed until a forge adapter is injected)",
            deterministic: true,
            // Not idempotent: each call creates a new comment.
            idempotent: false,
            reversible: false,
            supports_retry: false,
            required_permissions: vec!["forge.write"],
            // No `required` here: the effective fields may sit at the top level OR
            // nested under `input` (a step with static `capability_input` receives
            // the piped payload under that key), which a flat schema cannot
            // express. `execute` enforces the real contract with field-precise
            // errors either way.
            input_schema: Some(json!({
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "owner/repo" },
                    "number": { "type": "integer", "description": "issue or PR number" },
                    "body": { "type": "string", "description": "comment markdown" },
                    "channel": { "type": "string", "description": "optional channel-map key (git.<alias>); default = the single configured git channel" },
                    "input": { "description": "the piped payload carrying repo/number/body when capability_input is configured" }
                }
            })),
            output_schema: Some(json!({
                "type": "object",
                "required": ["posted", "repo", "number"],
                "properties": {
                    "posted": { "type": "boolean" },
                    "repo": { "type": "string" },
                    "number": { "type": "integer" }
                }
            })),
        }
    }

    fn execute(&self, _ctx: CapabilityContext, input: Value) -> Result<CapabilityResult> {
        let Some(adapter) = self.adapter.as_ref() else {
            return Ok(CapabilityResult::failure(
                "forge.comment capability requires an injected forge-write adapter",
            ));
        };

        // The step's static capability_input is merged with the piped value under
        // the `input` key; accept the fields at either the top level or nested.
        let src = input
            .get("input")
            .filter(|v| v.is_object())
            .unwrap_or(&input);

        let repo = match src.get("repo").and_then(Value::as_str) {
            Some(r) if !r.trim().is_empty() => r.trim(),
            _ => {
                return Ok(CapabilityResult::failure(
                    "forge.comment: missing string field 'repo' (owner/repo)",
                ));
            }
        };
        let number = match src.get("number").and_then(Value::as_u64) {
            Some(n) => n,
            None => {
                return Ok(CapabilityResult::failure(
                    "forge.comment: missing integer field 'number'",
                ));
            }
        };
        let body = match src.get("body").and_then(Value::as_str) {
            Some(b) if !b.trim().is_empty() => b,
            _ => {
                return Ok(CapabilityResult::failure(
                    "forge.comment: missing non-empty string field 'body'",
                ));
            }
        };
        // `channel` is authored in the static capability_input, so it lives at the
        // TOP level of the merged input (unlike repo/number/body, which usually
        // arrive in the nested piped payload). Accept both, top level first.
        let channel = input
            .get("channel")
            .or_else(|| src.get("channel"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty());

        match adapter.post_comment(channel, repo, number, body) {
            Ok(()) => Ok(CapabilityResult::success(json!({
                "posted": true,
                "repo": repo,
                "number": number,
            }))),
            Err(e) => Ok(CapabilityResult::failure(format!(
                "forge.comment: post to {repo}#{number} failed: {e}"
            ))),
        }
    }
}

/// [`ForgeCommentAdapter`] over the daemon's channel map: posts by driving the git
/// channel's normal outbound path (`Channel::send` with an `owner/repo#number`
/// recipient — the same path an agent reply takes), so it is provider-agnostic
/// (GitHub / Gitea / Forgejo alike) and reuses chunking + auth as-is.
///
/// Unlike the fire-and-forget approval route adapter, this WAITS for the send
/// result (bounded by [`POST_TIMEOUT`]): the comment IS the step's work product,
/// so the SOP must observe success/failure. The sync->async bridge runs the send
/// on a DEDICATED thread with its own small runtime (see [`run_bridged`]) rather
/// than spawning onto the host runtime: the capability executes while blocking a
/// host thread, and on a current-thread host context a task spawned back onto
/// that same runtime cannot be polled until the caller unblocks — a guaranteed
/// timeout (observed in the field before this design).
pub struct ChannelForgeAdapter {
    channels: HashMap<String, Arc<dyn Channel>>,
}

impl ChannelForgeAdapter {
    pub fn new(channels: HashMap<String, Arc<dyn Channel>>) -> Self {
        Self { channels }
    }

    /// Resolve which channel to post through: an explicit `git.<alias>` key, or the
    /// unique git channel in the map. Ambiguity and absence are hard errors so a
    /// multi-forge daemon never posts to the wrong host silently.
    fn resolve(&self, channel: Option<&str>) -> Result<Arc<dyn Channel>, String> {
        if let Some(key) = channel {
            return self
                .channels
                .get(key)
                .cloned()
                .ok_or_else(|| format!("channel '{key}' is not a configured channel"));
        }
        let mut git_keys: Vec<&String> = self
            .channels
            .keys()
            .filter(|k| *k == "git" || k.starts_with("git."))
            .collect();
        git_keys.sort();
        // The channel map registers a singleton under BOTH its bare type key and
        // its dotted alias; those are the same instance, not an ambiguity. Only
        // genuinely distinct channels require the explicit 'channel' field.
        let mut distinct: Vec<&String> = Vec::new();
        for key in git_keys {
            let arc = &self.channels[key];
            if !distinct
                .iter()
                .any(|k| Arc::ptr_eq(&self.channels[*k], arc))
            {
                distinct.push(key);
            }
        }
        match distinct.as_slice() {
            [] => Err("no git channel is configured (nothing to post through)".to_string()),
            [only] => Ok(Arc::clone(&self.channels[*only])),
            many => Err(format!(
                "multiple git channels configured ({}); set the 'channel' input field",
                many.iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

impl ForgeCommentAdapter for ChannelForgeAdapter {
    fn post_comment(
        &self,
        channel: Option<&str>,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<(), String> {
        let target = self.resolve(channel)?;
        let msg = SendMessage::new(body.to_string(), format!("{repo}#{number}")).suppress_voice();
        super::bridge::run_bridged(
            async move { target.send(&msg).await.map_err(|e| e.to_string()) },
            POST_TIMEOUT,
            "forge write",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// (channel hint, repo, number, body) as received by `post_comment`.
    type RecordedCall = (Option<String>, String, u64, String);

    struct RecordingAdapter {
        calls: Mutex<Vec<RecordedCall>>,
        result: Result<(), String>,
    }

    impl ForgeCommentAdapter for RecordingAdapter {
        fn post_comment(
            &self,
            channel: Option<&str>,
            repo: &str,
            number: u64,
            body: &str,
        ) -> Result<(), String> {
            self.calls.lock().unwrap().push((
                channel.map(str::to_string),
                repo.to_string(),
                number,
                body.to_string(),
            ));
            self.result.clone()
        }
    }

    fn ctx() -> CapabilityContext {
        CapabilityContext {
            run_id: "r1".into(),
            sop_name: "s".into(),
            step_number: 1,
            sop_location: None,
        }
    }

    #[test]
    fn fail_closed_without_adapter() {
        let cap = ForgeCommentCapability::new(None);
        let out = cap
            .execute(ctx(), json!({"repo": "o/r", "number": 1, "body": "hi"}))
            .unwrap();
        assert!(!out.success);
        assert!(out.error.unwrap().contains("requires an injected"));
    }

    #[test]
    fn posts_via_adapter() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Ok(()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter.clone()));
        let out = cap
            .execute(
                ctx(),
                json!({"repo": "Nillth/hello", "number": 5, "body": "triage"}),
            )
            .unwrap();
        assert!(out.success, "expected success, got {out:?}");
        assert_eq!(out.output["posted"], true);
        assert_eq!(
            adapter.calls.lock().unwrap().as_slice(),
            &[(None, "Nillth/hello".to_string(), 5, "triage".to_string())]
        );
    }

    #[test]
    fn reads_fields_nested_under_input_key_and_passes_channel() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Ok(()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter.clone()));
        let out = cap
            .execute(
                ctx(),
                json!({"input": {"repo": "o/r", "number": 9, "body": "x", "channel": "git.main"}}),
            )
            .unwrap();
        assert!(out.success, "expected success, got {out:?}");
        assert_eq!(
            adapter.calls.lock().unwrap()[0],
            (
                Some("git.main".to_string()),
                "o/r".to_string(),
                9,
                "x".to_string()
            )
        );
    }

    #[test]
    fn missing_fields_fail() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Ok(()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter));
        assert!(
            !cap.execute(ctx(), json!({"number": 1, "body": "b"}))
                .unwrap()
                .success
        );
        assert!(
            !cap.execute(ctx(), json!({"repo": "o/r", "body": "b"}))
                .unwrap()
                .success
        );
        assert!(
            !cap.execute(ctx(), json!({"repo": "o/r", "number": 1}))
                .unwrap()
                .success
        );
    }

    #[test]
    fn adapter_failure_maps_to_capability_failure() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Err("forge said no".into()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter));
        let out = cap
            .execute(ctx(), json!({"repo": "o/r", "number": 2, "body": "b"}))
            .unwrap();
        assert!(!out.success);
        assert!(out.error.unwrap().contains("forge said no"));
    }
}
