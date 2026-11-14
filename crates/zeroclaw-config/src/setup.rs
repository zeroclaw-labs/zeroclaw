//! Required-field setup metadata derived from the current typed configuration.

use std::collections::{HashMap, VecDeque};

use crate::schema::{Config, TtsProviderConfig};
use crate::traits::{ConfigFieldSetup, PropFieldInfo};

pub(crate) struct RouteRequiredFields {
    pub hint: ConfigFieldSetup,
    pub model_ref: ConfigFieldSetup,
}

impl RouteRequiredFields {
    fn into_fields(self) -> [(&'static str, ConfigFieldSetup); 2] {
        [("hint", self.hint), ("model_ref", self.model_ref)]
    }
}

/// The route validation contract, shared with field introspection.
pub(crate) fn route_required_fields(hint: &str, model_ref: &str) -> RouteRequiredFields {
    let required = |value: &str| ConfigFieldSetup {
        missing: value.trim().is_empty(),
    };
    RouteRequiredFields {
        hint: required(hint),
        model_ref: required(model_ref),
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
    let mut add_route_requirements = |section: &str, index: usize, hint: &str, model_ref: &str| {
        // Match the natural-key Vec traversal in Configurable exactly:
        // aliases retain whitespace and dots, and only an empty hint
        // uses the synthetic unnamed path. The hint itself is hidden
        // from the per-entry editor and is managed by alias rename.
        let alias = if hint.is_empty() {
            format!("<unnamed-{index}>")
        } else {
            hint.to_string()
        };
        for (name, setup) in route_required_fields(hint, model_ref).into_fields() {
            if name != "hint" {
                requirements
                    .entry(format!("{section}.{alias}.{name}"))
                    .or_default()
                    .push_back(setup);
            }
        }
    };
    for (index, route) in config.model_routes.iter().enumerate() {
        add_route_requirements("model_routes", index, &route.hint, &route.model_ref);
    }
    for (index, route) in config.embedding_routes.iter().enumerate() {
        let alias = if route.hint.is_empty() {
            format!("<unnamed-{index}>")
        } else {
            route.hint.clone()
        };
        for (name, setup) in [
            (
                "hint",
                ConfigFieldSetup {
                    missing: route.hint.trim().is_empty(),
                },
            ),
            (
                "model_provider",
                ConfigFieldSetup {
                    missing: route.model_provider.trim().is_empty(),
                },
            ),
        ] {
            if name != "hint" {
                requirements
                    .entry(format!("embedding_routes.{alias}.{name}"))
                    .or_default()
                    .push_back(setup);
            }
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
    use crate::schema::{EmbeddingRouteConfig, ModelRouteConfig};
    use crate::traits::ConfigFieldEntry;

    fn field_entries(config: &Config) -> Vec<ConfigFieldEntry> {
        config
            .prop_fields()
            .into_iter()
            .map(|field| ConfigFieldEntry::from_prop_field(field, false))
            .collect()
    }

    #[test]
    fn canonical_provider_secrets_use_the_registry_editor_surface() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "providers": {
                "connections": {"speech.prod": {"kind":"openai", "auth": {
                    "mode":"api_key", "api_key":"synthetic-tts-key"
                }}},
                "models": {"voice.en": {"capability":"tts", "provider":"speech.prod", "model":"tts-1"}}
            }
        })).unwrap();
        let document = crate::provider_registry::editor::snapshot(&config).unwrap();
        assert!(!document.to_string().contains("synthetic-tts-key"));
        assert!(document["models"].get("voice.en").is_some());
        assert!(config.providers.tts.is_empty());
    }

    #[test]
    fn route_setup_follows_natural_key_paths_and_each_typed_entry() {
        let mut config = Config::default();
        for (hint, model_ref) in [
            ("reasoning.deep", " \t "),
            ("reasoning.deep", "chat.default"),
            ("", ""),
            (" spaced ", "chat.default"),
        ] {
            config.model_routes.push(ModelRouteConfig {
                hint: hint.into(),
                model_ref: model_ref.into(),
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
            missing("model_routes.reasoning.deep.model_ref"),
            [true, false]
        );
        assert_eq!(missing("model_routes.<unnamed-2>.model_ref"), [true]);
        assert_eq!(missing("model_routes. spaced .model_ref"), [false]);
        assert_eq!(
            missing("embedding_routes.semantic.v2.model_provider"),
            [false]
        );
        assert!(
            fields
                .iter()
                .filter(|field| field.path == "embedding_routes.semantic.v2.model")
                .all(|field| field.setup.is_none())
        );
        assert!(
            fields
                .iter()
                .filter(|field| field.path.ends_with(".api_key"))
                .all(|field| field.setup.is_none())
        );
        assert!(!fields.iter().any(|field| field.path.ends_with(".hint")));
    }

    #[test]
    fn model_route_requires_a_canonical_chat_model() {
        let mut config = Config::default();
        config.model_routes.push(ModelRouteConfig {
            hint: "reasoning".into(),
            model_ref: "chat.prod.v2".into(),
        });
        let error = config.validate().unwrap_err();
        assert_eq!(
            error.downcast_ref::<ConfigApiError>().unwrap().code,
            ConfigApiCode::DanglingReference
        );
        crate::migration::bind_test_route_model(&mut config, "chat.prod.v2", "reasoning", None);
        config.validate().unwrap();
        config.providers.models.insert_canonical_model("embedding.prod".into(), serde_json::from_value(serde_json::json!({"capability":"embedding", "provider":"test.chat.prod.v2", "model":"embed"})).unwrap());
        config.model_routes[0].model_ref = "embedding.prod".into();
        let error = config.validate().unwrap_err();
        assert_eq!(
            error.downcast_ref::<ConfigApiError>().unwrap().code,
            ConfigApiCode::InvalidFormat
        );
    }

    #[test]
    fn embedding_route_validation_uses_exact_models_and_preserves_error_order() {
        for (hint, reference, code, leaf) in [
            (" ", "", ConfigApiCode::RequiredFieldEmpty, "hint"),
            (
                "route",
                " ",
                ConfigApiCode::RequiredFieldEmpty,
                "model_provider",
            ),
            (
                "route",
                "openai",
                ConfigApiCode::DanglingReference,
                "model_provider",
            ),
            (
                "route",
                "missing.model",
                ConfigApiCode::DanglingReference,
                "model_provider",
            ),
            (
                "route",
                "chat.default",
                ConfigApiCode::InvalidFormat,
                "model_provider",
            ),
        ] {
            let mut config: Config = serde_json::from_value(serde_json::json!({
                "providers": {"connections":{"gateway":{"kind":"openai"}},
                    "models":{"chat.default":{"capability":"chat", "provider":"gateway", "model":"chat"},
                        "embedding.default":{"capability":"embedding", "provider":"gateway", "model":"embed"}}}
            })).unwrap();
            config.embedding_routes.push(EmbeddingRouteConfig {
                hint: hint.into(),
                model_provider: reference.into(),
                ..Default::default()
            });
            let error = config.validate().unwrap_err();
            let error = error.downcast_ref::<ConfigApiError>().unwrap();
            assert_eq!(error.code, code);
            assert_eq!(
                error.path.as_deref(),
                Some(format!("embedding_routes[0].{leaf}").as_str())
            );
            config.embedding_routes[0].hint = "semantic.v1".into();
            config.embedding_routes[0].model_provider = "embedding.default".into();
            config.validate().unwrap();
        }
    }
}
