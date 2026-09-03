//! `android_device` — read-only device facts (sensors, location, telephony).
//!
//! These come from the Android APK's platform APIs rather than the screen, so unlike the rest of
//! the family they need no accessibility service. They travel the same Unix socket regardless: a
//! loopback port is reachable by every app on the device, and the phone's location and carrier
//! identify its owner, so this is at least as sensitive as screen control.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::OnceLock;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

use super::client::AndroidBridgeClient;
use super::{tool_msg, tool_msg_with_args};

static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

/// Facts the bridge can report. Kept closed so a typo fails here with the list, rather than
/// becoming an `unsupported_op` round trip to the phone.
const FACTS: &[&str] = &["sensors", "location", "telephony"];

pub struct AndroidDeviceTool {
    client: AndroidBridgeClient,
    security: Arc<SecurityPolicy>,
}

impl AndroidDeviceTool {
    #[must_use]
    pub fn new(client: AndroidBridgeClient, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}

/// Validate and build the protocol `device` payload. Pure, so it is testable off-device.
pub(crate) fn build_request(args: &Value) -> anyhow::Result<Value> {
    let what = args
        .get("what")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .ok_or_else(|| anyhow::Error::msg(tool_msg("tool-android-device-error-missing-what")))?;

    if !FACTS.contains(&what) {
        anyhow::bail!(tool_msg_with_args(
            "tool-android-device-error-bad-what",
            &[("what", what), ("supported", &FACTS.join(", "))],
        ));
    }
    Ok(json!({ "what": what }))
}

/// Upper bound on rendered text. The sensor list on a real phone runs to dozens of entries, which
/// is worth showing but not worth spending the whole context on.
const MAX_OUTPUT_BYTES: usize = 4096;

/// Render the bridge's answer. A permission the user has not granted comes back as an `error`
/// field inside a successful response, because the call worked and the answer is "you may not
/// have this" — surface it as a tool error so it cannot be mistaken for data.
///
/// The reading itself MUST land in the text, not only in the structured `data`. The dispatcher
/// forwards a tool's text to the model and drops the structured half, so a bare confirmation like
/// "Read Android telephony." tells the model an operation happened and nothing about what it
/// found. Asked for a value it was never shown, a model invents one: this tool reported 9 emulator
/// sensors on one run and 78 Bosch sensors on the next, against a real 47.
pub(crate) fn render(what: &str, data: &Value) -> anyhow::Result<ToolResult> {
    if let Some(error) = data.get("error").and_then(Value::as_str) {
        return Ok(ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(tool_msg_with_args(
                "tool-android-device-error-refused",
                &[("what", what), ("reason", error)],
            )),
        });
    }

    let mut text = tool_msg_with_args("tool-android-device-read", &[("what", what)]);
    text.push('\n');
    let mut rendered = serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
    if rendered.len() > MAX_OUTPUT_BYTES {
        let boundary = crate::util_helpers::floor_char_boundary(&rendered, MAX_OUTPUT_BYTES);
        rendered.truncate(boundary);
        rendered.push_str("\n… truncated");
    }
    text.push_str(&rendered);

    Ok(ToolResult {
        success: true,
        output: ToolOutput::json_with_text(data.clone(), text),
        error: None,
    })
}

#[async_trait]
impl Tool for AndroidDeviceTool {
    fn name(&self) -> &str {
        "android_device"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| tool_msg("tool-android-device"))
            .as_str()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "what": {
                    "type": "string",
                    "enum": FACTS,
                    "description": tool_msg("tool-android-device-param-what")
                }
            },
            "required": ["what"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "android_device")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let payload = build_request(&args)?;
        let what = payload["what"].as_str().unwrap_or_default().to_string();
        let data = self.client.call("device", payload).await?;
        render(&what, &data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_each_supported_fact() {
        for fact in FACTS {
            let payload = build_request(&json!({ "what": fact })).unwrap();
            assert_eq!(payload["what"], json!(fact));
        }
    }

    #[test]
    fn rejects_unknown_fact() {
        let err = build_request(&json!({ "what": "contacts" }))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("contacts"),
            "the error must name what was asked for: {err}"
        );
    }

    #[test]
    fn requires_what() {
        assert!(build_request(&json!({})).is_err());
    }

    /// The dispatcher forwards only a tool's text to the model. A reading that lives solely in the
    /// structured half is invisible to it, and a model asked for an unseen value invents one.
    #[test]
    fn the_reading_itself_appears_in_the_text() {
        let data = json!({ "count": 47, "sensors": [{ "name": "lsm6dsv Accelerometer" }] });
        let result = render("sensors", &data).unwrap();
        let text = result.output.as_str();
        assert!(
            text.contains("47") && text.contains("lsm6dsv"),
            "the values must reach the model through the text, got: {text}"
        );
    }

    #[test]
    fn oversized_readings_are_truncated_not_dropped() {
        let sensors: Vec<_> = (0..900)
            .map(|i| json!({ "name": format!("sensor {i}") }))
            .collect();
        let result = render("sensors", &json!({ "sensors": sensors })).unwrap();
        let text = result.output.as_str();
        assert!(
            text.len() <= MAX_OUTPUT_BYTES + 200,
            "text should be capped, got {} bytes",
            text.len()
        );
        assert!(
            text.contains("truncated"),
            "truncation must be visible to the reader"
        );
    }

    #[test]
    fn truncates_multibyte_reading_on_a_char_boundary() {
        let data = json!({ "value": "€".repeat(MAX_OUTPUT_BYTES) });
        let serialized = serde_json::to_string_pretty(&data).unwrap();
        assert!(serialized.len() > MAX_OUTPUT_BYTES);
        assert!(
            !serialized.is_char_boundary(MAX_OUTPUT_BYTES),
            "fixture must place the byte cap inside a multibyte character"
        );

        let result = render("sensors", &data).expect("UTF-8 output truncation must not panic");

        let text = result.output.as_str();
        assert!(text.contains("truncated"));
        assert!(text.is_char_boundary(text.len()));
    }

    #[test]
    fn ungranted_permission_is_an_error_not_data() {
        let data = json!({ "error": "ACCESS_FINE_LOCATION not granted" });
        let result = render("location", &data).unwrap();
        assert!(
            !result.success,
            "a refused permission must not read as a successful location fix"
        );
        assert!(result.error.unwrap().contains("ACCESS_FINE_LOCATION"));
    }

    #[test]
    fn attaches_the_raw_reading_as_structured_data() {
        let data = json!({ "lat": 51.5, "lon": -0.12, "accuracy_m": 12.0 });
        let result = render("location", &data).unwrap();
        assert!(result.success);
        assert_eq!(
            result.output.data().expect("structured data")["lat"],
            json!(51.5)
        );
    }
}
