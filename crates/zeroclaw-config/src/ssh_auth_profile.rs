//! Operator-defined authentication policy for a single SSH destination.

use crate::schema::{ScopedHttpCredential, SshCredentialDestination};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeRole {
    Username,
    Password,
    Totp,
    PasswordThenTotp,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PamChallenge {
    pub label: String,
    pub role: ChallengeRole,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SshAuthentication {
    PublicKey {
        key_alias: String,
    },
    Password {
        password_alias: String,
    },
    Pam {
        password_alias: Option<String>,
        totp_alias: Option<String>,
        challenges: Vec<PamChallenge>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshAuthProfile {
    pub destination: SshCredentialDestination,
    pub authentication: SshAuthentication,
}

impl SshAuthProfile {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.destination.is_valid(), "Invalid SSH destination");
        let valid_alias = ScopedHttpCredential::valid_name;
        match &self.authentication {
            SshAuthentication::PublicKey { key_alias } => {
                ensure!(valid_alias(key_alias), "Invalid SSH key alias");
            }
            SshAuthentication::Password { password_alias } => {
                ensure!(valid_alias(password_alias), "Invalid password alias");
            }
            SshAuthentication::Pam {
                password_alias,
                totp_alias,
                challenges,
            } => {
                ensure!(
                    password_alias
                        .iter()
                        .chain(totp_alias)
                        .all(|alias| valid_alias(alias)),
                    "Invalid SSH factor alias"
                );
                ensure!(
                    password_alias.is_none() || password_alias != totp_alias,
                    "SSH factors must use separate aliases"
                );
                ensure!(
                    (1..=3).contains(&challenges.len()),
                    "Invalid PAM challenge count"
                );
                let mut labels = HashSet::new();
                let mut roles = HashSet::new();
                for challenge in challenges {
                    ensure!(
                        !challenge.label.is_empty()
                            && challenge.label.len() <= 128
                            && challenge.label == challenge.label.trim()
                            && !challenge.label.chars().any(char::is_control)
                            && labels.insert(challenge.label.as_str())
                            && roles.insert(challenge.role),
                        "Invalid or duplicate PAM challenge"
                    );
                }
                let combined = roles.contains(&ChallengeRole::PasswordThenTotp);
                let separate_password = roles.contains(&ChallengeRole::Password);
                let separate_totp = roles.contains(&ChallengeRole::Totp);
                ensure!(
                    (!combined || !(separate_password || separate_totp))
                        && (combined || separate_password) == password_alias.is_some()
                        && (combined || separate_totp) == totp_alias.is_some()
                        && (password_alias.is_some() || totp_alias.is_some()),
                    "Configured SSH factors do not match PAM challenges"
                );
            }
        }
        Ok(())
    }
}

impl PamChallenge {
    /// The profile authorizes the role. A remote prompt can only match that role.
    pub fn match_prompt(&self, prompt: &str, echo: bool) -> Result<ChallengeRole> {
        ensure!(
            prompt.trim() == self.label && (!echo || self.role == ChallengeRole::Username),
            "Unexpected PAM challenge"
        );
        Ok(self.role)
    }
}

/// Tracks the expected challenges in a single authentication attempt.
pub struct PamExchange<'a> {
    remaining: &'a [PamChallenge],
    rejected: bool,
}

impl<'a> PamExchange<'a> {
    pub fn start(profile: &'a SshAuthProfile) -> Result<Self> {
        profile.validate()?;
        let SshAuthentication::Pam { challenges, .. } = &profile.authentication else {
            anyhow::bail!("SSH profile does not use keyboard-interactive authentication");
        };
        Ok(Self {
            remaining: challenges,
            rejected: false,
        })
    }

    pub fn accept_prompt(&mut self, prompt: &str, echo: bool) -> Result<ChallengeRole> {
        ensure!(!self.rejected, "PAM authentication was rejected");
        self.rejected = true;
        let (expected, rest) = self
            .remaining
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("Extra PAM challenge"))?;
        let role = expected.match_prompt(prompt, echo)?;
        self.remaining = rest;
        self.rejected = false;
        Ok(role)
    }

    pub fn verify_complete(self) -> Result<()> {
        ensure!(
            !self.rejected && self.remaining.is_empty(),
            "Incomplete PAM challenge sequence"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pam_accepts_configured_separate_localized_and_combined_prompts() -> Result<()> {
        for (password_alias, totp_alias, configured) in [
            (
                Some("PASSWORD"),
                Some("OTP"),
                vec![
                    ("Usuário:", "username", true),
                    ("Senha:", "password", false),
                    ("Código:", "totp", false),
                ],
            ),
            (
                Some("PASSWORD"),
                Some("OTP"),
                vec![("Password plus code:", "password_then_totp", false)],
            ),
            (
                Some("PASSWORD"),
                None,
                vec![("Password:", "password", false)],
            ),
            (None, Some("OTP"), vec![("Code:", "totp", false)]),
        ] {
            let profile: SshAuthProfile = serde_json::from_value(json!({
                "destination":{"host":"guest.example.test","port":2222,"username":"fixture"},
                "authentication":{"mode":"pam","password_alias":password_alias,"totp_alias":totp_alias,
                    "challenges": configured.iter().map(|(label,role,_)|json!({"label":label,"role":role})).collect::<Vec<_>>()}
            }))?;
            profile.validate()?;
            let SshAuthentication::Pam { challenges, .. } = profile.authentication else {
                unreachable!()
            };
            for (challenge, (label, role, echo)) in challenges.iter().zip(configured) {
                assert_eq!(
                    serde_json::to_value(challenge.match_prompt(label, echo)?)?,
                    role
                );
                assert!(challenge.match_prompt("Unknown:", false).is_err());
                if !echo {
                    assert!(challenge.match_prompt(label, true).is_err());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn exchange_rejects_reordered_repeated_and_missing_challenges() -> Result<()> {
        let profile: SshAuthProfile = serde_json::from_value(json!({
            "destination":{"host":"guest.example.test","port":2222,"username":"fixture"},
            "authentication":{"mode":"pam","password_alias":"PASSWORD","totp_alias":"OTP",
                "challenges":[{"label":"Password:","role":"password"},
                    {"label":"Code:","role":"totp"}]}
        }))?;
        let mut exchange = PamExchange::start(&profile)?;
        assert_eq!(
            exchange.accept_prompt("Password:", false)?,
            ChallengeRole::Password
        );
        assert_eq!(exchange.accept_prompt("Code:", false)?, ChallengeRole::Totp);
        exchange.verify_complete()?;

        let mut reordered = PamExchange::start(&profile)?;
        assert!(reordered.accept_prompt("Code:", false).is_err());
        assert!(reordered.accept_prompt("Password:", false).is_err());
        let mut repeated = PamExchange::start(&profile)?;
        repeated.accept_prompt("Password:", false)?;
        assert!(repeated.accept_prompt("Password:", false).is_err());
        let mut missing = PamExchange::start(&profile)?;
        missing.accept_prompt("Password:", false)?;
        assert!(missing.verify_complete().is_err());
        Ok(())
    }

    #[test]
    fn pam_rejects_missing_factor_and_duplicate_labels() -> Result<()> {
        for (totp_alias, label) in [(None, "Code:"), (Some("OTP"), "Password:")] {
            let profile: SshAuthProfile = serde_json::from_value(json!({
                "destination":{"host":"guest.example.test","port":2222,"username":"fixture"},
                "authentication":{"mode":"pam","password_alias":"PASSWORD","totp_alias":totp_alias,
                    "challenges":[{"label":"Password:","role":"password"},{"label":label,"role":"totp"}]}
            }))?;
            assert!(profile.validate().is_err());
        }
        Ok(())
    }
}
