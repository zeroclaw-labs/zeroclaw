use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use zeroclaw_api::grants::ResolvedGrants;
use zeroclaw_api::principal::{AuthMethod, AuthOutcome, DenyReason, Principal};

use super::{AuthProvider, Credential};

pub struct RosterUser {
    pub authorized_keys: Vec<String>,
    pub uid: Option<u32>,
    pub grants: ResolvedGrants,
}

pub type UserRoster = HashMap<String, RosterUser>;

pub struct SshKeyAuthProvider {
    roster: Arc<UserRoster>,
}

impl SshKeyAuthProvider {
    #[must_use]
    pub fn new(roster: Arc<UserRoster>) -> Self {
        Self { roster }
    }
}

fn openssh_key_blob(entry: &str) -> Option<(String, Vec<u8>)> {
    let mut parts = entry.split_whitespace();
    let algo = parts.next()?.to_string();
    let blob = STANDARD.decode(parts.next()?).ok()?;
    Some((algo, blob))
}

fn wire_string(blob: &[u8], offset: &mut usize) -> Option<Vec<u8>> {
    let len_bytes = blob.get(*offset..*offset + 4)?;
    let len = u32::from_be_bytes(len_bytes.try_into().ok()?) as usize;
    *offset += 4;
    let value = blob.get(*offset..*offset + len)?.to_vec();
    *offset += len;
    Some(value)
}

fn verify_with_key(entry: &str, message: &[u8], signature: &[u8]) -> bool {
    use ring::signature;
    let Some((algo, blob)) = openssh_key_blob(entry) else {
        return false;
    };
    let mut offset = 0;
    let Some(blob_algo) = wire_string(&blob, &mut offset) else {
        return false;
    };
    if blob_algo != algo.as_bytes() {
        return false;
    }
    match algo.as_str() {
        "ssh-ed25519" => {
            let Some(key) = wire_string(&blob, &mut offset) else {
                return false;
            };
            signature::UnparsedPublicKey::new(&signature::ED25519, key)
                .verify(message, signature)
                .is_ok()
        }
        "ecdsa-sha2-nistp256" => {
            let Some(curve) = wire_string(&blob, &mut offset) else {
                return false;
            };
            if curve != b"nistp256" {
                return false;
            }
            let Some(point) = wire_string(&blob, &mut offset) else {
                return false;
            };
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(message, signature)
                .is_ok()
        }
        _ => false,
    }
}

#[async_trait]
impl AuthProvider for SshKeyAuthProvider {
    fn name(&self) -> &str {
        "ssh-key"
    }

    fn method(&self) -> AuthMethod {
        AuthMethod::SshKey
    }

    fn accepts(&self, credential: &Credential) -> bool {
        matches!(credential, Credential::SshSignature { .. })
    }

    async fn verify(&self, credential: &Credential) -> AuthOutcome {
        let Credential::SshSignature {
            username,
            nonce,
            signature,
        } = credential
        else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        if nonce.is_empty() || signature.is_empty() {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        }
        let Some(user) = self.roster.get(username) else {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        };
        let verified = user
            .authorized_keys
            .iter()
            .any(|entry| verify_with_key(entry, nonce, signature));
        if !verified {
            return AuthOutcome::Denied {
                reason: DenyReason::BadCredential,
            };
        }
        let namespaced_id = format!("user:{}", username.as_str());
        let mut principal = Principal::new(namespaced_id, username.as_str(), AuthMethod::SshKey);
        principal.grants = user.grants.clone();
        AuthOutcome::Authenticated(principal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use zeroclaw_api::grants::{Resource, Verb};

    fn test_keypair() -> (Ed25519KeyPair, String) {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = key.public_key().as_ref().to_vec();
        let mut blob = Vec::new();
        for part in [b"ssh-ed25519".as_slice(), &public] {
            blob.extend_from_slice(&(part.len() as u32).to_be_bytes());
            blob.extend_from_slice(part);
        }
        let entry = format!("ssh-ed25519 {} test@host", STANDARD.encode(&blob));
        (key, entry)
    }

    fn roster_with(entry: String) -> Arc<UserRoster> {
        let mut grants = ResolvedGrants::none();
        grants.resources.insert(
            Resource::System,
            std::collections::BTreeSet::from([Verb::Read]),
        );
        let mut roster = UserRoster::new();
        roster.insert(
            "alice".to_string(),
            RosterUser {
                authorized_keys: vec![entry],
                uid: Some(1234),
                grants,
            },
        );
        Arc::new(roster)
    }

    #[tokio::test]
    async fn valid_signature_authenticates_with_profile_grants() {
        let (key, entry) = test_keypair();
        let provider = SshKeyAuthProvider::new(roster_with(entry));
        let nonce = b"server-issued-nonce".to_vec();
        let signature = key.sign(&nonce).as_ref().to_vec();
        let out = provider
            .verify(&Credential::SshSignature {
                username: "alice".into(),
                nonce,
                signature,
            })
            .await;
        let p = out.principal().expect("authenticated");
        assert_eq!(p.id.as_str(), "user:alice");
        assert_eq!(p.user_id, "alice");
        assert_eq!(p.auth_method, AuthMethod::SshKey);
        assert!(p.grants.permits(Resource::System, Verb::Read));
        assert!(!p.grants.permits(Resource::Config, Verb::Update));
    }

    #[tokio::test]
    async fn wrong_key_signature_is_denied() {
        let (_, entry) = test_keypair();
        let (other_key, _) = test_keypair();
        let provider = SshKeyAuthProvider::new(roster_with(entry));
        let nonce = b"server-issued-nonce".to_vec();
        let signature = other_key.sign(&nonce).as_ref().to_vec();
        let out = provider
            .verify(&Credential::SshSignature {
                username: "alice".into(),
                nonce,
                signature,
            })
            .await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn signature_over_different_nonce_is_denied() {
        let (key, entry) = test_keypair();
        let provider = SshKeyAuthProvider::new(roster_with(entry));
        let signature = key.sign(b"a-different-nonce").as_ref().to_vec();
        let out = provider
            .verify(&Credential::SshSignature {
                username: "alice".into(),
                nonce: b"server-issued-nonce".to_vec(),
                signature,
            })
            .await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn unknown_user_is_denied() {
        let (key, entry) = test_keypair();
        let provider = SshKeyAuthProvider::new(roster_with(entry));
        let nonce = b"server-issued-nonce".to_vec();
        let signature = key.sign(&nonce).as_ref().to_vec();
        let out = provider
            .verify(&Credential::SshSignature {
                username: "mallory".into(),
                nonce,
                signature,
            })
            .await;
        assert!(matches!(
            out,
            AuthOutcome::Denied {
                reason: DenyReason::BadCredential
            }
        ));
    }

    #[tokio::test]
    async fn empty_nonce_or_signature_is_denied() {
        let (key, entry) = test_keypair();
        let provider = SshKeyAuthProvider::new(roster_with(entry));
        let out = provider
            .verify(&Credential::SshSignature {
                username: "alice".into(),
                nonce: Vec::new(),
                signature: key.sign(b"x").as_ref().to_vec(),
            })
            .await;
        assert!(!out.is_allowed());
        let out = provider
            .verify(&Credential::SshSignature {
                username: "alice".into(),
                nonce: b"nonce".to_vec(),
                signature: Vec::new(),
            })
            .await;
        assert!(!out.is_allowed());
    }

    #[tokio::test]
    async fn only_ssh_signature_credentials_are_accepted() {
        let (_, entry) = test_keypair();
        let provider = SshKeyAuthProvider::new(roster_with(entry));
        assert!(!provider.accepts(&Credential::Bearer("tok".into())));
        assert!(!provider.accepts(&Credential::Peercred { uid: 1000 }));
        assert!(!provider.accepts(&Credential::None));
        assert!(provider.accepts(&Credential::SshSignature {
            username: "alice".into(),
            nonce: vec![1],
            signature: vec![2],
        }));
    }

    #[test]
    fn malformed_authorized_keys_entry_never_verifies() {
        assert!(!verify_with_key("garbage", b"m", b"s"));
        assert!(!verify_with_key("ssh-ed25519 !!!notbase64!!!", b"m", b"s"));
        assert!(!verify_with_key("ssh-rsa AAAA unsupported", b"m", b"s"));
    }
}
