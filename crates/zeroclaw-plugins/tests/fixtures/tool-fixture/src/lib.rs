//! Minimal tool component used by the plugin-host integration tests.
//!
//! The fixture consumes the host's typed-config contract the way a real tool
//! plugin is expected to after `config_schema` enforcement: the operator's
//! canonical values are a string map, the host materializes them against the
//! manifest schema, and the guest receives `__config` as an object of *already
//! typed* JSON — a real boolean and a real number, never `"true"` or `"5"`.
//!
//! The guest therefore does no string parsing at all. It deserializes straight
//! into a typed struct with `deny_unknown_fields`, which is what makes the
//! host's work observable: if the host ever regressed to injecting the raw
//! string map, or injected a key the schema does not declare, this fixture
//! fails the call instead of silently coercing.
//!
//! Every observable is deterministic so the host tests can assert exact bytes:
//!
//! ```text
//! label=<string>|uppercase=<bool>|max_len=<u32>|keys=<count>|text=<transformed>
//! ```
//!
//! `keys` is the number of entries the host actually injected, so a test can
//! tell "the section was withheld" (`keys=0`) from "the section was empty".

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};

    /// Value used for the `label` string when the host injected no section.
    const LABEL_DEFAULT: &str = "unset";

    /// The typed view of this plugin's `[config_schema]`: one string, one
    /// integer, one boolean, all optional. `deny_unknown_fields` mirrors the
    /// schema's `additionalProperties: false` from the guest side.
    #[derive(serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct FixtureConfig {
        label: String,
        max_len: u32,
        uppercase: bool,
    }

    impl Default for FixtureConfig {
        fn default() -> Self {
            Self {
                label: LABEL_DEFAULT.to_string(),
                max_len: 0,
                uppercase: false,
            }
        }
    }

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
            "config-echo".to_string()
        }

        fn description() -> String {
            "Echo the caller's text after applying the plugin's typed config.".to_string()
        }

        fn parameters_schema() -> String {
            // Not closed with `additionalProperties: false`: the host injects
            // its own reserved `__config` key into these arguments, so a closed
            // parameter schema would describe a shape the host never sends.
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"text":{"type":"string","description":"Text to echo back."}},"required":["text"]}"#
                .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let parsed: serde_json::Value =
                serde_json::from_str(&args).map_err(|e| format!("args are not valid JSON: {e}"))?;
            let object = parsed
                .as_object()
                .ok_or_else(|| "args must be a JSON object".to_string())?;

            let text = match object.get("text") {
                None => String::new(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(_) => return Err("`text` must be a string".to_string()),
            };

            let injected = match object.get("__config") {
                None => serde_json::Map::new(),
                Some(serde_json::Value::Object(map)) => map.clone(),
                Some(_) => return Err("`__config` must be a JSON object".to_string()),
            };
            let keys = injected.len();

            // No string parsing: the host owns materialization, so a value that
            // is not already the declared JSON type is a host-side regression.
            let config: FixtureConfig = serde_json::from_value(serde_json::Value::Object(injected))
                .map_err(|e| format!("__config did not arrive as typed values: {e}"))?;

            let mut rendered = if config.uppercase {
                text.to_uppercase()
            } else {
                text
            };
            if config.max_len > 0 {
                rendered = rendered.chars().take(config.max_len as usize).collect();
            }

            Ok(ToolResult {
                success: true,
                output: format!(
                    "label={}|uppercase={}|max_len={}|keys={keys}|text={rendered}",
                    config.label, config.uppercase, config.max_len
                ),
                error: None,
            })
        }
    }

    export!(FixtureTool);
}
