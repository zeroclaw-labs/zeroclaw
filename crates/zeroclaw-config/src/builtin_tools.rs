//! Built-in availability, separate from execution authorization.

use serde::{Deserialize, Serialize};
use zeroclaw_macros::Configurable;

/// The approved default set. Optional selection cannot change permissions.
pub const CORE_TOOL_NAMES: &[&str] = &[
    "shell",
    "file_read",
    "file_write",
    "file_edit",
    "glob_search",
    "content_search",
    "memory_recall",
    "memory_store",
    "memory_forget",
    "web_fetch",
    "git_operations",
];

/// Native adapters for external services or separately installed applications.
pub const EXTERNAL_TOOL_NAMES: &[&str] = &[
    "browser",
    "browser_open",
    "browser_delegate",
    "screenshot",
    "text_browser",
    "pushover",
    "weather",
    "web_search_tool",
    "image_gen",
    "email_read",
    "email_search",
];

/// Model-visible built-in selection (`[tools]`). Rebuild the registry after edits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct BuiltinToolsConfig {
    /// Built-in selection: empty is minimal (eleven core tools); `"*"` is
    /// full (every compiled-in built-in permitted by its config and policy).
    /// Named entries add tools to minimal and are redundant alongside `"*"`.
    #[serde(default)]
    pub optional: Vec<String>,
}

impl BuiltinToolsConfig {
    /// Availability only: callers must still enforce policy and prerequisites.
    pub fn is_enabled(&self, name: &str) -> bool {
        CORE_TOOL_NAMES.contains(&name)
            || self
                .optional
                .iter()
                .any(|entry| entry == name || entry == "*")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_three_round_trip_preserves_optional_selection() {
        let config: crate::schema::Config = toml::from_str(
            "schema_version = 3\n[tools]\noptional = [\"calculator\", \"cron_list\"]\n",
        )
        .unwrap();
        assert_eq!(config.schema_version, 3);
        assert!(config.tools.is_enabled("calculator"));
        assert!(!config.tools.is_enabled("weather"));
        let saved = toml::to_string(&config).unwrap();
        let restored: crate::schema::Config = toml::from_str(&saved).unwrap();
        assert_eq!(restored.tools.optional, config.tools.optional);
    }

    #[test]
    fn optional_external_adapter_reports_compiled_out_in_lean_build() {
        let mut config = crate::schema::Config::default();
        config.tools.optional = vec!["weather".into()];
        let warnings = config.collect_warnings();
        let unavailable = warnings.iter().any(|warning| {
            warning.code == crate::validation_warnings::TOOL_COMPILED_OUT
                && warning.path == "tools.optional"
                && warning.message.contains("weather")
        });
        assert_eq!(unavailable, !cfg!(feature = "tools-external"));
    }

    #[test]
    fn absent_section_is_core_only_and_wildcard_is_explicit() {
        let config = BuiltinToolsConfig::default();
        assert!(CORE_TOOL_NAMES.iter().all(|name| config.is_enabled(name)));
        assert!(!config.is_enabled("cron_list"));
        assert!(!config.is_enabled("weather"));
        let compatibility = BuiltinToolsConfig {
            optional: vec!["*".into()],
        };
        assert!(compatibility.is_enabled("cron_list"));
    }
    #[test]
    fn existing_schema_three_config_defaults_to_minimal_without_migration() {
        let mut config: crate::schema::Config =
            toml::from_str("schema_version = 3\nlocale = \"en\"\n").unwrap();
        assert_eq!(config.schema_version, 3);
        assert!(config.tools.optional.is_empty());
        assert!(!config.tools.is_enabled("calculator"));
        assert!(!config.plugins.enabled);
        assert!(!config.plugins.auto_discover);
        config.set_prop("tools.optional", "[\"*\"]").unwrap();
        assert!(config.tools.is_enabled("calculator"));
        let saved = toml::to_string(&config).unwrap();
        let restored: crate::schema::Config = toml::from_str(&saved).unwrap();
        assert_eq!(restored.schema_version, 3);
        assert_eq!(restored.tools.optional, ["*"]);
        assert!(!restored.plugins.enabled);
        assert!(!restored.plugins.auto_discover);
    }

    #[test]
    fn full_wildcard_reports_each_compiled_out_external_adapter() {
        let mut config = crate::schema::Config::default();
        config.tools.optional = vec!["*".into(), "calculator".into(), "weather".into()];
        let warnings: Vec<_> = config
            .collect_warnings()
            .into_iter()
            .filter(|warning| {
                warning.code == crate::validation_warnings::TOOL_COMPILED_OUT
                    && warning.path == "tools.optional"
            })
            .collect();
        if cfg!(feature = "tools-external") {
            assert!(warnings.is_empty());
        } else {
            assert_eq!(warnings.len(), EXTERNAL_TOOL_NAMES.len());
            for name in EXTERNAL_TOOL_NAMES {
                assert_eq!(
                    warnings
                        .iter()
                        .filter(|warning| {
                            warning.message.starts_with(&format!("{name} was selected"))
                                && warning.message.contains("tools-compat")
                                && warning.message.contains("cannot add compiled-out code")
                        })
                        .count(),
                    1,
                    "missing or duplicated diagnostic for {name}"
                );
            }
        }
    }
}
