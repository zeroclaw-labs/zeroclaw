//! Goal Mode admission, budget, verifier, and channel prompt configuration.

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

/// Default-closed Goal Mode configuration. Budget defaults are optional: a
/// start without budget flags uses them only when at least one is positive or
/// both are explicitly `0` (unlimited); otherwise admission asks for an
/// explicit budget.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "goal"]
pub struct GoalConfig {
    /// Allow Goal Mode admission. Disabled by default.
    #[serde(default)]
    pub enabled: bool,
    /// Default Goal token limit for `/goal start` without budget flags; `0` means explicitly unlimited. Without budget flags, a start needs a positive token or cost default, or both defaults set to `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_token_limit: Option<u64>,
    /// Default Goal cost limit in US dollars for `/goal start` without budget flags; `0` means explicitly unlimited. Without budget flags, a start needs a positive token or cost default, or both defaults set to `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cost_limit_usd: Option<f64>,
    /// Include ordinary channel turn context in the system message of Matrix Goal parent turns. Defaults to `true`; `false` narrows ordinary prompt reuse by omitting that context. ZeroCode and verifier turns are unchanged. Resolved once per parent turn; reload affects the next parent turn. Including changing context may affect provider prompt-cache reuse.
    #[serde(default = "default_true")]
    pub include_channel_turn_context: bool,
    #[serde(default)]
    #[nested]
    pub verifier: GoalVerifierConfig,
}

fn default_true() -> bool {
    true
}

impl Default for GoalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_token_limit: None,
            default_cost_limit_usd: None,
            include_channel_turn_context: true,
            verifier: GoalVerifierConfig::default(),
        }
    }
}

impl GoalConfig {
    /// Validate values locally. `Config::validate` additionally checks that a
    /// configured verifier reference resolves through `providers.models`.
    pub fn validate(&self) -> Result<(), GoalConfigError> {
        if self
            .default_token_limit
            .is_some_and(|limit| limit > i64::MAX as u64)
        {
            return Err(GoalConfigError::TokenLimitOutOfRange);
        }
        if self
            .default_cost_limit_usd
            .is_some_and(|cost| !cost.is_finite() || cost < 0.0)
        {
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

    /// Resolve the configured defaults for a start without budget flags.
    /// Never treats an unset default as unlimited.
    pub fn default_budget(&self) -> Result<GoalDefaultBudget, GoalConfigError> {
        self.validate()?;
        let token_limit = self.default_token_limit.filter(|limit| *limit > 0);
        let cost_limit_usd = self.default_cost_limit_usd.filter(|cost| *cost > 0.0);
        let explicitly_unlimited =
            self.default_token_limit == Some(0) && self.default_cost_limit_usd == Some(0.0);
        if token_limit.is_some() || cost_limit_usd.is_some() || explicitly_unlimited {
            Ok(GoalDefaultBudget::Configured(GoalBudgetLimits {
                token_limit,
                cost_limit_usd,
            }))
        } else {
            Ok(GoalDefaultBudget::Unconfigured)
        }
    }
}

/// Outcome of resolving configured defaults for a start without budget flags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GoalDefaultBudget {
    /// Defaults select at least one positive limit, or both are explicitly `0` (unlimited). `None` in a dimension means no limit there.
    Configured(GoalBudgetLimits),
    /// Defaults select no budget; a start without budget flags must be rejected.
    Unconfigured,
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
                "Default Goal token limit for `/goal start` without budget flags; `0` means explicitly unlimited. Without budget flags, a start needs a positive token or cost default, or both defaults set to `0`.",
            ),
            (
                "goal.default_cost_limit_usd",
                "Default Goal cost limit in US dollars for `/goal start` without budget flags; `0` means explicitly unlimited. Without budget flags, a start needs a positive token or cost default, or both defaults set to `0`.",
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
                "Default Goal token limit for `/goal start` without budget flags; `0` means explicitly unlimited. Without budget flags, a start needs a positive token or cost default, or both defaults set to `0`.",
            ),
            (
                "goal",
                "default_cost_limit_usd",
                "Default Goal cost limit in US dollars for `/goal start` without budget flags; `0` means explicitly unlimited. Without budget flags, a start needs a positive token or cost default, or both defaults set to `0`.",
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
    fn channel_turn_context_defaults_and_round_trips() {
        assert!(GoalConfig::default().include_channel_turn_context);
        assert!(Config::default().goal.include_channel_turn_context);
        assert!(
            toml::from_str::<Config>("")
                .unwrap()
                .goal
                .include_channel_turn_context
        );
        let section = "[goal]\ndefault_token_limit = 0\ndefault_cost_limit_usd = 0.0\n";
        assert!(
            toml::from_str::<Config>(section)
                .unwrap()
                .goal
                .include_channel_turn_context
        );
        for include in [false, true] {
            let config: Config = toml::from_str(&format!(
                "{section}include_channel_turn_context = {include}\n"
            ))
            .unwrap();
            assert_eq!(config.goal.include_channel_turn_context, include);
            let encoded = toml::to_string(&config).unwrap();
            let decoded: Config = toml::from_str(&encoded).unwrap();
            assert_eq!(decoded.goal.include_channel_turn_context, include);
        }
        for invalid in ["\"yes\"", "1", "\"false\""] {
            assert!(
                toml::from_str::<Config>(&format!(
                    "{section}include_channel_turn_context = {invalid}\n"
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn channel_turn_context_does_not_change_validation() {
        for enabled in [false, true] {
            let mut config: Config = toml::from_str(&format!(
                "[goal]\nenabled = {enabled}\ndefault_token_limit = 0\n\
                 default_cost_limit_usd = 0.0\n[goal.verifier]\n\
                 model_provider = \"openai.default\"\n\
                 [providers.models.openai.default]\nmodel = \"gpt-test\"\n"
            ))
            .unwrap();
            for include in [false, true] {
                config.goal.include_channel_turn_context = include;
                assert_eq!(config.goal.validate(), Ok(()));
                assert!(config.validate().is_ok());
                config.goal.default_cost_limit_usd = Some(-1.0);
                assert_eq!(
                    config.goal.validate(),
                    Err(GoalConfigError::InvalidCostLimit)
                );
                assert!(config.validate().is_err());
                config.goal.default_cost_limit_usd = Some(0.0);
            }
        }
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
        assert_eq!(config.default_budget(), Ok(GoalDefaultBudget::Unconfigured));
    }

    #[test]
    fn goal_section_budget_defaults_are_optional() {
        let enabled_only = toml::from_str::<GoalConfig>(
            r#"
                enabled = true
            "#,
        )
        .expect("enabled-only Goal section parses");
        assert_eq!(enabled_only.default_token_limit, None);
        assert_eq!(enabled_only.default_cost_limit_usd, None);

        let cost_only = toml::from_str::<GoalConfig>(
            r#"
                default_cost_limit_usd = 0.0
            "#,
        )
        .expect("single cost default parses");
        assert_eq!(cost_only.default_token_limit, None);
        assert_eq!(cost_only.default_cost_limit_usd, Some(0.0));
    }

    #[test]
    fn default_budget_requires_a_positive_default_or_explicit_unlimited() {
        let cases = [
            (None, None, GoalDefaultBudget::Unconfigured),
            (None, Some(0.0), GoalDefaultBudget::Unconfigured),
            (
                None,
                Some(2.5),
                GoalDefaultBudget::Configured(GoalBudgetLimits {
                    token_limit: None,
                    cost_limit_usd: Some(2.5),
                }),
            ),
            (Some(0), None, GoalDefaultBudget::Unconfigured),
            (
                Some(0),
                Some(0.0),
                GoalDefaultBudget::Configured(GoalBudgetLimits {
                    token_limit: None,
                    cost_limit_usd: None,
                }),
            ),
            (
                Some(0),
                Some(2.5),
                GoalDefaultBudget::Configured(GoalBudgetLimits {
                    token_limit: None,
                    cost_limit_usd: Some(2.5),
                }),
            ),
            (
                Some(10),
                None,
                GoalDefaultBudget::Configured(GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: None,
                }),
            ),
            (
                Some(10),
                Some(0.0),
                GoalDefaultBudget::Configured(GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: None,
                }),
            ),
            (
                Some(10),
                Some(2.5),
                GoalDefaultBudget::Configured(GoalBudgetLimits {
                    token_limit: Some(10),
                    cost_limit_usd: Some(2.5),
                }),
            ),
        ];
        for (token_limit, cost_limit_usd, expected) in cases {
            let config = GoalConfig {
                default_token_limit: token_limit,
                default_cost_limit_usd: cost_limit_usd,
                ..GoalConfig::default()
            };
            assert_eq!(config.default_budget(), Ok(expected));
        }
        let negative_zero = GoalConfig {
            default_token_limit: Some(0),
            default_cost_limit_usd: Some(-0.0),
            ..GoalConfig::default()
        };
        assert_eq!(
            negative_zero.default_budget(),
            Ok(GoalDefaultBudget::Configured(GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: None,
            }))
        );
    }

    #[test]
    fn rejects_malformed_defaults_even_while_disabled() {
        assert_eq!(
            GoalConfig {
                default_cost_limit_usd: Some(-1.0),
                ..GoalConfig::default()
            }
            .validate(),
            Err(GoalConfigError::InvalidCostLimit)
        );
        assert_eq!(
            GoalConfig {
                default_token_limit: Some(i64::MAX as u64 + 1),
                ..GoalConfig::default()
            }
            .validate(),
            Err(GoalConfigError::TokenLimitOutOfRange)
        );
        assert_eq!(
            GoalConfig {
                default_cost_limit_usd: Some(f64::NAN),
                ..GoalConfig::default()
            }
            .validate(),
            Err(GoalConfigError::InvalidCostLimit)
        );
        assert_eq!(
            GoalConfig {
                default_cost_limit_usd: Some(f64::INFINITY),
                ..GoalConfig::default()
            }
            .validate(),
            Err(GoalConfigError::InvalidCostLimit)
        );
        assert!(toml::from_str::<GoalConfig>("default_token_limit = -1").is_err());
        assert!(toml::from_str::<GoalConfig>("default_token_limit = 'many'").is_err());
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
        config.goal.default_token_limit = Some(i64::MAX as u64 + 1);
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
        config.goal.default_cost_limit_usd = Some(f64::NAN);
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
    fn goal_validation_runs_after_other_config_checks() {
        // A later invalid delegate setting must win over a retained Goal error.
        let mut config = Config::default();
        config.goal.enabled = true;
        config.goal.verifier.model_provider = ModelProviderRef::new("openai.missing");
        config.delegate.timeout_secs = 0;

        let error = ConfigApiError::from_validation(
            config
                .validate()
                .expect_err("invalid delegate setting must be reported first"),
        );
        assert_eq!(error.code, ConfigApiCode::InvalidNumericRange);
        assert_eq!(error.path.as_deref(), Some("delegate.timeout_secs"));

        config.delegate.timeout_secs = Config::default().delegate.timeout_secs;
        let error = ConfigApiError::from_validation(
            config
                .validate()
                .expect_err("unresolved verifier must still fail validation"),
        );
        assert_eq!(error.code, ConfigApiCode::DanglingReference);
        assert_eq!(error.path.as_deref(), Some("goal.verifier.model_provider"));
    }

    #[test]
    fn goal_validation_input_paths_follow_enablement_and_verifier_reference() {
        // Only enabled, well-formed verifier references add a provider input.
        let mut config = Config::default();
        assert_eq!(config.goal_validation_input_paths(), ["goal".to_owned()]);

        config.goal.enabled = true;
        config.goal.verifier.model_provider = ModelProviderRef::new("openai.default");
        assert_eq!(
            config.goal_validation_input_paths(),
            [
                "goal".to_owned(),
                "providers.models.openai.default".to_owned()
            ]
        );
        config.goal.verifier.model_provider = ModelProviderRef::new(" openai.default ");
        assert_eq!(
            config.goal_validation_input_paths(),
            [
                "goal".to_owned(),
                "providers.models.openai.default".to_owned()
            ]
        );

        config.goal.enabled = false;
        assert_eq!(config.goal_validation_input_paths(), ["goal".to_owned()]);

        config.goal.enabled = true;
        for reference in ["openai", ".default", "openai.", ""] {
            config.goal.verifier.model_provider = ModelProviderRef::new(reference);
            assert_eq!(
                config.goal_validation_input_paths(),
                ["goal".to_owned()],
                "reference {reference:?} must not produce a provider path"
            );
        }
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
        assert_eq!(config.default_token_limit, Some(0));
        assert_eq!(config.default_cost_limit_usd, Some(0.0));
        assert_eq!(
            config.default_budget(),
            Ok(GoalDefaultBudget::Configured(GoalBudgetLimits {
                token_limit: None,
                cost_limit_usd: None,
            }))
        );

        let encoded = toml::to_string(&config).expect("Goal config serializes");
        let decoded: GoalConfig = toml::from_str(&encoded).expect("serialized Goal config parses");
        assert_eq!(decoded.default_token_limit, Some(0));
        assert_eq!(decoded.default_cost_limit_usd, Some(0.0));
        assert_eq!(decoded.verifier.model_provider.as_str(), "openai.default");
        assert_eq!(decoded.verifier.model.as_deref(), Some("gpt-test"));
    }
}
