//! Required-field setup metadata derived from the current typed configuration.

use std::collections::{HashMap, VecDeque};

use crate::schema::{Config, TtsProviderConfig};
use crate::traits::{ConfigFieldSetup, PropFieldInfo};

pub(crate) struct RouteRequiredFields {
    pub hint: ConfigFieldSetup,
    pub model_provider: ConfigFieldSetup,
    pub model: ConfigFieldSetup,
}

impl RouteRequiredFields {
    fn into_fields(self) -> [(&'static str, ConfigFieldSetup); 3] {
        [
            ("hint", self.hint),
            ("model_provider", self.model_provider),
            ("model", self.model),
        ]
    }
}

/// The route validation contract, shared with field introspection.
pub(crate) fn route_required_fields(
    hint: &str,
    model_provider: &str,
    model: &str,
) -> RouteRequiredFields {
    let required = |value: &str| ConfigFieldSetup {
        missing: value.trim().is_empty(),
    };
    RouteRequiredFields {
        hint: required(hint),
        model_provider: required(model_provider),
        model: required(model),
    }
}

/// Cloud TTS constructors require a nonempty key and use its trimmed value.
/// This does not impose a key requirement on local TTS providers.
pub fn tts_api_key(config: &TtsProviderConfig) -> Option<&str> {
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
}

/// Enrich final introspection paths using typed values, including secrets that
/// have already been masked in the display representation. This is a derived
/// field requirement view, not a provider readiness or authentication check.
pub fn annotate_fields(config: &Config, fields: &mut [PropFieldInfo]) {
    let mut requirements: HashMap<String, VecDeque<ConfigFieldSetup>> = HashMap::new();
    let routes = config
        .model_routes
        .iter()
        .enumerate()
        .map(|(index, route)| {
            (
                "model_routes",
                index,
                &route.hint,
                &route.model_provider,
                &route.model,
            )
        })
        .chain(
            config
                .embedding_routes
                .iter()
                .enumerate()
                .map(|(index, route)| {
                    (
                        "embedding_routes",
                        index,
                        &route.hint,
                        &route.model_provider,
                        &route.model,
                    )
                }),
        );
    for (section, index, hint, provider, model) in routes {
        // Match the natural-key Vec traversal in Configurable exactly:
        // aliases retain whitespace and dots, and only an empty hint
        // uses the synthetic unnamed path. The hint itself is hidden
        // from the per-entry editor and is managed by alias rename.
        let alias = if hint.is_empty() {
            format!("<unnamed-{index}>")
        } else {
            hint.clone()
        };
        for (name, setup) in route_required_fields(hint, provider, model).into_fields() {
            if name != "hint" {
                requirements
                    .entry(format!("{section}.{alias}.{name}"))
                    .or_default()
                    .push_back(setup);
            }
        }
    }
    for (family, alias, provider) in config.providers.tts.iter_entries() {
        if matches!(family, "openai" | "elevenlabs" | "google") {
            requirements
                .entry(format!("providers.tts.{family}.{alias}.api_key"))
                .or_default()
                .push_back(ConfigFieldSetup {
                    missing: tts_api_key(provider).is_none(),
                });
        }
    }
    for field in fields {
        // Duplicate natural keys produce repeated field paths in traversal
        // order. Keep each row paired with its own typed route value.
        if let Some(setup) = requirements
            .get_mut(&field.name)
            .and_then(VecDeque::pop_front)
        {
            field.setup = Some(setup);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_error::{ConfigApiCode, ConfigApiError};
    use crate::schema::{EmbeddingRouteConfig, ModelRouteConfig, OpenAIModelProviderConfig};
    use crate::traits::ConfigFieldEntry;

    fn field_entries(config: &Config) -> Vec<ConfigFieldEntry> {
        config
            .prop_fields()
            .into_iter()
            .map(|field| ConfigFieldEntry::from_prop_field(field, false))
            .collect()
    }

    #[test]
    fn field_setup_serializes_typed_cloud_key_status_without_secret_values() {
        let mut config = Config::default();
        for section in [
            "providers.tts.openai",
            "providers.tts.elevenlabs",
            "providers.tts.google",
            "providers.tts.edge",
            "providers.tts.piper",
            "providers.models.openai",
        ] {
            config.create_map_key(section, "setup_test").unwrap();
            config
                .set_prop(&format!("{section}.setup_test.api_key"), " \t ")
                .unwrap();
        }
        config
            .create_map_key("providers.tts.openai", "configured")
            .unwrap();
        config
            .set_prop(
                "providers.tts.openai.configured.api_key",
                " synthetic-tts-key ",
            )
            .unwrap();
        let json = serde_json::json!({"fields": field_entries(&config)});
        let fields = json["fields"].as_array().unwrap();
        for family in ["openai", "elevenlabs", "google"] {
            let path = format!("providers.tts.{family}.setup_test.api_key");
            let field = fields.iter().find(|field| field["path"] == path).unwrap();
            // Display masking treats a whitespace secret as populated. Setup
            // status must come from the typed key contract instead.
            assert_eq!(field["populated"], true);
            assert_eq!(field["setup"]["missing"], true);
            assert!(field.get("value").is_none());
        }
        let configured = fields
            .iter()
            .find(|field| field["path"] == "providers.tts.openai.configured.api_key")
            .unwrap();
        assert_eq!(configured["setup"]["missing"], false);
        assert!(!json.to_string().contains("synthetic-tts-key"));
        for field in fields.iter().filter(|field| {
            let path = field["path"].as_str().unwrap();
            path.starts_with("providers.models.")
                || path.starts_with("providers.tts.edge.")
                || path.starts_with("providers.tts.piper.")
        }) {
            assert!(field.get("setup").is_none(), "{}", field["path"]);
        }
        // Older daemon output decodes as unknown, rather than optional or ready.
        let legacy: ConfigFieldEntry = serde_json::from_value(
            fields
                .iter()
                .find(|field| field["path"] == "providers.models.openai.setup_test.api_key")
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert!(legacy.setup.is_none());
    }

    #[test]
    fn route_setup_follows_natural_key_paths_and_each_typed_entry() {
        let mut config = Config::default();
        for (hint, provider, model) in [
            ("reasoning.deep", " \t ", "model"),
            ("reasoning.deep", "openai.default", " \n "),
            ("", "", ""),
            (" spaced ", "openai.default", "model"),
        ] {
            config.model_routes.push(ModelRouteConfig {
                hint: hint.into(),
                model_provider: provider.into(),
                model: model.into(),
                ..Default::default()
            });
        }
        config.embedding_routes.push(EmbeddingRouteConfig {
            hint: "semantic.v2".into(),
            model_provider: "openai.default".into(),
            model: "".into(),
            ..Default::default()
        });
        let fields = field_entries(&config);
        let missing = |path: &str| -> Vec<bool> {
            fields
                .iter()
                .filter(|field| field.path == path)
                .map(|field| field.setup.as_ref().unwrap().missing)
                .collect()
        };
        assert_eq!(
            missing("model_routes.reasoning.deep.model_provider"),
            [true, false]
        );
        assert_eq!(missing("model_routes.reasoning.deep.model"), [false, true]);
        assert_eq!(missing("model_routes.<unnamed-2>.model"), [true]);
        assert_eq!(missing("model_routes. spaced .model"), [false]);
        assert_eq!(missing("embedding_routes.semantic.v2.model"), [true]);
        assert!(
            fields
                .iter()
                .filter(|field| field.path.ends_with(".api_key"))
                .all(|field| field.setup.is_none())
        );
        assert!(!fields.iter().any(|field| field.path.ends_with(".hint")));
    }

    #[test]
    fn route_validation_preserves_error_order_codes_messages_and_resolution() {
        for section in ["model_routes", "embedding_routes"] {
            for (hint, provider, model, code, leaf, message) in [
                (
                    " \t ",
                    "",
                    "",
                    ConfigApiCode::RequiredFieldEmpty,
                    "hint",
                    "hint must not be empty",
                ),
                (
                    "route",
                    " \n ",
                    "",
                    ConfigApiCode::RequiredFieldEmpty,
                    "model_provider",
                    "model_provider must not be empty",
                ),
                (
                    "route",
                    "openai",
                    "",
                    ConfigApiCode::InvalidFormat,
                    "model_provider",
                    "model_provider must be dotted form `<type>.<alias>` (got \"openai\")",
                ),
                (
                    "route",
                    "openai.missing",
                    "",
                    ConfigApiCode::DanglingReference,
                    "model_provider",
                    "model_provider = \"openai.missing\" but providers.models.openai.missing is not configured",
                ),
                (
                    "route",
                    " openai.default ",
                    " \t ",
                    ConfigApiCode::RequiredFieldEmpty,
                    "model",
                    "model must not be empty",
                ),
            ] {
                let mut config = Config::default();
                config
                    .providers
                    .models
                    .openai
                    .insert("default".into(), OpenAIModelProviderConfig::default());
                if section == "model_routes" {
                    config.model_routes.push(ModelRouteConfig {
                        hint: hint.into(),
                        model_provider: provider.into(),
                        model: model.into(),
                        ..Default::default()
                    });
                } else {
                    config.embedding_routes.push(EmbeddingRouteConfig {
                        hint: hint.into(),
                        model_provider: provider.into(),
                        model: model.into(),
                        ..Default::default()
                    });
                }
                let error = config.validate().unwrap_err();
                let error = error.downcast_ref::<ConfigApiError>().unwrap();
                assert_eq!(error.code, code);
                assert_eq!(
                    error.path.as_deref(),
                    Some(format!("{section}[0].{leaf}").as_str())
                );
                assert_eq!(error.message, format!("{section}[0].{message}"));
            }
        }
    }
}
