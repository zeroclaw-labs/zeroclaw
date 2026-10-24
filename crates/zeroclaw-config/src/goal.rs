//! Dormant Goal Mode configuration. Runtime admission is introduced later.

use serde::{Deserialize, Serialize};
use zeroclaw_macros::Configurable;

use crate::providers::ModelProviderRef;

/// Normalized limits used by Goal Mode. `None` means unlimited in a dimension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoalBudgetLimits {
    pub token_limit: Option<u64>,
    pub cost_limit_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalConfigError {
    MissingVerifierProvider,
    TokenLimitOutOfRange,
    InvalidCostLimit,
    EmptyVerifierModel,
}

/// The existing model-provider profile selected for the mandatory verifier.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GoalVerifierConfig {
    /// Existing model-provider profile used by the mandatory Goal verifier.
    // Inline string schema: with a `$ref`, the generated config reference
    // would show the shared `ModelProviderRef` text, not this description.
    #[serde(default)]
    #[cfg_attr(feature = "schema-export", schemars(with = "String"))]
    pub model_provider: ModelProviderRef,
    /// Optional model override for the selected verifier provider profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Default-closed Goal Mode configuration. Explicit zero defaults mean
/// unlimited. When the `[goal]` section is present, both defaults are required
/// so Goal Mode cannot acquire a hidden hard-coded budget.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "goal"]
pub struct GoalConfig {
    /// Allow Goal Mode admission. Disabled by default.
    #[serde(default)]
    pub enabled: bool,
    /// Default Goal token limit; zero means unlimited. Required, with `default_cost_limit_usd`, whenever the `[goal]` section is present.
    pub default_token_limit: u64,
    /// Default Goal cost limit in US dollars; zero means unlimited. Required, with `default_token_limit`, whenever the `[goal]` section is present.
    pub default_cost_limit_usd: f64,
    #[serde(default)]
    #[nested]
    pub verifier: GoalVerifierConfig,
}

impl GoalConfig {
    /// Validate values locally. `Config::validate` additionally checks that a
    /// configured verifier reference resolves through `providers.models`.
    pub fn validate(&self) -> Result<(), GoalConfigError> {
        if self.default_token_limit > i64::MAX as u64 {
            return Err(GoalConfigError::TokenLimitOutOfRange);
        }
        if !self.default_cost_limit_usd.is_finite() || self.default_cost_limit_usd < 0.0 {
            return Err(GoalConfigError::InvalidCostLimit);
        }
        if self
            .verifier
            .model
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(GoalConfigError::EmptyVerifierModel);
        }
        if self.enabled && self.verifier.model_provider.as_str().trim().is_empty() {
            return Err(GoalConfigError::MissingVerifierProvider);
        }
        Ok(())
    }

    /// Normalize explicit zero configuration defaults to unlimited.
    pub fn effective_limits(&self) -> Result<GoalBudgetLimits, GoalConfigError> {
        self.validate()?;
        Ok(GoalBudgetLimits {
            token_limit: (self.default_token_limit != 0).then_some(self.default_token_limit),
            cost_limit_usd: (self.default_cost_limit_usd != 0.0)
                .then_some(self.default_cost_limit_usd),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_error::{ConfigApiCode, ConfigApiError};
    use crate::schema::Config;

    #[test]
    fn goal_field_descriptions_reach_config_list_entries() {
        let fields = Config::default().prop_fields();
        for (path, expected) in [
            (
                "goal.enabled",
                "Allow Goal Mode admission. Disabled by default.",
            ),
            (
                "goal.default_token_limit",
                "Default Goal token limit; zero means unlimited. Required, with `default_cost_limit_usd`, whenever the `[goal]` section is present.",
            ),
            (
                "goal.default_cost_limit_usd",
                "Default Goal cost limit in US dollars; zero means unlimited. Required, with `default_token_limit`, whenever the `[goal]` section is present.",
            ),
            (
                "goal.verifier.model_provider",
                "Existing model-provider profile used by the mandatory Goal verifier.",
            ),
            (
                "goal.verifier.model",
                "Optional model override for the selected verifier provider profile.",
            ),
        ] {
            let field = fields.iter().find(|field| field.name == path).unwrap();
            let entry = crate::traits::ConfigFieldEntry::from_prop_field(field.clone(), false);
            let wire = serde_json::to_value(entry).unwrap();
            assert_eq!(wire["description"], expected, "description for {path}");
            if path == "goal.verifier.model_provider" {
                assert_eq!(wire["kind"], "alias_ref");
                assert_eq!(wire["alias_source"], "model_providers");
                assert_eq!(wire["type_hint"], "ModelProviderRef");
            }
        }
    }

    #[cfg(feature = "schema-export")]
    #[test]
    fn goal_field_descriptions_reach_generated_reference() {
        fn field_row<'a>(reference: &'a str, section: &str, key: &str) -> Option<&'a str> {
            let heading = format!("`{section}`");
            let prefix = format!("| `{key}`");
            let mut lines = reference.lines();
            lines.find(|line| {
                line.starts_with('#') && line.trim_start_matches('#').trim() == heading
            })?;
            lines
                .take_while(|line| !line.starts_with('#'))
                .find(|line| line.starts_with(&prefix))
        }

        let schema = schemars::schema_for!(Config);
        let reference = crate::schema_markdown::generate(&schema.to_value());
        let mut misses = Vec::new();
        for (section, key, expected) in [
            (
                "goal",
                "enabled",
                "Allow Goal Mode admission. Disabled by default.",
            ),
            (
                "goal",
                "default_token_limit",
                "Default Goal token limit; zero means unlimited. Required, with `default_cost_limit_usd`, whenever the `[goal]` section is present.",
            ),
            (
                "goal",
                "default_cost_limit_usd",
                "Default Goal cost limit in US dollars; zero means unlimited. Required, with `default_token_limit`, whenever the `[goal]` section is present.",
            ),
            (
                "goal.verifier",
                "model_provider",
                "Existing model-provider profile used by the mandatory Goal verifier.",
            ),
            (
                "goal.verifier",
                "model",
                "Optional model override for the selected verifier provider profile.",
            ),
        ] {
            let row = field_row(&reference, section, key);
            if !row.is_some_and(|row| row.ends_with(&format!(" | {expected} |"))) {
                misses.push(format!("{section}.{key}: {}", row.unwrap_or("no row")));
            }
        }
        assert!(
            misses.is_empty(),
            "description misses:\n{}",
            misses.join("\n")
        );
    }

    #[test]
    fn enabled_config_requires_a_verifier() {
        let mut config = GoalConfig {
            enabled: true,
            ..GoalConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(GoalConfigError::MissingVerifierProvider)
        );
        config.verifier.model_provider = ModelProviderRef::new("openai.default");
        assert_eq!(
            config.effective_limits(),
            Ok(GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: None,
            })
        );
    }

    #[test]
    fn an_explicit_goal_section_requires_both_budget_defaults() {
        let missing_token_limit = toml::from_str::<GoalConfig>(
            r#"
                enabled = true
                default_cost_limit_usd = 0.0
            "#,
        );
        assert!(missing_token_limit.is_err());

        let missing_cost_limit = toml::from_str::<GoalConfig>(
            r#"
                enabled = true
                default_token_limit = 0
            "#,
        );
        assert!(missing_cost_limit.is_err());
    }

    #[test]
    fn rejects_malformed_defaults_even_while_disabled() {
        assert_eq!(
            GoalConfig {
                default_cost_limit_usd: -1.0,
                ..GoalConfig::default()
            }
            .validate(),
            Err(GoalConfigError::InvalidCostLimit)
        );
        assert_eq!(
            GoalConfig {
                default_token_limit: i64::MAX as u64 + 1,
                ..GoalConfig::default()
            }
            .validate(),
            Err(GoalConfigError::TokenLimitOutOfRange)
        );
    }

    #[test]
    fn enabled_verifier_must_resolve_through_existing_provider_config() {
        let valid: Config = toml::from_str(
            r#"
                [goal]
                enabled = true
                default_token_limit = 0
                default_cost_limit_usd = 0.0

                [goal.verifier]
                model_provider = "openai.default"

                [providers.models.openai.default]
                model = "gpt-test"
            "#,
        )
        .expect("configuration parses");
        assert!(valid.validate().is_ok());

        let unresolved: Config = toml::from_str(
            r#"
                [goal]
                enabled = true
                default_token_limit = 0
                default_cost_limit_usd = 0.0

                [goal.verifier]
                model_provider = "openai.missing"
            "#,
        )
        .expect("configuration parses");
        let error = ConfigApiError::from_validation(
            unresolved
                .validate()
                .expect_err("unresolved verifier must fail"),
        );
        assert_eq!(error.code, ConfigApiCode::DanglingReference);
        assert_eq!(error.path.as_deref(), Some("goal.verifier.model_provider"));
        assert!(error.message.contains("no matching providers.models entry"));
    }

    #[test]
    fn goal_validation_projects_typed_field_errors_at_config_boundary() {
        fn assert_error(config: Config, code: ConfigApiCode, path: &str, message: &str) {
            let error = ConfigApiError::from_validation(
                config
                    .validate()
                    .expect_err("invalid Goal config must fail"),
            );
            assert_eq!(error.code, code);
            assert_eq!(error.path.as_deref(), Some(path));
            assert_eq!(error.message, message);
            assert!(!error.message.contains("MissingVerifierProvider"));
            assert!(!error.message.contains("TokenLimitOutOfRange"));
            assert!(!error.message.contains("InvalidCostLimit"));
            assert!(!error.message.contains("EmptyVerifierModel"));
        }

        let mut config = Config::default();
        config.goal.enabled = true;
        assert_error(
            config,
            ConfigApiCode::RequiredFieldEmpty,
            "goal.verifier.model_provider",
            "goal.verifier.model_provider is required when goal.enabled is true",
        );

        let mut config = Config::default();
        config.goal.default_token_limit = i64::MAX as u64 + 1;
        assert_error(
            config,
            ConfigApiCode::InvalidNumericRange,
            "goal.default_token_limit",
            &format!(
                "goal.default_token_limit must be less than or equal to {}",
                i64::MAX
            ),
        );

        let mut config = Config::default();
        config.goal.default_cost_limit_usd = f64::NAN;
        assert_error(
            config,
            ConfigApiCode::InvalidNumericRange,
            "goal.default_cost_limit_usd",
            "goal.default_cost_limit_usd must be finite and non-negative",
        );

        let mut config = Config::default();
        config.goal.verifier.model = Some(" \t ".into());
        assert_error(
            config,
            ConfigApiCode::RequiredFieldEmpty,
            "goal.verifier.model",
            "goal.verifier.model must not be empty when provided",
        );

        for (reference, expected) in [
            (
                "openai",
                "goal.verifier.model_provider must use `<type>.<alias>` format (got \"openai\")",
            ),
            (
                ".default",
                "goal.verifier.model_provider must use non-empty `<type>.<alias>` components (got \".default\")",
            ),
            (
                "openai.",
                "goal.verifier.model_provider must use non-empty `<type>.<alias>` components (got \"openai.\")",
            ),
        ] {
            let mut config = Config::default();
            config.goal.enabled = true;
            config.goal.verifier.model_provider = ModelProviderRef::new(reference);
            assert_error(
                config,
                ConfigApiCode::InvalidFormat,
                "goal.verifier.model_provider",
                expected,
            );
        }

        let mut config = Config::default();
        config.goal.enabled = true;
        config.goal.verifier.model_provider = ModelProviderRef::new("openai.missing");
        assert_error(
            config,
            ConfigApiCode::DanglingReference,
            "goal.verifier.model_provider",
            "goal.verifier.model_provider = \"openai.missing\" but no matching providers.models entry is configured",
        );
    }

    #[test]
    fn goal_validation_preserves_valid_and_disabled_config_behavior() {
        let disabled = Config::default();
        assert!(disabled.validate().is_ok());

        let mut disabled_with_unresolved_verifier = Config::default();
        disabled_with_unresolved_verifier
            .goal
            .verifier
            .model_provider = ModelProviderRef::new("openai.missing");
        assert!(disabled_with_unresolved_verifier.validate().is_ok());

        let mut enabled = Config::default();
        enabled.goal.enabled = true;
        enabled.goal.verifier.model_provider = ModelProviderRef::new("openai.default");
        enabled
            .providers
            .models
            .openai
            .insert("default".into(), Default::default());
        assert!(enabled.validate().is_ok());
    }

    #[test]
    fn toml_round_trip_preserves_explicit_unlimited_defaults() {
        let source = r#"
            enabled = true
            default_token_limit = 0
            default_cost_limit_usd = 0.0

            [verifier]
            model_provider = "openai.default"
            model = "gpt-test"
        "#;
        let config: GoalConfig = toml::from_str(source).expect("Goal config parses");
        assert_eq!(config.default_token_limit, 0);
        assert_eq!(config.default_cost_limit_usd, 0.0);
        assert_eq!(
            config.effective_limits(),
            Ok(GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: None,
            })
        );

        let encoded = toml::to_string(&config).expect("Goal config serializes");
        let decoded: GoalConfig = toml::from_str(&encoded).expect("serialized Goal config parses");
        assert_eq!(decoded.default_token_limit, 0);
        assert_eq!(decoded.default_cost_limit_usd, 0.0);
        assert_eq!(decoded.verifier.model_provider.as_str(), "openai.default");
        assert_eq!(decoded.verifier.model.as_deref(), Some("gpt-test"));
    }
}
