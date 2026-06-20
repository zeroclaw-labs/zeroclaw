use zeroclaw_plugin_sdk::tool::{ToolMetadata, ToolPlugin, ToolResult, ToolResultExt};

struct Echo;

impl ToolPlugin for Echo {
    fn metadata() -> ToolMetadata {
        ToolMetadata::new("echo", "Echoes its input argument back as the tool output.")
            .parameters_schema(
                r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#,
            )
    }

    fn plugin_info() -> (&'static str, &'static str) {
        ("tool-echo", "0.1.0")
    }

    fn execute(args: String) -> Result<ToolResult, String> {
        Ok(ToolResult::ok(args))
    }
}

zeroclaw_plugin_sdk::export_tool!(Echo);
