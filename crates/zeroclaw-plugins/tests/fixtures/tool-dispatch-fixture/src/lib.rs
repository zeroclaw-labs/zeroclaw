//! Tool component that exercises the host's `dispatch` import.
//!
//! Its parameters name one `target` agent alias and one `prompt`. The guest
//! forwards both to `zeroclaw:plugin/dispatch.wake` unchanged and reports only
//! the outcome:
//!
//! * `Ok` becomes a successful result with output `ok`.
//! * `Err` becomes a failed result whose `error` is the `Debug` form of the
//!   returned `dispatch-error`, which carries the WIT case name, so a host test
//!   can assert the exact category (`invalid-request`, `rate-limited`, ...).
//!
//! The fixture imports `dispatch` unconditionally, so a scope that was not
//! granted `agent_dispatch` cannot even instantiate it: the linker has no such
//! import to bind.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0", "plugins-wit-v0-dispatch"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::dispatch::wake;

    struct DispatchFixtureTool;

    impl PluginInfo for DispatchFixtureTool {
        fn plugin_name() -> String {
            "tool-dispatch-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Tool for DispatchFixtureTool {
        fn name() -> String {
            "dispatch_fixture".to_string()
        }

        fn description() -> String {
            "Exercises the host-mediated agent wake import".to_string()
        }

        fn parameters_schema() -> String {
            r#"{"type":"object","properties":{"target":{"type":"string"},"prompt":{"type":"string"}},"required":["target","prompt"]}"#
                .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let args: serde_json::Value =
                serde_json::from_str(&args).map_err(|_| "args-invalid".to_string())?;
            let field = |key: &str| {
                args.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("{key}-missing"))
            };
            let target = field("target")?;
            let prompt = field("prompt")?;

            // The guest adds no opinion of its own: every admission decision,
            // including whether `target` and `prompt` are well formed, belongs
            // to the host, and its category comes back across the boundary.
            match wake(&target, &prompt) {
                Ok(()) => Ok(ToolResult {
                    success: true,
                    output: "ok".to_string(),
                    error: None,
                }),
                Err(err) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("{err:?}")),
                }),
            }
        }
    }

    export!(DispatchFixtureTool);
}
