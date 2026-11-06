//! The `password` provider: verifies a roster entry's name and password
//! against its `[users.<name>].password_hash`.
//!
//! Registered only while `security.password_auth.enabled` is on and the
//! gateway requires pairing. The login name is the `[users.<name>]` key,
//! matched exactly, and a match resolves to that entry's durable principal
//! id: the same principal its peer credential, if it has one, resolves to.
//! Every miss (an unknown name, an entry without a hash, an unusable input,
//! a wrong password) is the same `BadCredential` denial after one scrypt
//! verification. A miss with no hash to check runs against a decoy at the
//! parameters most of the roster's hashes use, so its timing matches checking
//! one of them.
//!
//! The hash check runs on the blocking pool: scrypt deliberately spends a
//! noticeable amount of CPU time and tens of MiB of memory on every attempt.

use std::collections::HashMap;

use async_trait::async_trait;
use zeroclaw_api::principal::{
    AuthMethod, AuthOutcome, AuthenticatedIdentity, DenyReason, IdentitySubject,
};
use zeroclaw_config::password_hash::{Decoy, verify_password};
use zeroclaw_config::schema::Config;

use super::{AuthProvider, Credential};

struct PasswordEntry {
    principal_id: String,
    password_hash: String,
}

/// The provider for roster passwords. Compiled from the accepted policy's
/// configuration and replaced along with it, like the other providers.
pub struct PasswordAuthProvider {
    /// Login name to entry, for roster entries that carry a hash.
    by_name: HashMap<String, PasswordEntry>,
    /// What a miss is checked against, at the roster's usual parameters.
    decoy: Decoy,
}

impl PasswordAuthProvider {
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let by_name: HashMap<String, PasswordEntry> = config
            .users
            .iter()
            .filter_map(|(name, user)| {
                user.password_hash.as_ref().map(|hash| {
                    (
                        name.clone(),
                        PasswordEntry {
                            principal_id: user.effective_principal_id(name).to_owned(),
                            password_hash: hash.clone(),
                        },
                    )
                })
            })
            .collect();
        let decoy = Decoy::for_hashes(by_name.values().map(|entry| entry.password_hash.as_str()));
        Self { by_name, decoy }
    }
}

#[async_trait]
impl AuthProvider for PasswordAuthProvider {
    fn name(&self) -> &str {
        "password"
    }

    fn method(&self) -> AuthMethod {
        AuthMethod::Password
    }

    fn accepts(&self, credential: &Credential) -> bool {
        matches!(credential, Credential::Password { .. })
    }

    async fn verify(&self, credential: &Credential) -> AuthOutcome {
        let Credential::Password { username, password } = credential else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        let entry = self.by_name.get(username.as_str());
        let stored = entry.map(|entry| entry.password_hash.clone());
        let password = password.clone();
        let decoy = self.decoy.clone();
        let check = tokio::task::spawn_blocking(move || {
            verify_password(&password, stored.as_deref(), &decoy)
        })
        .await;
        let matched = match check {
            Ok(matched) => matched,
            // A failed hash check verifies nothing: it is a denial like any
            // other miss, and it is logged because it should never happen.
            Err(error) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "panicked": error.is_panic() })),
                    "password check task failed; denying the attempt"
                );
                false
            }
        };
        match entry {
            Some(entry) if matched => AuthOutcome::Verified(AuthenticatedIdentity::new(
                IdentitySubject::Roster {
                    principal_id: entry.principal_id.clone(),
                },
                AuthMethod::Password,
            )),
            _ => AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;
    use zeroclaw_config::password_hash::hash_password;
    use zeroclaw_config::schema::UserConfig;
    use zeroize::Zeroizing;

    const PASSWORD: &str = "zeroclaw-test-passphrase";

    /// One real hash for the whole module: every scrypt run is slow in an
    /// unoptimized test build.
    static STORED: LazyLock<String> =
        LazyLock::new(|| hash_password(PASSWORD).expect("hashing a test password"));

    fn user(uid: Option<u32>, principal_id: Option<&str>, hashed: bool) -> UserConfig {
        UserConfig {
            principal_id: principal_id.map(str::to_owned),
            uid,
            password_hash: hashed.then(|| STORED.clone()),
            permission_profiles: vec!["operator".into()],
        }
    }

    fn provider() -> PasswordAuthProvider {
        let mut config = Config::default();
        config
            .users
            .insert("zeroclaw_user".into(), user(Some(1001), None, true));
        config
            .users
            .insert("user_a".into(), user(None, Some("user_a_pinned"), true));
        config
            .users
            .insert("peer_only".into(), user(Some(1002), None, false));
        PasswordAuthProvider::from_config(&config)
    }

    fn password(username: &str, password: &str) -> Credential {
        Credential::Password {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    fn roster_principal(outcome: &AuthOutcome) -> Option<String> {
        match outcome.identity() {
            Some(AuthenticatedIdentity {
                subject: IdentitySubject::Roster { principal_id },
                method: AuthMethod::Password,
                ..
            }) => Some(principal_id.clone()),
            _ => None,
        }
    }

    #[tokio::test]
    async fn matching_password_resolves_to_the_entry_principal() {
        let out = provider()
            .verify(&password("zeroclaw_user", PASSWORD))
            .await;
        assert_eq!(roster_principal(&out).as_deref(), Some("zeroclaw_user"));
    }

    #[tokio::test]
    async fn pinned_principal_id_wins_over_the_entry_name() {
        let out = provider().verify(&password("user_a", PASSWORD)).await;
        assert_eq!(roster_principal(&out).as_deref(), Some("user_a_pinned"));
    }

    #[tokio::test]
    async fn every_miss_is_the_same_denial() {
        let provider = provider();
        let misses = [
            password("zeroclaw_user", "zeroclaw-wrong-passphrase"),
            password("zeroclaw_nobody", PASSWORD),
            password("peer_only", PASSWORD),
            password("ZeroClaw_User", PASSWORD),
        ];
        for credential in misses {
            let out = provider.verify(&credential).await;
            assert!(
                matches!(
                    out,
                    AuthOutcome::Denied {
                        reason: DenyReason::BadCredential
                    }
                ),
                "{credential:?} -> {out:?}"
            );
        }
    }

    #[tokio::test]
    async fn other_credential_kinds_are_not_accepted() {
        let provider = provider();
        for credential in [
            Credential::Bearer(PASSWORD.into()),
            Credential::Peercred { uid: 1001 },
            Credential::None,
        ] {
            assert!(!provider.accepts(&credential), "{credential:?}");
            assert!(!provider.verify(&credential).await.is_allowed());
        }
        assert!(provider.accepts(&password("zeroclaw_user", PASSWORD)));
    }
}
