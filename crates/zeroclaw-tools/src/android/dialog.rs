//! `android_dialog` — confirm or deny a privileged Android system dialog.
//!
//! Kept separate from ordinary UI gestures so a profile that pre-approves
//! `android_action` cannot silently inherit permission/install confirmation.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

use super::client::AndroidBridgeClient;
use super::{tool_msg, tool_msg_with_args};

const BUTTONS: &[&str] = &["allow", "deny"];
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

pub struct AndroidDialogTool {
    client: AndroidBridgeClient,
    security: Arc<SecurityPolicy>,
}

impl AndroidDialogTool {
    #[must_use]
    pub fn new(client: AndroidBridgeClient, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}

fn build_request(args: &Value) -> anyhow::Result<Value> {
    let button = args
        .get("button")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| BUTTONS.contains(value))
        .ok_or_else(|| anyhow::Error::msg(tool_msg("tool-android-dialog-error-button")))?;
    let expected = args
        .get("expect_package")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::Error::msg(tool_msg("tool-android-dialog-error-package")))?;
    Ok(json!({ "button": button, "expect_package": expected }))
}

#[async_trait]
impl Tool for AndroidDialogTool {
    fn name(&self) -> &str {
        "android_dialog"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| tool_msg("tool-android-dialog"))
            .as_str()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "button": {
                    "type": "string",
                    "enum": BUTTONS,
                    "description": tool_msg("tool-android-dialog-param-button")
                },
                "expect_package": {
                    "type": "string",
                    "description": tool_msg("tool-android-dialog-param-expect-package")
                }
            },
            "required": ["button", "expect_package"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "android_dialog")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }
        let payload = build_request(&args)?;
        let expected = payload["expect_package"].as_str().unwrap_or_default();
        self.client.assert_foreground(expected).await?;
        self.client.call("dialog", payload.clone()).await?;
        Ok(ToolResult {
            success: true,
            output: tool_msg_with_args(
                "tool-android-dialog-did-click",
                &[("button", payload["button"].as_str().unwrap_or_default())],
            )
            .into(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_closed_button_set_with_target() {
        let payload = build_request(&json!({
            "button": "allow",
            "expect_package": "com.android.permissioncontroller"
        }))
        .unwrap();
        assert_eq!(payload["button"], "allow");
        assert_eq!(
            payload["expect_package"],
            "com.android.permissioncontroller"
        );
    }

    #[test]
    fn rejects_arbitrary_labels_and_missing_target() {
        assert!(
            build_request(&json!({
                "button": "Install anyway",
                "expect_package": "com.android.packageinstaller"
            }))
            .is_err()
        );
        assert!(build_request(&json!({ "button": "deny" })).is_err());
    }
}
