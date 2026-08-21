//! `android_ui_read` — read the on-screen accessibility node tree.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::OnceLock;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

use super::client::AndroidBridgeClient;
use super::{tool_msg, tool_msg_with_args};

/// Maximum rendered text handed back to the model. Mirrors the byte cap in
/// `crate::screenshot` (`MAX_BASE64_BYTES`) so a pathological UI tree cannot
/// blow out the context window.
const MAX_OUTPUT_BYTES: usize = 2_097_152;

/// Default accessibility-tree depth, matching the protocol default.
const DEFAULT_MAX_DEPTH: u64 = 15;
const MAX_MAX_DEPTH: u64 = 50;

static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

pub struct AndroidUiReadTool {
    client: AndroidBridgeClient,
}

impl AndroidUiReadTool {
    #[must_use]
    pub fn new(client: AndroidBridgeClient) -> Self {
        Self { client }
    }
}

/// Render one node as a single compact line.
fn render_node(node: &Value) -> String {
    let mut line = String::new();

    if let Some(class) = node.get("class").and_then(Value::as_str) {
        // `android.widget.Button` → `Button`; the package prefix is noise.
        let short = class.rsplit('.').next().unwrap_or(class);
        let _ = write!(line, "{short}");
    } else {
        line.push_str("Node");
    }

    if let Some(text) = node
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        let _ = write!(line, " \"{text}\"");
    }
    if let Some(desc) = node
        .get("desc")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty())
    {
        let _ = write!(line, " desc=\"{desc}\"");
    }
    if let Some(id) = node
        .get("resource_id")
        .and_then(Value::as_str)
        .filter(|i| !i.is_empty())
    {
        let _ = write!(line, " id={id}");
    }

    // The precomputed tap point is the single most useful field for the
    // model, because `android_action` takes coordinates.
    if let Some(center) = node.get("center") {
        let x = center.get("x").and_then(Value::as_i64).unwrap_or(0);
        let y = center.get("y").and_then(Value::as_i64).unwrap_or(0);
        let _ = write!(line, " center=({x},{y})");
    }

    if node.get("clickable").and_then(Value::as_bool) == Some(true) {
        line.push_str(" [clickable]");
    }
    if node.get("editable").and_then(Value::as_bool) == Some(true) {
        line.push_str(" [editable]");
    }

    line
}

/// Render a successful `read` response.
///
/// Split from the socket path so the capping and formatting are testable
/// against a synthetic bridge response.
pub(crate) fn render_read(data: &Value) -> ToolResult {
    let mut text = String::new();

    let package = data
        .get("foreground_package")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let _ = writeln!(
        text,
        "{}",
        tool_msg_with_args("tool-android-ui-read-foreground", &[("package", package)])
    );

    if data.get("system_dialog").and_then(Value::as_bool) == Some(true) {
        let kind = data
            .pointer("/dialog/kind")
            .and_then(Value::as_str)
            .unwrap_or("");
        let buttons: Vec<&str> = data
            .pointer("/dialog/buttons")
            .and_then(Value::as_array)
            .map(|b| b.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let _ = writeln!(
            text,
            "{}",
            tool_msg_with_args(
                "tool-android-ui-read-dialog",
                &[("kind", kind), ("buttons", &buttons.join(", "))],
            )
        );
    }

    let nodes = data
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let _ = writeln!(
        text,
        "{}",
        tool_msg_with_args(
            "tool-android-ui-read-node-count",
            &[("count", &nodes.len().to_string())],
        )
    );

    let mut truncated_at: Option<usize> = None;
    for (index, node) in nodes.iter().enumerate() {
        let line = format!("{index}: {}\n", render_node(node));
        if text.len() + line.len() > MAX_OUTPUT_BYTES {
            truncated_at = Some(index);
            break;
        }
        text.push_str(&line);
    }

    if let Some(index) = truncated_at {
        let _ = writeln!(
            text,
            "{}",
            tool_msg_with_args(
                "tool-android-ui-read-truncated",
                &[
                    ("shown", &index.to_string()),
                    ("total", &nodes.len().to_string()),
                ],
            )
        );
    }

    ToolResult {
        success: true,
        output: ToolOutput::json_with_text(data.clone(), text),
        error: None,
    }
}

#[async_trait]
impl Tool for AndroidUiReadTool {
    fn name(&self) -> &str {
        "android_ui_read"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| tool_msg("tool-android-ui-read"))
            .as_str()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_depth": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_MAX_DEPTH,
                    "description": tool_msg("tool-android-ui-read-param-max-depth")
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let max_depth = args
            .get("max_depth")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MAX_DEPTH, |d| d.clamp(1, MAX_MAX_DEPTH));

        let data = self
            .client
            .call("read", json!({ "max_depth": max_depth }))
            .await?;
        Ok(render_read(&data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "foreground_package": "com.android.settings",
            "system_dialog": false,
            "nodes": [
                {
                    "text": "Wi-Fi",
                    "class": "android.widget.TextView",
                    "resource_id": "com.android.settings:id/title",
                    "clickable": true,
                    "editable": false,
                    "bounds": { "l": 0, "t": 100, "r": 1080, "b": 220 },
                    "center": { "x": 540, "y": 160 }
                }
            ]
        })
    }

    #[test]
    fn renders_foreground_package_and_nodes() {
        let result = render_read(&sample());
        assert!(result.success);
        let output = result.output.as_str();
        assert!(output.contains("com.android.settings"), "got: {output}");
        assert!(output.contains("Wi-Fi"), "got: {output}");
        // The precomputed tap point must survive; it is what android_action consumes.
        assert!(output.contains("center=(540,160)"), "got: {output}");
        assert!(output.contains("[clickable]"), "got: {output}");
    }

    #[test]
    fn attaches_raw_node_list_as_structured_data() {
        let result = render_read(&sample());
        let data = result.output.data().expect("structured data attached");
        assert_eq!(
            data["nodes"][0]["resource_id"],
            "com.android.settings:id/title"
        );
    }

    #[test]
    fn surfaces_system_dialog() {
        let data = json!({
            "foreground_package": "com.android.permissioncontroller",
            "system_dialog": true,
            "dialog": { "kind": "permission", "buttons": ["Allow", "Deny"] },
            "nodes": []
        });
        let output = render_read(&data).output.as_str().to_string();
        assert!(output.contains("permission"), "got: {output}");
        assert!(output.contains("Allow"), "got: {output}");
    }

    /// A pathological tree must be capped rather than handed to the model whole.
    #[test]
    fn caps_output_at_the_byte_limit() {
        let node = json!({
            "text": "x".repeat(512),
            "class": "android.widget.TextView",
            "clickable": false,
            "editable": false,
            "center": { "x": 1, "y": 1 }
        });
        let nodes: Vec<Value> = std::iter::repeat_n(node, 20_000).collect();
        let data = json!({ "foreground_package": "com.example", "nodes": nodes });

        let output = render_read(&data).output.as_str().to_string();
        assert!(
            output.len() <= MAX_OUTPUT_BYTES + 512,
            "output must stay near the cap, got {} bytes",
            output.len()
        );
        assert!(output.contains("20000"), "should report the true total");
    }

    #[test]
    fn handles_missing_nodes_field() {
        let result = render_read(&json!({ "foreground_package": "com.example" }));
        assert!(result.success);
    }
}
