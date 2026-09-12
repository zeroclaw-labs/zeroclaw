//! Tool component that exercises the host's **scoped secret service**.
//!
//! This is deliberately a separate crate from `tests/fixtures/tool-fixture`.
//! That fixture is the typed-config echo component asserted byte-for-byte by
//! `tests/reference_plugin.rs` and by the runtime crate's
//! `plugin_live_config` regression, so it cannot also carry the secret
//! probes below: a component exports exactly one `tool` world, and the two
//! suites need different `name()` values, different `config_schema` shapes,
//! and different `execute` outputs.
//!
//! What this component proves, from the guest side:
//!
//! * `name()` runs during *metadata*, where the secret service must be
//!   closed. It reports `metadata-secret-gate-failed` if a secret is ever
//!   readable there, so the gate failing open turns into a visible tool name
//!   rather than a silent leak.
//! * `execute()` receives only the **non-secret** properties in `__config`,
//!   and reaches secrets solely through the host-mediated `secrets` import.
//! * A property marked `x-secret` is withheld from `__config`, and a public
//!   property is *not* reachable through the secret import.
//! * The secret it reads is the one bound to *its own* instance, so two
//!   bindings of the same package cannot read each other's values.

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

    struct FixtureTool;

    impl PluginInfo for FixtureTool {
        fn plugin_name() -> String {
            "tool-secret-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Tool for FixtureTool {
        fn name() -> String {
            match secret_get("api_token") {
                Err(SecretError::Unavailable) => "scoped-secret-check".to_string(),
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

            Ok(ToolResult {
                success: true,
                output: binding.to_string(),
                error: None,
            })
        }
    }

    export!(FixtureTool);
}
