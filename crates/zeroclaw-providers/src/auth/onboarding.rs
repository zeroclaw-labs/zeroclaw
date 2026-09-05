//! Provider-owned credential effects for onboarding transactions.
//!
//! The daemon coordinates configuration persistence and reload admission, but
//! a provider owns how an onboarding secret becomes a stored credential and
//! how that binding is compensated. The effect intentionally hides profile
//! snapshots and provider metadata from callers.

use anyhow::{Result, bail};
use zeroclaw_config::schema::Config;

use super::{AuthService, anthropic_token};
use crate::auth::profiles::{ProfileSaveOutcome, StagedProfileBinding};

const ANTHROPIC_PROVIDER: &str = "anthropic";
const SETUP_TOKEN_AUTH_MODE: &str = "setup_token";

/// Canonical, provider-neutral credential data submitted by an onboarding
/// surface. The secret is never formatted or retained outside the provider
/// operation that persists it.
#[derive(Clone, Copy)]
pub struct OnboardingCredentialSubmission<'a> {
    provider_type: &'a str,
    alias: &'a str,
    auth_mode: Option<&'a str>,
    setup_token: Option<&'a str>,
}

/// Validate the provider-owned credential contract before a daemon begins its
/// generic configuration or filesystem transaction.
pub fn validate_onboarding_credential(
    submission: &OnboardingCredentialSubmission<'_>,
) -> Result<()> {
    if submission
        .provider_type
        .trim()
        .eq_ignore_ascii_case(ANTHROPIC_PROVIDER)
        && submission
            .auth_mode
            .is_some_and(|mode| mode.trim().eq_ignore_ascii_case(SETUP_TOKEN_AUTH_MODE))
        && submission
            .setup_token
            .is_none_or(|token| token.trim().is_empty())
    {
        bail!("setup-token authentication requires a setup token");
    }
    Ok(())
}

impl<'a> OnboardingCredentialSubmission<'a> {
    #[must_use]
    pub fn new(
        provider_type: &'a str,
        alias: &'a str,
        auth_mode: Option<&'a str>,
        setup_token: Option<&'a str>,
    ) -> Self {
        Self {
            provider_type,
            alias,
            auth_mode,
            setup_token,
        }
    }
}

impl std::fmt::Debug for OnboardingCredentialSubmission<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OnboardingCredentialSubmission")
            .field("provider_type", &self.provider_type)
            .field("alias", &self.alias)
            .field("auth_mode", &self.auth_mode)
            .field("has_setup_token", &self.setup_token.is_some())
            .finish()
    }
}

/// A staged provider credential binding. Its concrete persistence state stays
/// private so generic onboarding callers cannot acquire a provider dependency.
pub struct StagedOnboardingCredential {
    field: &'static str,
    label: &'static str,
    effect: StagedCredentialEffect,
}

enum StagedCredentialEffect {
    Anthropic {
        auth_service: AuthService,
        alias: String,
        staged: StagedProfileBinding,
    },
}

/// Provider-neutral completion state for a staged credential write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingCredentialCommit {
    Durable,
    CommittedWithDurabilityWarning {
        field: &'static str,
        label: &'static str,
        detail: String,
    },
}

impl StagedOnboardingCredential {
    /// Translate the provider's already-committed persistence outcome without
    /// exposing its profile-store types to the daemon.
    #[must_use]
    pub fn commit(&self) -> OnboardingCredentialCommit {
        match &self.effect {
            StagedCredentialEffect::Anthropic { staged, .. } => match &staged.save_outcome {
                ProfileSaveOutcome::Durable => OnboardingCredentialCommit::Durable,
                ProfileSaveOutcome::CommittedWithDurabilityWarning(error) => {
                    OnboardingCredentialCommit::CommittedWithDurabilityWarning {
                        field: self.field,
                        label: self.label,
                        detail: error.to_string(),
                    }
                }
            },
        }
    }

    /// Compensate only the credential binding staged by this effect. The
    /// compare-and-swap protection is provider-owned and refuses to overwrite
    /// a later credential change.
    pub async fn rollback(self) -> Result<()> {
        match self.effect {
            StagedCredentialEffect::Anthropic {
                auth_service,
                alias,
                staged,
            } => {
                auth_service
                    .restore_model_provider_profile(
                        ANTHROPIC_PROVIDER,
                        &alias,
                        staged.snapshot,
                        &staged.staged_profile,
                    )
                    .await
            }
        }
    }
}

/// Stage a provider-owned onboarding credential when the selected auth mode
/// requires one. Providers that do not own a stored onboarding credential
/// return `None` without changing durable state.
pub async fn stage_onboarding_credential(
    config: &Config,
    submission: OnboardingCredentialSubmission<'_>,
) -> Result<Option<StagedOnboardingCredential>> {
    if !submission
        .provider_type
        .trim()
        .eq_ignore_ascii_case(ANTHROPIC_PROVIDER)
        || !submission
            .auth_mode
            .is_some_and(|mode| mode.trim().eq_ignore_ascii_case(SETUP_TOKEN_AUTH_MODE))
    {
        return Ok(None);
    }

    validate_onboarding_credential(&submission)?;
    let token = submission.setup_token.unwrap_or("").trim();

    let auth_service = AuthService::from_config(config);
    let metadata = std::collections::HashMap::from([(
        "auth_kind".to_string(),
        anthropic_token::detect_auth_kind(token, Some("authorization"))
            .as_metadata_value()
            .to_string(),
    )]);
    let staged = auth_service
        .stage_model_provider_token(ANTHROPIC_PROVIDER, submission.alias, token, metadata)
        .await?;

    Ok(Some(StagedOnboardingCredential {
        field: "api_key",
        label: "setup credential",
        effect: StagedCredentialEffect::Anthropic {
            auth_service,
            alias: submission.alias.to_string(),
            staged,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn anthropic_setup_token_stages_same_alias_without_changing_active_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let config = Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Config::default()
        };
        let auth = AuthService::from_config(&config);
        auth.store_model_provider_token(
            ANTHROPIC_PROVIDER,
            "active",
            "existing-token",
            std::collections::HashMap::new(),
            true,
        )
        .await
        .unwrap();

        let staged = stage_onboarding_credential(
            &config,
            OnboardingCredentialSubmission::new(
                ANTHROPIC_PROVIDER,
                "subscription",
                Some(SETUP_TOKEN_AUTH_MODE),
                Some("sk-ant-oat01-synthetic"),
            ),
        )
        .await
        .unwrap()
        .expect("setup-token submission must stage a provider credential");

        assert_eq!(staged.commit(), OnboardingCredentialCommit::Durable);
        let stored = auth
            .get_profile(ANTHROPIC_PROVIDER, Some("subscription"))
            .await
            .unwrap()
            .expect("same alias profile must exist");
        assert_eq!(stored.token.as_deref(), Some("sk-ant-oat01-synthetic"));
        assert_eq!(
            stored.metadata.get("auth_kind").map(String::as_str),
            Some("authorization")
        );
        assert_eq!(
            auth.load_profiles()
                .await
                .unwrap()
                .active_profiles
                .get(ANTHROPIC_PROVIDER)
                .map(String::as_str),
            Some("anthropic:active"),
            "staging must not change the provider's active profile"
        );

        staged.rollback().await.unwrap();
        assert!(
            auth.get_profile(ANTHROPIC_PROVIDER, Some("subscription"))
                .await
                .unwrap()
                .is_none(),
            "rollback must remove the exact staged binding"
        );
    }
}
