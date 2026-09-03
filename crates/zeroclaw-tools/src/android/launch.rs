//! `android_launch` — start an app by package name.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::OnceLock;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

use super::client::AndroidBridgeClient;
use super::{tool_msg, tool_msg_with_args};

static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

pub struct AndroidLaunchTool {
    client: AndroidBridgeClient,
    security: Arc<SecurityPolicy>,
}

impl AndroidLaunchTool {
    #[must_use]
    pub fn new(client: AndroidBridgeClient, security: Arc<SecurityPolicy>) -> Self {
        Self { client, security }
    }
}

/// An Android package name: dot-separated segments of alphanumerics and
/// underscores. Validated here so a malformed value fails locally with a
/// clear message instead of becoming a `not_found` round trip.
fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && package.len() <= 255
        && package.contains('.')
        && package.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Build the protocol `launch` payload. Pure, so validation is testable.
pub(crate) fn build_request(args: &Value) -> anyhow::Result<Value> {
    let package = args
        .get("package")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| anyhow::Error::msg(tool_msg("tool-android-launch-error-missing-package")))?;

    if !valid_package(package) {
        anyhow::bail!(tool_msg_with_args(
            "tool-android-launch-error-bad-package",
            &[("package", package)],
        ));
    }

    let mut payload = json!({ "package": package });
    if let Some(activity) = args
        .get("activity")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        payload["activity"] = json!(activity);
    }
    Ok(payload)
}

#[async_trait]
impl Tool for AndroidLaunchTool {
    fn name(&self) -> &str {
        "android_launch"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| tool_msg("tool-android-launch"))
            .as_str()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": tool_msg("tool-android-launch-param-package")
                },
                "activity": {
                    "type": "string",
                    "description": tool_msg("tool-android-launch-param-activity")
                }
            },
            "required": ["package"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "android_launch")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let payload = build_request(&args)?;
        self.client.call("launch", payload.clone()).await?;

        Ok(ToolResult {
            success: true,
            output: tool_msg_with_args(
                "tool-android-launch-launched",
                &[("package", payload["package"].as_str().unwrap_or_default())],
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
    fn builds_package_only_payload() {
        let payload = build_request(&json!({ "package": "com.android.settings" })).unwrap();
        assert_eq!(payload, json!({ "package": "com.android.settings" }));
    }

    #[test]
    fn includes_optional_activity() {
        let payload = build_request(&json!({
            "package": "com.android.settings",
            "activity": "com.android.settings.Settings"
        }))
        .unwrap();
        assert_eq!(payload["activity"], "com.android.settings.Settings");
    }

    #[test]
    fn requires_a_package() {
        build_request(&json!({})).expect_err("missing package rejected");
        build_request(&json!({ "package": "   " })).expect_err("blank package rejected");
    }

    #[test]
    fn rejects_malformed_package_names() {
        for bad in [
            "nodots",
            "com..double",
            "com.foo/bar",
            "com.foo;rm -rf /",
            ".leading",
            "trailing.",
        ] {
            build_request(&json!({ "package": bad }))
                .unwrap_err_or_panic(&format!("expected {bad} to be rejected"));
        }
    }

    #[test]
    fn accepts_realistic_package_names() {
        for good in [
            "com.android.settings",
            "org.zerodroid.bridge",
            "com.foo_bar.baz2",
        ] {
            build_request(&json!({ "package": good }))
                .unwrap_or_else(|e| panic!("expected {good} to be accepted: {e}"));
        }
    }

    /// Small helper so the rejection loop reads clearly.
    trait ErrExpect {
        fn unwrap_err_or_panic(self, message: &str);
    }
    impl<T> ErrExpect for anyhow::Result<T> {
        fn unwrap_err_or_panic(self, message: &str) {
            assert!(self.is_err(), "{message}");
        }
    }
}
