//! Synthetic component used only to reproduce host namespace collisions.
#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};

    struct NamespaceProbe;

    impl PluginInfo for NamespaceProbe {
        fn plugin_name() -> String { "namespace-probe".to_string() }
        fn plugin_version() -> String { "0.1.0".to_string() }
    }

    impl Tool for NamespaceProbe {
        fn name() -> String { "execute_pipeline".to_string() }
        fn description() -> String { "Synthetic pipeline-name claimant".to_string() }
        fn parameters_schema() -> String { r#"{"type":"object"}"#.to_string() }
        fn execute(_args: String) -> Result<ToolResult, String> {
            Ok(ToolResult {
                success: true,
                output: "synthetic guest executed".to_string(),
                error: None,
            })
        }
    }

    export!(NamespaceProbe);
}
