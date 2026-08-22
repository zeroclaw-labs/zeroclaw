//! The `peercred` provider: authenticates a local-socket peer by its
//! kernel-reported uid.
//!
//! Two acceptance paths, both explicit:
//! - The daemon's OWN uid maps to the trusted shared operator while
//!   `security.trust_daemon_uid` is on (the default): the operator who
//!   runs the daemon owns its config file, and local-only lockout
//!   recovery depends on that authority.
//! - Any other uid must be bound by an explicit `[users.<name>].uid`
//!   roster entry, which resolves to that entry's durable principal id.
//!
//! Everything else — an unrostered uid, root, a roster entry removed
//! since boot — is denied. There is no fallback from a failed uid match
//! to shared-operator access.
//!
//! The uid→principal binding is live state ([`UidRoster`]): the daemon
//! refreshes it from config at the same moment it bumps the resolver's
//! authorization generation, so roster edits apply without restart. The
//! resolver independently denies identities whose roster entry vanished,
//! so a stale binding can only DENY sooner, never allow longer.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use zeroclaw_api::principal::{
    AuthMethod, AuthOutcome, AuthenticatedIdentity, DenyReason, IdentitySubject,
};
use zeroclaw_config::schema::Config;

use super::{AuthProvider, Credential};

/// Live uid → durable-principal-id binding compiled from `[users]`.
#[derive(Default)]
pub struct UidRoster {
    by_uid: RwLock<HashMap<u32, String>>,
}

impl UidRoster {
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let roster = Self::default();
        roster.replace_from_config(config);
        roster
    }

    /// Swap in the current `[users]` uid bindings. Call alongside the
    /// resolver's policy replacement so both move at one generation.
    pub fn replace_from_config(&self, config: &Config) {
        let map = config
            .users
            .iter()
            .filter_map(|(name, user)| {
                user.uid
                    .map(|uid| (uid, user.effective_principal_id(name).to_owned()))
            })
            .collect();
        *self.by_uid.write() = map;
    }

    #[must_use]
    pub fn principal_id_for(&self, uid: u32) -> Option<String> {
        self.by_uid.read().get(&uid).cloned()
    }
}

pub struct PeercredAuthProvider {
    daemon_uid: u32,
    trust_daemon_uid: bool,
    roster: Arc<UidRoster>,
}

impl PeercredAuthProvider {
    #[must_use]
    pub fn new(daemon_uid: u32, trust_daemon_uid: bool, roster: Arc<UidRoster>) -> Self {
        Self {
            daemon_uid,
            trust_daemon_uid,
            roster,
        }
    }

    /// The daemon's own uid (Unix). On non-Unix targets the transport
    /// never produces a `Peercred` credential, so the provider is inert
    /// there; `u32::MAX` guarantees no accidental match.
    #[must_use]
    pub fn current_process_uid() -> u32 {
        #[cfg(unix)]
        // SAFETY: getuid is always safe to call and cannot fail.
        let uid = unsafe { libc::getuid() };
        #[cfg(not(unix))]
        let uid = u32::MAX;
        uid
    }
}

#[async_trait]
impl AuthProvider for PeercredAuthProvider {
    fn name(&self) -> &str {
        "peercred"
    }

    fn method(&self) -> AuthMethod {
        AuthMethod::Peercred
    }

    fn accepts(&self, credential: &Credential) -> bool {
        matches!(credential, Credential::Peercred { .. })
    }

    async fn verify(&self, credential: &Credential) -> AuthOutcome {
        let Credential::Peercred { uid } = credential else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        if *uid == self.daemon_uid && self.trust_daemon_uid {
            return AuthOutcome::Verified(AuthenticatedIdentity::shared_operator(
                AuthMethod::Peercred,
            ));
        }
        match self.roster.principal_id_for(*uid) {
            Some(principal_id) => AuthOutcome::Verified(AuthenticatedIdentity::new(
                IdentitySubject::Roster { principal_id },
                AuthMethod::Peercred,
            )),
            // No roster match, no daemon-uid trust: denied. Never a
            // shared-operator fallback (root included).
            None => AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::principal::PrincipalId;
    use zeroclaw_config::schema::{PermissionProfileConfig, UserConfig};

    fn roster_with(entries: &[(&str, u32)]) -> Arc<UidRoster> {
        let mut config = Config::default();
        config
            .permission_profiles
            .insert("operator".into(), PermissionProfileConfig::default());
        for (name, uid) in entries {
            config.users.insert(
                (*name).to_string(),
                UserConfig {
                    principal_id: None,
                    uid: Some(*uid),
                    permission_profiles: vec!["operator".into()],
                },
            );
        }
        Arc::new(UidRoster::from_config(&config))
    }

    #[tokio::test]
    async fn daemon_uid_is_trusted_while_the_flag_is_on() {
        let provider = PeercredAuthProvider::new(1000, true, roster_with(&[]));
        let out = provider.verify(&Credential::Peercred { uid: 1000 }).await;
        let identity = out.identity().expect("trusted");
        assert_eq!(identity.subject, IdentitySubject::SharedOperator);
        assert_eq!(
            identity.subject.principal_id().as_str(),
            PrincipalId::SHARED_OPERATOR
        );
    }

    #[tokio::test]
    async fn trust_daemon_uid_off_requires_a_roster_mapping() {
        let provider = PeercredAuthProvider::new(1000, false, roster_with(&[]));
        assert!(
            !provider
                .verify(&Credential::Peercred { uid: 1000 })
                .await
                .is_allowed(),
            "with the flag off the daemon's own uid holds no implicit trust"
        );
        let provider = PeercredAuthProvider::new(1000, false, roster_with(&[("op", 1000)]));
        let out = provider.verify(&Credential::Peercred { uid: 1000 }).await;
        assert_eq!(
            out.identity().expect("roster-mapped").subject,
            IdentitySubject::Roster {
                principal_id: "op".into()
            }
        );
    }

    #[tokio::test]
    async fn roster_uid_authenticates_as_its_durable_principal() {
        let provider = PeercredAuthProvider::new(1000, true, roster_with(&[("bob", 2222)]));
        let out = provider.verify(&Credential::Peercred { uid: 2222 }).await;
        let identity = out.identity().expect("authenticated");
        assert_eq!(
            identity.subject,
            IdentitySubject::Roster {
                principal_id: "bob".into()
            }
        );
        assert_eq!(identity.subject.principal_id().as_str(), "user:bob");
    }

    #[tokio::test]
    async fn pinned_principal_id_wins_over_the_entry_name() {
        let mut config = Config::default();
        config
            .permission_profiles
            .insert("operator".into(), PermissionProfileConfig::default());
        config.users.insert(
            "bob-renamed".into(),
            UserConfig {
                principal_id: Some("bob".into()),
                uid: Some(2222),
                permission_profiles: vec!["operator".into()],
            },
        );
        let roster = Arc::new(UidRoster::from_config(&config));
        let provider = PeercredAuthProvider::new(1000, true, roster);
        let out = provider.verify(&Credential::Peercred { uid: 2222 }).await;
        assert_eq!(
            out.identity().expect("authenticated").subject,
            IdentitySubject::Roster {
                principal_id: "bob".into()
            }
        );
    }

    #[tokio::test]
    async fn unrostered_uid_and_root_are_denied() {
        let provider = PeercredAuthProvider::new(1000, true, roster_with(&[("bob", 2222)]));
        assert!(
            !provider
                .verify(&Credential::Peercred { uid: 3333 })
                .await
                .is_allowed()
        );
        assert!(
            !provider
                .verify(&Credential::Peercred { uid: 0 })
                .await
                .is_allowed(),
            "uid 0 must map through [users], never bypass"
        );
    }

    #[tokio::test]
    async fn roster_replacement_applies_live() {
        let roster = roster_with(&[("bob", 2222)]);
        let provider = PeercredAuthProvider::new(1000, true, Arc::clone(&roster));
        assert!(
            provider
                .verify(&Credential::Peercred { uid: 2222 })
                .await
                .is_allowed()
        );
        // Rebind: bob's entry is removed, carol appears.
        let mut config = Config::default();
        config
            .permission_profiles
            .insert("operator".into(), PermissionProfileConfig::default());
        config.users.insert(
            "carol".into(),
            UserConfig {
                principal_id: None,
                uid: Some(4444),
                permission_profiles: vec!["operator".into()],
            },
        );
        roster.replace_from_config(&config);
        assert!(
            !provider
                .verify(&Credential::Peercred { uid: 2222 })
                .await
                .is_allowed(),
            "a removed roster entry denies without restart"
        );
        assert!(
            provider
                .verify(&Credential::Peercred { uid: 4444 })
                .await
                .is_allowed()
        );
    }

    #[tokio::test]
    async fn non_peercred_credentials_are_not_accepted() {
        let provider = PeercredAuthProvider::new(1000, true, roster_with(&[]));
        assert!(!provider.accepts(&Credential::Bearer("tok".into())));
        assert!(!provider.accepts(&Credential::None));
        assert!(
            !provider
                .verify(&Credential::Bearer("tok".into()))
                .await
                .is_allowed()
        );
    }
}
