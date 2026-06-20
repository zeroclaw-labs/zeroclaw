use zeroclaw_plugin_sdk::bindings::tool::exports::zeroclaw::plugin::{plugin_info, tool};

struct Component;

impl tool::Guest for Component {
    fn name() -> String {
        "echo".to_string()
    }

    fn description() -> String {
        "Echoes its input argument back as the tool output.".to_string()
    }

    fn parameters_schema() -> String {
        r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#
            .to_string()
    }

    fn execute(args: String) -> Result<tool::ToolResult, String> {
        Ok(tool::ToolResult {
            success: true,
            output: args,
            error: None,
        })
    }
}

impl plugin_info::Guest for Component {
    fn plugin_name() -> String {
        "tool-echo".to_string()
    }

    fn plugin_version() -> String {
        "0.1.0".to_string()
    }
}

zeroclaw_plugin_sdk::bindings::tool::export!(Component with_types_in zeroclaw_plugin_sdk::bindings::tool);
