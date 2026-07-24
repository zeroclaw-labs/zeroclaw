//! Minimal tool component used by the plugin-host integration tests.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::secrets::{SecretError, get as secret_get};
    use zeroclaw::plugin::state::{StateError, get as state_get, put as state_put};

    struct FixtureTool;

    impl PluginInfo for FixtureTool {
        fn plugin_name() -> String {
            "tool-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Tool for FixtureTool {
        fn name() -> String {
            match (secret_get("api_token"), state_get("fixture-state")) {
                (Err(SecretError::Unavailable), Err(StateError::Unavailable)) => {
                    "scoped-secret-check".to_string()
                }
                _ => "metadata-secret-gate-failed".to_string(),
            }
        }

        fn description() -> String {
            "Checks scoped host config".to_string()
        }

        fn parameters_schema() -> String {
            r#"{"type":"object","additionalProperties":false}"#.to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let args: serde_json::Value = serde_json::from_str(&args)
                .map_err(|_| "expected tool arguments object".to_string())?;
            let public = args
                .get("__config")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| "expected public config".to_string())?;
            let binding = public
                .get("binding_label")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "expected binding_label config".to_string())?;
            if public.len() != 1 {
                return Err("expected only public config".to_string());
            }
            if !matches!(secret_get("binding_label"), Err(SecretError::NotFound)) {
                return Err("public property was exposed as a secret".to_string());
            }
            let token = secret_get("api_token")
                .map_err(|_| "expected scoped api_token secret".to_string())?;
            if token != format!("token-{binding}") {
                return Err("received a secret from another binding".to_string());
            }

            if binding == "state-denied" {
                if !matches!(state_get("fixture-state"), Err(StateError::AccessDenied))
                    || !matches!(
                        state_put("fixture-state", b"denied", None),
                        Err(StateError::AccessDenied)
                    )
                {
                    return Err("state permissions were not enforced".to_string());
                }
            } else {
                let current = state_get("fixture-state")
                    .map_err(|_| "expected scoped durable state".to_string())?;
                let expected = current.as_ref().map(|entry| entry.revision);
                let next = current.map_or(1, |entry| entry.revision + 1);
                let value = format!("{binding}:{next}");
                let revision = state_put("fixture-state", value.as_bytes(), expected)
                    .map_err(|_| "expected durable state write".to_string())?;
                if revision != next {
                    return Err("unexpected durable state revision".to_string());
                }
            }

            Ok(ToolResult {
                success: true,
                output: binding.to_string(),
                error: None,
            })
        }
    }

    export!(FixtureTool);
}
