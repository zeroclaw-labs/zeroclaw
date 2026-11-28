//! Daemon-held OIDC refresh grants (ADR-017, "Keep device sessions alive
//! through IdP refresh, held by the daemon").
//!
//! An IdP returns a refresh token when the device grant or a browser sign-in
//! completes, which is before the certificate request that creates the
//! device's ledger `device_id`. This module holds that refresh token in memory
//! as a [`PendingGrant`] keyed by a hash of the access token from the same
//! token response, because that access token is what the certificate request
//! then presents as its bearer. The certificate route claims the pending grant
//! once, only for the provider and principal the bearer verified as, seals it
//! through the [`SecretStore`], and the certificate ledger stores it in the
//! same transaction that publishes the certificate (see
//! `CertLedger::record_issued_with_refresh_grant`).
//!
//! Clients never hold a refresh token, and a grant is never a bearer
//! credential: it is only ever redeemed by the daemon, for the device whose
//! certificate owns it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use parking_lot::Mutex;
use sha2::{Digest as _, Sha256};
use zeroclaw_api::principal::PrincipalId;
use zeroize::Zeroizing;

use super::enrollment::ClientAuthentication;
use crate::security::SecretStore;

/// The longest a pending grant is held, whatever the access token's or the
/// device code's advertised lifetime. RFC 8628 sets no ceiling on a device
/// code's `expires_in`; this matches the ceiling Zerocode applies to it.
pub const MAX_PENDING_GRANT_LIFETIME: Duration = Duration::from_secs(3600);

/// How many pending grants are held at once. A pending grant exists only
/// between a sign-in and its certificate request, so a real deployment holds
/// a handful; the bound keeps a flood of sign-ins from growing memory without
/// limit. Past it a new grant is not held, and that device simply has no
/// refresh grant until its next sign-in.
pub const MAX_PENDING_GRANTS: usize = 1024;

/// The provider and principal a refresh token was issued for. Recorded with
/// the grant and required to match on every later use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantBinding {
    /// The `[oidc.<alias>]` entry the sign-in went through.
    pub alias: String,
    /// That entry's issuer at sign-in.
    pub issuer: String,
    /// That entry's effective client id at sign-in.
    pub client_id: String,
    /// The canonical principal the sign-in verified as (issuer and `sub`).
    pub principal: PrincipalId,
}

/// A refresh token waiting for the certificate request of the sign-in that
/// produced it.
pub struct PendingGrant {
    binding: GrantBinding,
    client_auth: ClientAuthentication,
    refresh_token: Zeroizing<String>,
}

impl PendingGrant {
    /// `client_auth` is the method the token response was obtained with, so a
    /// refresh can authenticate the same way.
    pub fn new(
        binding: GrantBinding,
        client_auth: ClientAuthentication,
        refresh_token: String,
    ) -> Self {
        Self {
            binding,
            client_auth,
            refresh_token: Zeroizing::new(refresh_token),
        }
    }

    pub fn binding(&self) -> &GrantBinding {
        &self.binding
    }

    /// Seal the refresh token for storage. Fails rather than produce a grant
    /// when the store did not encrypt it: with secrets encryption turned off
    /// the store returns plaintext, and a refresh token is never stored in
    /// plaintext. The device then has no refresh grant.
    pub fn seal(self, store: &SecretStore) -> Result<SealedRefreshGrant> {
        let sealed = store.encrypt(&self.refresh_token)?;
        if !SecretStore::is_secure_encrypted(&sealed) {
            bail!(
                "secrets encryption is disabled, so the refresh token was not retained; \
                 enable [secrets] encrypt to keep device sessions alive through refresh"
            );
        }
        Ok(SealedRefreshGrant {
            binding: self.binding,
            client_auth: self.client_auth,
            sealed_refresh_token: sealed,
        })
    }
}

impl std::fmt::Debug for PendingGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingGrant")
            .field("binding", &self.binding)
            .field("client_auth", &self.client_auth)
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}

/// A refresh grant as the certificate ledger stores it: the binding, the
/// client authentication, and the refresh token sealed through the
/// [`SecretStore`].
#[derive(Clone)]
pub struct SealedRefreshGrant {
    binding: GrantBinding,
    client_auth: ClientAuthentication,
    sealed_refresh_token: String,
}

impl SealedRefreshGrant {
    /// Rebuild a grant from its stored columns.
    pub(crate) fn from_stored(
        binding: GrantBinding,
        client_auth: ClientAuthentication,
        sealed_refresh_token: String,
    ) -> Self {
        Self {
            binding,
            client_auth,
            sealed_refresh_token,
        }
    }

    pub fn binding(&self) -> &GrantBinding {
        &self.binding
    }

    pub fn client_auth(&self) -> ClientAuthentication {
        self.client_auth
    }

    pub(crate) fn sealed_refresh_token(&self) -> &str {
        &self.sealed_refresh_token
    }

    /// The refresh token, for redeeming it. Refuses a stored value that is not
    /// sealed, so plaintext that reached the table some other way is never
    /// sent to an IdP.
    pub fn unseal(&self, store: &SecretStore) -> Result<Zeroizing<String>> {
        if !SecretStore::is_secure_encrypted(&self.sealed_refresh_token) {
            bail!("stored refresh grant is not sealed; refusing to use it");
        }
        Ok(Zeroizing::new(store.decrypt(&self.sealed_refresh_token)?))
    }
}

impl std::fmt::Debug for SealedRefreshGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedRefreshGrant")
            .field("binding", &self.binding)
            .field("client_auth", &self.client_auth)
            .field("sealed_refresh_token", &"<redacted>")
            .finish()
    }
}

/// How long to hold a pending grant: no longer than its paired access token,
/// capped at the sign-in flow's lifetime (the device code's) and at
/// [`MAX_PENDING_GRANT_LIFETIME`]. An access token with no advertised
/// lifetime is held for the flow's lifetime.
pub fn pending_lifetime(access_token_expires_in: Option<u64>, flow_lifetime: Duration) -> Duration {
    let ceiling = flow_lifetime.min(MAX_PENDING_GRANT_LIFETIME);
    access_token_expires_in.map_or(ceiling, |secs| Duration::from_secs(secs).min(ceiling))
}

struct HeldGrant {
    grant: PendingGrant,
    expires_at: Instant,
}

/// The pending grants of in-flight sign-ins, in memory only. Nothing here
/// survives a restart: a sign-in interrupted by one leaves its device with no
/// refresh grant until the next sign-in.
#[derive(Default)]
pub struct PendingGrants {
    held: Mutex<HashMap<[u8; 32], HeldGrant>>,
}

impl PendingGrants {
    /// Hold `grant` for `lifetime` (see [`pending_lifetime`]) until the
    /// certificate request that presents `access_token` claims it.
    ///
    /// Returns `false`, holding nothing, when the lifetime is zero, the bound
    /// is reached, or the access token is already holding a grant. A repeated
    /// access token is ambiguous, so both grants are dropped rather than one
    /// being chosen.
    pub fn hold(&self, access_token: &str, grant: PendingGrant, lifetime: Duration) -> bool {
        if lifetime.is_zero() {
            return false;
        }
        let now = Instant::now();
        let Some(expires_at) = now.checked_add(lifetime) else {
            return false;
        };
        let key = token_key(access_token);
        let mut held = self.held.lock();
        held.retain(|_, entry| entry.expires_at > now);
        if held.remove(&key).is_some() {
            return false;
        }
        if held.len() >= MAX_PENDING_GRANTS {
            return false;
        }
        held.insert(key, HeldGrant { grant, expires_at });
        true
    }

    /// Claim the grant held for `access_token`, for a certificate request
    /// whose bearer verified as `presented`.
    ///
    /// The grant is removed whether or not it is returned: a claim through
    /// another provider or as another principal fails closed, and the grant
    /// does not wait for a second attempt. An expired grant is never returned.
    pub fn claim(&self, access_token: &str, presented: &GrantBinding) -> Option<PendingGrant> {
        let entry = self.held.lock().remove(&token_key(access_token))?;
        if entry.expires_at <= Instant::now() || entry.grant.binding != *presented {
            return None;
        }
        Some(entry.grant)
    }

    /// How many unexpired grants are held.
    pub fn len(&self) -> usize {
        self.held
            .lock()
            .values()
            .filter(|entry| entry.expires_at > Instant::now())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for PendingGrants {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingGrants")
            .field("held", &self.held.lock().len())
            .finish()
    }
}

/// The key a pending grant is held under: a SHA-256 of its paired access
/// token, so the map never holds a bearer credential.
fn token_key(access_token: &str) -> [u8; 32] {
    Sha256::digest(access_token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn binding(alias: &str, sub: &str) -> GrantBinding {
        GrantBinding {
            alias: alias.into(),
            issuer: "https://idp.example".into(),
            client_id: "zerocode-cli".into(),
            principal: PrincipalId::for_oidc("https://idp.example", sub),
        }
    }

    fn pending(sub: &str, token: &str) -> PendingGrant {
        PendingGrant::new(
            binding("corp", sub),
            ClientAuthentication::ClientSecretBasic,
            token.into(),
        )
    }

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn a_held_grant_is_claimed_once_by_its_own_access_token() {
        let grants = PendingGrants::default();
        assert!(grants.hold("at-1", pending("alice", "rt-1"), MINUTE));
        // Another access token never reaches it.
        assert!(grants.claim("at-2", &binding("corp", "alice")).is_none());
        let claimed = grants.claim("at-1", &binding("corp", "alice")).unwrap();
        assert_eq!(claimed.binding(), &binding("corp", "alice"));
        assert_eq!(claimed.refresh_token.as_str(), "rt-1");
        // Claimed means gone.
        assert!(grants.claim("at-1", &binding("corp", "alice")).is_none());
    }

    #[test]
    fn two_devices_signing_in_as_one_person_each_claim_only_their_own_grant() {
        // Issuer and sub alone cannot tell the two logins apart; their access
        // tokens can.
        let grants = PendingGrants::default();
        assert!(grants.hold("at-laptop", pending("alice", "rt-laptop"), MINUTE));
        assert!(grants.hold("at-phone", pending("alice", "rt-phone"), MINUTE));
        let phone = grants.claim("at-phone", &binding("corp", "alice")).unwrap();
        let laptop = grants
            .claim("at-laptop", &binding("corp", "alice"))
            .unwrap();
        assert_eq!(phone.refresh_token.as_str(), "rt-phone");
        assert_eq!(laptop.refresh_token.as_str(), "rt-laptop");
    }

    #[test]
    fn a_claim_for_another_provider_or_principal_gets_nothing_and_spends_the_grant() {
        let presented = [
            binding("corp", "mallory"),
            binding("other", "alice"),
            GrantBinding {
                issuer: "https://elsewhere.example".into(),
                ..binding("corp", "alice")
            },
            GrantBinding {
                client_id: "another-client".into(),
                ..binding("corp", "alice")
            },
        ];
        for presented in presented {
            let grants = PendingGrants::default();
            assert!(grants.hold("at-1", pending("alice", "rt-1"), MINUTE));
            assert!(grants.claim("at-1", &presented).is_none(), "{presented:?}");
            // A mismatched claim fails closed: the grant does not wait for a
            // second attempt.
            assert!(grants.claim("at-1", &binding("corp", "alice")).is_none());
        }
    }

    #[test]
    fn an_unclaimed_grant_expires_without_attaching_to_anything() {
        let grants = PendingGrants::default();
        assert!(grants.hold("at-1", pending("alice", "rt-1"), Duration::from_millis(20)));
        std::thread::sleep(Duration::from_millis(40));
        assert!(grants.claim("at-1", &binding("corp", "alice")).is_none());
        assert_eq!(grants.len(), 0);
    }

    #[test]
    fn a_grant_is_held_no_longer_than_the_ceiling() {
        assert_eq!(
            pending_lifetime(Some(30), Duration::from_secs(600)),
            Duration::from_secs(30)
        );
        assert_eq!(
            pending_lifetime(Some(900), Duration::from_secs(600)),
            Duration::from_secs(600)
        );
        assert_eq!(
            pending_lifetime(None, Duration::from_secs(600)),
            Duration::from_secs(600)
        );
        assert_eq!(
            pending_lifetime(Some(u64::MAX), Duration::from_secs(u64::MAX)),
            MAX_PENDING_GRANT_LIFETIME
        );
        assert_eq!(pending_lifetime(Some(0), MINUTE), Duration::ZERO);
    }

    #[test]
    fn a_zero_lifetime_or_a_reused_access_token_holds_nothing() {
        let grants = PendingGrants::default();
        assert!(!grants.hold("at-1", pending("alice", "rt-1"), Duration::ZERO));
        assert_eq!(grants.len(), 0);

        // The same access token offered twice is ambiguous: neither refresh
        // token is held.
        assert!(grants.hold("at-1", pending("alice", "rt-1"), MINUTE));
        assert!(!grants.hold("at-1", pending("alice", "rt-2"), MINUTE));
        assert!(grants.claim("at-1", &binding("corp", "alice")).is_none());
    }

    #[test]
    fn pending_grants_are_bounded() {
        let grants = PendingGrants::default();
        for i in 0..MAX_PENDING_GRANTS {
            assert!(grants.hold(&format!("at-{i}"), pending("alice", "rt"), MINUTE));
        }
        assert!(!grants.hold("at-overflow", pending("alice", "rt"), MINUTE));
        // Expired entries make room again.
        let grants = PendingGrants::default();
        for i in 0..MAX_PENDING_GRANTS {
            assert!(grants.hold(
                &format!("at-{i}"),
                pending("alice", "rt"),
                Duration::from_millis(10)
            ));
        }
        std::thread::sleep(Duration::from_millis(30));
        assert!(grants.hold("at-next", pending("alice", "rt"), MINUTE));
    }

    #[test]
    fn grants_never_print_token_material() {
        let grants = PendingGrants::default();
        assert!(grants.hold("raw-access", pending("alice", "raw-refresh"), MINUTE));
        let claimed = grants
            .claim("raw-access", &binding("corp", "alice"))
            .unwrap();
        let printed = format!("{claimed:?} {grants:?}");
        assert!(!printed.contains("raw-refresh"), "{printed}");
        assert!(!printed.contains("raw-access"), "{printed}");

        let dir = tempfile::TempDir::new().unwrap();
        let sealed = claimed.seal(&SecretStore::new(dir.path(), true)).unwrap();
        let printed = format!("{sealed:?}");
        assert!(!printed.contains("raw-refresh"), "{printed}");
    }

    #[test]
    fn sealing_encrypts_and_unsealing_recovers_the_refresh_token() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = SecretStore::new(dir.path(), true);
        let sealed = pending("alice", "rt-1").seal(&store).unwrap();
        assert!(SecretStore::is_secure_encrypted(
            sealed.sealed_refresh_token()
        ));
        assert!(!sealed.sealed_refresh_token().contains("rt-1"));
        assert_eq!(sealed.binding(), &binding("corp", "alice"));
        assert_eq!(
            sealed.client_auth(),
            ClientAuthentication::ClientSecretBasic
        );
        assert_eq!(sealed.unseal(&store).unwrap().as_str(), "rt-1");
    }

    #[test]
    fn a_refresh_token_is_never_sealed_in_plaintext() {
        // With secrets encryption turned off the store hands the plaintext
        // back; a grant must then not be produced at all.
        let dir = tempfile::TempDir::new().unwrap();
        let err = pending("alice", "rt-1")
            .seal(&SecretStore::new(dir.path(), false))
            .unwrap_err();
        assert!(!format!("{err:#}").contains("rt-1"));
        // And a stored value that is not sealed is refused on the way out.
        let store = SecretStore::new(dir.path(), true);
        let tampered = SealedRefreshGrant::from_stored(
            binding("corp", "alice"),
            ClientAuthentication::ClientSecretBasic,
            "rt-plain".into(),
        );
        assert!(tampered.unseal(&store).is_err());
    }

    #[test]
    fn concurrent_claims_hand_a_grant_to_at_most_one_caller() {
        let grants = Arc::new(PendingGrants::default());
        assert!(grants.hold("at-1", pending("alice", "rt-1"), MINUTE));
        let winners: usize = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let grants = Arc::clone(&grants);
                    scope.spawn(move || grants.claim("at-1", &binding("corp", "alice")).is_some())
                })
                .collect();
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum()
        });
        assert_eq!(winners, 1);
    }
}
