//! `android_action` — drive the phone UI (tap, swipe, scroll, type, keys,
//! system dialogs).
//!
//! This is the one tool in the family that mutates device state, so it is
//! deliberately kept out of `default_auto_approve()`: an unlisted tool falls
//! through to `ApprovalRequirement::Prompt` under supervised autonomy
//! (`zeroclaw-runtime/src/approval/mod.rs`), which is what puts a human in
//! front of every tap. It additionally passes the autonomy and rate-limit
//! gate via `enforce_tool_operation(ToolOperation::Act, …)`.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::OnceLock;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

use super::client::AndroidBridgeClient;
use super::{tool_msg, tool_msg_with_args};

/// Actions the model may select, each mapping to one protocol op.
const ACTIONS: &[&str] = &["tap", "swipe", "scroll", "text", "key", "dialog"];
/// Scroll directions the protocol accepts.
const SCROLL_DIRECTIONS: &[&str] = &["forward", "backward", "up", "down"];
/// Keys the protocol accepts.
const KEYS: &[&str] = &["back", "home", "recents", "enter"];

/// Upper bound on a single `text` payload. The protocol caps a request line
/// at 64 KiB; staying well under that keeps room for the envelope.
const MAX_TEXT_LEN: usize = 4096;

static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

pub struct AndroidActionTool {
    client: AndroidBridgeClient,
    security: Arc<SecurityPolicy>,
}

impl AndroidActionTool {
    #[must_use]
    pub fn new(client: AndroidBridgeClient, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}

/// Coordinate bounds. Negative coordinates are never valid on a display, and
/// an absurd upper bound catches a model emitting nonsense before it reaches
/// `dispatchGesture`.
const MAX_COORD: i64 = 20_000;

fn coord(args: &Value, key: &str) -> anyhow::Result<i64> {
    let value = args.get(key).and_then(Value::as_i64).ok_or_else(|| {
        anyhow::Error::msg(tool_msg_with_args(
            "tool-android-action-error-missing-int",
            &[("param", key)],
        ))
    })?;
    if !(0..=MAX_COORD).contains(&value) {
        anyhow::bail!(tool_msg_with_args(
            "tool-android-action-error-coord-range",
            &[
                ("param", key),
                ("value", &value.to_string()),
                ("max", &MAX_COORD.to_string()),
            ],
        ));
    }
    Ok(value)
}

fn required_str<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow::Error::msg(tool_msg_with_args(
                "tool-android-action-error-missing-str",
                &[("param", key)],
            ))
        })
}

fn one_of(value: &str, allowed: &[&str], param: &str) -> anyhow::Result<()> {
    if allowed.contains(&value) {
        return Ok(());
    }
    anyhow::bail!(tool_msg_with_args(
        "tool-android-action-error-enum",
        &[
            ("param", param),
            ("value", value),
            ("allowed", &allowed.join(", ")),
        ],
    ))
}

/// Translate tool arguments into a protocol `(op, args)` pair.
///
/// Pure and fallible, so argument validation is testable without a bridge.
pub(crate) fn build_request(args: &Value) -> anyhow::Result<(&'static str, Value)> {
    let action = required_str(args, "action")?;
    one_of(action, ACTIONS, "action")?;

    match action {
        "tap" => {
            // Contract: `{x, y}` OR `{text}`. Prefer explicit coordinates.
            if args.get("x").is_some() || args.get("y").is_some() {
                let x = coord(args, "x")?;
                let y = coord(args, "y")?;
                Ok(("tap", json!({ "x": x, "y": y })))
            } else {
                let text = required_str(args, "text")?;
                Ok(("tap", json!({ "text": text })))
            }
        }
        "swipe" => {
            let x1 = coord(args, "x1")?;
            let y1 = coord(args, "y1")?;
            let x2 = coord(args, "x2")?;
            let y2 = coord(args, "y2")?;
            let duration = args
                .get("duration_ms")
                .and_then(Value::as_i64)
                .unwrap_or(300)
                .clamp(1, 60_000);
            Ok((
                "swipe",
                json!({ "x1": x1, "y1": y1, "x2": x2, "y2": y2, "duration_ms": duration }),
            ))
        }
        "scroll" => {
            let direction = required_str(args, "direction")?;
            one_of(direction, SCROLL_DIRECTIONS, "direction")?;
            let mut payload = json!({ "direction": direction });
            // `x`/`y` are optional hints at the scrollable to target.
            if args.get("x").is_some() && args.get("y").is_some() {
                payload["x"] = json!(coord(args, "x")?);
                payload["y"] = json!(coord(args, "y")?);
            }
            Ok(("scroll", payload))
        }
        "text" => {
            let text = args.get("text").and_then(Value::as_str).ok_or_else(|| {
                anyhow::Error::msg(tool_msg_with_args(
                    "tool-android-action-error-missing-str",
                    &[("param", "text")],
                ))
            })?;
            if text.len() > MAX_TEXT_LEN {
                anyhow::bail!(tool_msg_with_args(
                    "tool-android-action-error-text-too-long",
                    &[
                        ("bytes", &text.len().to_string()),
                        ("max", &MAX_TEXT_LEN.to_string()),
                    ],
                ));
            }
            Ok(("text", json!({ "text": text })))
        }
        "key" => {
            let key = required_str(args, "key")?;
            one_of(key, KEYS, "key")?;
            Ok(("key", json!({ "key": key })))
        }
        "dialog" => {
            let button = required_str(args, "button")?;
            Ok(("dialog", json!({ "button": button })))
        }
        // `one_of` above already rejected anything outside ACTIONS.
        other => anyhow::bail!(tool_msg_with_args(
            "tool-android-action-error-enum",
            &[
                ("param", "action"),
                ("value", other),
                ("allowed", &ACTIONS.join(", ")),
            ],
        )),
    }
}

/// Human-readable one-line description of what an action call will do.
///
/// The approval prompt itself is rendered centrally by
/// `zeroclaw-runtime`'s `approval::summarize_args`, which sees the raw
/// arguments; this mirrors that summary into the tool's own success output so
/// the transcript records what was actually dispatched.
pub(crate) fn describe(action: &str, payload: &Value) -> String {
    match action {
        "tap" => payload.get("text").and_then(Value::as_str).map_or_else(
            || {
                tool_msg_with_args(
                    "tool-android-action-did-tap-xy",
                    &[
                        ("x", &payload["x"].to_string()),
                        ("y", &payload["y"].to_string()),
                    ],
                )
            },
            |text| tool_msg_with_args("tool-android-action-did-tap-text", &[("text", text)]),
        ),
        "swipe" => tool_msg_with_args(
            "tool-android-action-did-swipe",
            &[
                ("x1", &payload["x1"].to_string()),
                ("y1", &payload["y1"].to_string()),
                ("x2", &payload["x2"].to_string()),
                ("y2", &payload["y2"].to_string()),
            ],
        ),
        "scroll" => tool_msg_with_args(
            "tool-android-action-did-scroll",
            &[(
                "direction",
                payload["direction"].as_str().unwrap_or_default(),
            )],
        ),
        "text" => tool_msg_with_args(
            "tool-android-action-did-text",
            &[(
                "chars",
                &payload["text"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .count()
                    .to_string(),
            )],
        ),
        "key" => tool_msg_with_args(
            "tool-android-action-did-key",
            &[("key", payload["key"].as_str().unwrap_or_default())],
        ),
        "dialog" => tool_msg_with_args(
            "tool-android-action-did-dialog",
            &[("button", payload["button"].as_str().unwrap_or_default())],
        ),
        other => other.to_string(),
    }
}

#[async_trait]
impl Tool for AndroidActionTool {
    fn name(&self) -> &str {
        "android_action"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| tool_msg("tool-android-action"))
            .as_str()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ACTIONS,
                    "description": tool_msg("tool-android-action-param-action")
                },
                "x": { "type": "integer", "description": tool_msg("tool-android-action-param-x") },
                "y": { "type": "integer", "description": tool_msg("tool-android-action-param-y") },
                "x1": { "type": "integer", "description": tool_msg("tool-android-action-param-x1") },
                "y1": { "type": "integer", "description": tool_msg("tool-android-action-param-y1") },
                "x2": { "type": "integer", "description": tool_msg("tool-android-action-param-x2") },
                "y2": { "type": "integer", "description": tool_msg("tool-android-action-param-y2") },
                "duration_ms": {
                    "type": "integer",
                    "description": tool_msg("tool-android-action-param-duration")
                },
                "direction": {
                    "type": "string",
                    "enum": SCROLL_DIRECTIONS,
                    "description": tool_msg("tool-android-action-param-direction")
                },
                "text": {
                    "type": "string",
                    "description": tool_msg("tool-android-action-param-text")
                },
                "key": {
                    "type": "string",
                    "enum": KEYS,
                    "description": tool_msg("tool-android-action-param-key")
                },
                "button": {
                    "type": "string",
                    "description": tool_msg("tool-android-action-param-button")
                },
                "expect_package": {
                    "type": "string",
                    "description": tool_msg("tool-android-action-param-expect-package")
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "android_action")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let (op, payload) = build_request(&args)?;

        // Acting in the wrong app is the failure that actually hurts: a tap meant for one app
        // lands in whatever drifted to the foreground, and the model never learns it moved. When
        // the caller says which app it believes it is driving, verify before touching the screen.
        if let Some(expected) = args.get("expect_package").and_then(Value::as_str)
            && let Err(error) = self.client.assert_foreground(expected).await
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error.to_string()),
            });
        }

        self.client.call(op, payload.clone()).await?;

        Ok(ToolResult {
            success: true,
            output: describe(op, &payload).into(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_prefers_coordinates() {
        let (op, payload) = build_request(&json!({ "action": "tap", "x": 540, "y": 160 })).unwrap();
        assert_eq!(op, "tap");
        assert_eq!(payload, json!({ "x": 540, "y": 160 }));
    }

    #[test]
    fn tap_falls_back_to_text() {
        let (op, payload) = build_request(&json!({ "action": "tap", "text": "Submit" })).unwrap();
        assert_eq!(op, "tap");
        assert_eq!(payload, json!({ "text": "Submit" }));
    }

    #[test]
    fn swipe_defaults_duration_to_protocol_default() {
        let (op, payload) =
            build_request(&json!({ "action": "swipe", "x1": 1, "y1": 2, "x2": 3, "y2": 4 }))
                .unwrap();
        assert_eq!(op, "swipe");
        assert_eq!(payload["duration_ms"], 300);
    }

    #[test]
    fn scroll_rejects_unknown_direction() {
        let err = build_request(&json!({ "action": "scroll", "direction": "sideways" }))
            .expect_err("unknown direction rejected");
        assert!(err.to_string().contains("sideways"), "got: {err}");
    }

    #[test]
    fn scroll_omits_coordinates_when_not_supplied() {
        let (_, payload) =
            build_request(&json!({ "action": "scroll", "direction": "forward" })).unwrap();
        assert_eq!(payload, json!({ "direction": "forward" }));
    }

    #[test]
    fn key_rejects_unknown_key() {
        let err = build_request(&json!({ "action": "key", "key": "power" }))
            .expect_err("unknown key rejected");
        assert!(err.to_string().contains("power"), "got: {err}");
    }

    #[test]
    fn key_accepts_protocol_keys() {
        for key in KEYS {
            let (op, payload) = build_request(&json!({ "action": "key", "key": key })).unwrap();
            assert_eq!(op, "key");
            assert_eq!(payload["key"], *key);
        }
    }

    #[test]
    fn schema_offers_expect_package() {
        let tool = AndroidActionTool::new(
            AndroidBridgeClient::new("/nonexistent/ui.sock"),
            Arc::new(SecurityPolicy::default()),
        );
        let schema = tool.parameters_schema();
        assert!(
            schema["properties"].get("expect_package").is_some(),
            "expect_package must be offered, or the model cannot state which app it is driving"
        );
    }

    #[test]
    fn rejects_unknown_action() {
        let err =
            build_request(&json!({ "action": "reboot" })).expect_err("unknown action rejected");
        assert!(err.to_string().contains("reboot"), "got: {err}");
    }

    #[test]
    fn rejects_negative_coordinates() {
        let err = build_request(&json!({ "action": "tap", "x": -5, "y": 10 }))
            .expect_err("negative coordinate rejected");
        assert!(err.to_string().contains("-5"), "got: {err}");
    }

    #[test]
    fn rejects_oversized_text() {
        let long = "a".repeat(MAX_TEXT_LEN + 1);
        let err = build_request(&json!({ "action": "text", "text": long }))
            .expect_err("oversized text rejected");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn text_allows_empty_string() {
        // Clearing a field is a legitimate ACTION_SET_TEXT payload.
        let (op, payload) = build_request(&json!({ "action": "text", "text": "" })).unwrap();
        assert_eq!(op, "text");
        assert_eq!(payload["text"], "");
    }

    #[test]
    fn dialog_requires_a_button() {
        build_request(&json!({ "action": "dialog" })).expect_err("missing button rejected");
        let (op, payload) =
            build_request(&json!({ "action": "dialog", "button": "Allow" })).unwrap();
        assert_eq!(op, "dialog");
        assert_eq!(payload["button"], "Allow");
    }

    #[test]
    fn describe_mentions_the_dispatched_action() {
        let (op, payload) = build_request(&json!({ "action": "tap", "x": 10, "y": 20 })).unwrap();
        let summary = describe(op, &payload);
        assert!(
            summary.contains("10") && summary.contains("20"),
            "got: {summary}"
        );
    }
}
