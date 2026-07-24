//! `forge.comment` deterministic SOP capability.
//!
//! Posts a comment to a git-forge issue/PR as the work product of a SOP step —
//! the write-back primitive a triage SOP uses after a human approval gate clears.
//!
//! ## Why a deterministic capability and not an agent tool or MCP server
//!
//! This is a first-party forge-WRITE authority, so the placement is deliberate:
//!
//! - **Not an agent tool.** An agent tool is invoked by a live model turn under
//!   the agent's tool-approval policy — the model decides when to write. Here the
//!   write must be a fixed, replayable pipeline step that fires ONLY after a
//!   human clears the checkpoint gate; the authorizing decision is the SOP
//!   approval (ledger-audited via the broker chokepoint), never model judgement.
//!   A deterministic capability is the only shape that runs headlessly after an
//!   out-of-band approval with no agent loop in the path.
//! - **Not an MCP server.** MCP would put the forge credential and the write
//!   behind a separate process/transport with its own trust surface; instead
//!   this reuses the already-configured git [`Channel`]'s outbound path (the
//!   same credential and provider dialect the channel already holds), so the
//!   write authority is exactly the channel the operator configured — nothing
//!   wider.
//!
//! Like `shell.exec` / `notify.channel`, this is **fail-closed** until a real
//! [`ForgeCommentAdapter`] is injected at engine-build time (the daemon supplies
//! [`ChannelForgeAdapter`] over its channel map; CLI / offline paths leave it
//! `None`, so the capability reports a clear failure instead of a silent no-op).
//! It also fails closed on a missing or ambiguous target channel (see
//! [`ChannelForgeAdapter::post_comment`]), so it can never post to the wrong
//! forge. Because it runs on the deterministic executor it can execute
//! headlessly after an out-of-band approval, without a live agent turn.
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

/// Soft alert boundary for one forge write. If the channel send has not finished
/// by this point, the bridge keeps waiting for the eventual result instead of
/// letting a public mutation continue in the background while the SOP takes
/// `on_failure`.
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

pub(crate) struct ForgeCommentTarget<'a> {
    pub channel: Option<&'a str>,
    pub repo: &'a str,
    pub number: u64,
    pub body: &'a str,
}

pub(crate) fn resolve_forge_comment_target(
    input: &Value,
) -> std::result::Result<ForgeCommentTarget<'_>, String> {
    // The step's static capability_input is merged with the piped value under
    // the `input` key. Accept repo/number/body field-by-field from nested input
    // with top-level fallback. The channel route still comes only from the static
    // top-level input, but an approved checkpoint may echo the same channel under
    // `input.channel`; that echo is accepted only when it exactly matches the
    // static channel.
    let nested_input = input.get("input").filter(|v| v.is_object());
    let channel = input
        .get("channel")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty());
    if let Some(nested_channel) = nested_input
        .and_then(|nested| nested.get("channel"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        if channel != Some(nested_channel) {
            return Err(
                "forge.comment: field 'input.channel' must match top-level 'channel' in capability_input"
                    .to_string(),
            );
        }
    } else if nested_input
        .and_then(Value::as_object)
        .is_some_and(|nested| nested.contains_key("channel"))
    {
        return Err(
            "forge.comment: field 'input.channel' must be a non-empty string matching top-level 'channel'"
                .to_string(),
        );
    }

    let repo = string_field(nested_input, input, "repo", true)
        .ok_or_else(|| "forge.comment: missing string field 'repo' (owner/repo)".to_string())?;
    let number = number_field(nested_input, input, "number")
        .ok_or_else(|| "forge.comment: missing integer field 'number'".to_string())?;
    let body = string_field(nested_input, input, "body", false)
        .ok_or_else(|| "forge.comment: missing non-empty string field 'body'".to_string())?;

    Ok(ForgeCommentTarget {
        channel,
        repo,
        number,
        body,
    })
}

fn string_field<'a>(
    nested: Option<&'a Value>,
    input: &'a Value,
    key: &str,
    trim_output: bool,
) -> Option<&'a str> {
    if let Some(value) = nested
        .and_then(|nested| nested.get(key))
        .and_then(Value::as_str)
        .and_then(|value| nonempty_string(value, trim_output))
    {
        return Some(value);
    }
    input
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| nonempty_string(value, trim_output))
}

fn nonempty_string(value: &str, trim_output: bool) -> Option<&str> {
    if value.trim().is_empty() {
        None
    } else if trim_output {
        Some(value.trim())
    } else {
        Some(value)
    }
}

fn number_field(nested: Option<&Value>, input: &Value, key: &str) -> Option<u64> {
    nested
        .and_then(|nested| nested.get(key))
        .and_then(Value::as_u64)
        .or_else(|| input.get(key).and_then(Value::as_u64))
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
            // No `required` here: repo/number/body may sit at the top level OR nested
            // under `input` (a step with static `capability_input` receives the piped
            // payload under that key), while `channel` is trusted only from the static
            // top-level input. `execute` enforces the real contract with field-precise
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

        let target = match resolve_forge_comment_target(&input) {
            Ok(target) => target,
            Err(error) => return Ok(CapabilityResult::failure(error)),
        };

        match adapter.post_comment(target.channel, target.repo, target.number, target.body) {
            Ok(()) => Ok(CapabilityResult::success(json!({
                "posted": true,
                "repo": target.repo,
                "number": target.number,
            }))),
            Err(e) => Ok(CapabilityResult::failure(format!(
                "forge.comment: post to {}#{} failed: {e}",
                target.repo, target.number
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
/// result. The timeout is a soft alert boundary: if the channel send is still in
/// flight at that point, the bridge joins it and returns the eventual result
/// instead of letting a public comment continue in the background while the SOP
/// takes `on_failure`. The sync->async bridge runs the send on a DEDICATED
/// thread with its own small runtime (see `run_bridged_to_completion`) rather
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

    fn is_git_channel_key(key: &str) -> bool {
        key == "git" || key.starts_with("git.")
    }

    /// Resolve which channel to post through: an explicit `git.<alias>` key, or the
    /// unique git channel in the map. Ambiguity and absence are hard errors so a
    /// multi-forge daemon never posts to the wrong host silently.
    fn resolve(&self, channel: Option<&str>) -> Result<Arc<dyn Channel>, String> {
        if let Some(key) = channel {
            if !Self::is_git_channel_key(key) {
                return Err(format!(
                    "channel '{key}' is not a git channel (expected 'git' or 'git.<alias>')"
                ));
            }
            return self
                .channels
                .get(key)
                .cloned()
                .ok_or_else(|| format!("channel '{key}' is not a configured channel"));
        }
        let mut git_keys: Vec<&String> = self
            .channels
            .keys()
            .filter(|k| Self::is_git_channel_key(k))
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

    fn post_comment_with_timeout(
        &self,
        channel: Option<&str>,
        repo: &str,
        number: u64,
        body: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let target = self.resolve(channel)?;
        let msg = SendMessage::new(body.to_string(), format!("{repo}#{number}")).suppress_voice();
        super::bridge::run_bridged_to_completion(
            async move { target.send(&msg).await.map_err(|e| e.to_string()) },
            timeout,
            "forge write",
        )
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
        self.post_comment_with_timeout(channel, repo, number, body, POST_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

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

    struct SlowChannel {
        sends: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl zeroclaw_api::attribution::Attributable for SlowChannel {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }

        fn alias(&self) -> &str {
            "slow"
        }
    }

    #[async_trait::async_trait]
    impl Channel for SlowChannel {
        fn name(&self) -> &str {
            "slow"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            tokio::time::sleep(self.delay).await;
            self.sends.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
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
            .execute(ctx(), json!({"repo": "o/r", "number": 5, "body": "triage"}))
            .unwrap();
        assert!(out.success, "expected success, got {out:?}");
        assert_eq!(out.output["posted"], true);
        assert_eq!(
            adapter.calls.lock().unwrap().as_slice(),
            &[(None, "o/r".to_string(), 5, "triage".to_string())]
        );
    }

    #[test]
    fn reads_fields_nested_under_input_key_and_top_level_channel() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Ok(()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter.clone()));
        let out = cap
            .execute(
                ctx(),
                json!({
                    "channel": "git.main",
                    "input": {"repo": "o/r", "number": 9, "body": "x", "channel": "git.main"},
                }),
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
    fn reads_mixed_nested_and_top_level_fields_field_by_field() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Ok(()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter.clone()));
        let out = cap
            .execute(
                ctx(),
                json!({
                    "channel": "git.main",
                    "repo": "o/r",
                    "number": 9,
                    "input": {"body": "mixed payload"},
                }),
            )
            .unwrap();
        assert!(out.success, "expected success, got {out:?}");
        assert_eq!(
            adapter.calls.lock().unwrap()[0],
            (
                Some("git.main".to_string()),
                "o/r".to_string(),
                9,
                "mixed payload".to_string()
            )
        );
    }

    #[test]
    fn rejects_channel_from_nested_piped_input() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Ok(()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter.clone()));
        let out = cap
            .execute(
                ctx(),
                json!({
                    "input": {
                        "repo": "o/r",
                        "number": 9,
                        "body": "x",
                        "channel": "discord.ops",
                    },
                }),
            )
            .unwrap();

        assert!(
            !out.success,
            "expected nested channel rejection, got {out:?}"
        );
        assert!(
            out.error
                .as_deref()
                .is_some_and(|e| e.contains("input.channel")),
            "expected field-specific error, got {out:?}"
        );
        assert!(
            adapter.calls.lock().unwrap().is_empty(),
            "nested channel input must fail before any forge adapter call"
        );
    }

    #[test]
    fn rejects_nested_channel_that_differs_from_static_channel() {
        let adapter = Arc::new(RecordingAdapter {
            calls: Mutex::new(Vec::new()),
            result: Ok(()),
        });
        let cap = ForgeCommentCapability::new(Some(adapter.clone()));
        let out = cap
            .execute(
                ctx(),
                json!({
                    "channel": "git.main",
                    "input": {
                        "repo": "o/r",
                        "number": 9,
                        "body": "x",
                        "channel": "git.admin",
                    },
                }),
            )
            .unwrap();

        assert!(
            !out.success,
            "expected mismatched nested channel rejection, got {out:?}"
        );
        assert!(
            out.error
                .as_deref()
                .is_some_and(|e| e.contains("input.channel")),
            "expected field-specific error, got {out:?}"
        );
        assert!(
            adapter.calls.lock().unwrap().is_empty(),
            "mismatched nested channel input must fail before any forge adapter call"
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

    #[test]
    fn channel_forge_rejects_explicit_non_git_channel() {
        let sends = Arc::new(AtomicUsize::new(0));
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert(
            "discord.ops".to_string(),
            Arc::new(SlowChannel {
                sends: Arc::clone(&sends),
                delay: Duration::from_millis(0),
            }),
        );
        let adapter = ChannelForgeAdapter::new(channels);

        let err = match adapter.resolve(Some("discord.ops")) {
            Ok(_) => panic!("expected explicit non-git channel to be rejected"),
            Err(err) => err,
        };

        assert!(
            err.contains("not a git channel"),
            "expected git-only channel rejection, got {err}"
        );
        assert_eq!(
            sends.load(Ordering::SeqCst),
            0,
            "non-git channel rejection must happen before send"
        );
    }

    #[test]
    fn channel_forge_waits_for_late_send_before_reporting() {
        let sends = Arc::new(AtomicUsize::new(0));
        let mut channels: HashMap<String, Arc<dyn Channel>> = HashMap::new();
        channels.insert(
            "git".to_string(),
            Arc::new(SlowChannel {
                sends: Arc::clone(&sends),
                delay: Duration::from_millis(75),
            }),
        );
        let adapter = ChannelForgeAdapter::new(channels);
        let started = Instant::now();

        let out = adapter.post_comment_with_timeout(
            None,
            "example/hello",
            5,
            "triage",
            Duration::from_millis(10),
        );

        assert_eq!(out, Ok(()));
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert_eq!(
            sends.load(Ordering::SeqCst),
            1,
            "the forge send must finish before the capability can proceed"
        );
    }
}
