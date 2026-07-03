//! RFC #7141 `peercred` provider: authenticates a local Unix-socket peer by
//! its kernel-reported uid (`SO_PEERCRED`), with zero client-side work.
//!
//! This slice accepts exactly the uid the daemon itself runs as, mirroring
//! the trust already granted by the socket's `0o600` mode. The RFC's
//! `[users.<name>]` roster (mapping additional uids to named principals with
//! grants) layers on in the config-schema slice; until then a same-uid peer
//! binds the trusted shared-operator sentinel, which is today's behaviour
//! made explicit.

use async_trait::async_trait;
use zeroclaw_api::principal::{AuthMethod, AuthOutcome, DenyReason, Principal};

use super::{AuthProvider, Credential};

pub struct PeercredAuthProvider {
    daemon_uid: u32,
}

impl PeercredAuthProvider {
    #[must_use]
    pub fn new(daemon_uid: u32) -> Self {
        Self { daemon_uid }
    }

    /// Construct with the current process uid (Unix). On non-Unix targets the
    /// transport never produces a `Peercred` credential, so the provider is
    /// inert there; `u32::MAX` guarantees no accidental match.
    #[must_use]
    pub fn for_current_process() -> Self {
        #[cfg(unix)]
        let uid = unsafe { libc::getuid() };
        #[cfg(not(unix))]
        let uid = u32::MAX;
        Self::new(uid)
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
        match credential {
            Credential::Peercred { uid } if *uid == self.daemon_uid => {
                let mut principal = Principal::shared_operator();
                principal.auth_method = AuthMethod::Peercred;
                AuthOutcome::Trusted(principal)
            }
            _ => AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_uid_is_trusted() {
        let provider = PeercredAuthProvider::new(1000);
        let out = provider.verify(&Credential::Peercred { uid: 1000 }).await;
        assert!(out.is_allowed());
        let p = out.principal().expect("principal bound");
        assert_eq!(p.auth_method, AuthMethod::Peercred);
    }

    #[tokio::test]
    async fn different_uid_is_denied() {
        let provider = PeercredAuthProvider::new(1000);
        let out = provider.verify(&Credential::Peercred { uid: 1001 }).await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    #[tokio::test]
    async fn root_peer_is_not_implicitly_trusted() {
        let provider = PeercredAuthProvider::new(1000);
        let out = provider.verify(&Credential::Peercred { uid: 0 }).await;
        assert!(
            !out.is_allowed(),
            "uid 0 must map through [users], not bypass"
        );
    }

    #[tokio::test]
    async fn non_peercred_credentials_are_not_accepted() {
        let provider = PeercredAuthProvider::new(1000);
        assert!(!provider.accepts(&Credential::Bearer("tok".into())));
        assert!(!provider.accepts(&Credential::None));
        let out = provider.verify(&Credential::Bearer("tok".into())).await;
        assert!(!out.is_allowed());
    }
}
