//! Per-conversation identity for provider-side cache affinity.
//!
//! Some backends keep a prompt cache warm only for requests that say which
//! conversation they belong to. The identity ZeroClaw has for a conversation is
//! its session key ([`zeroclaw_api::TOOL_LOOP_SESSION_KEY`]), and session keys
//! embed channel and user identifiers — `sanitize_session_key` covers inputs
//! shaped like `whatsapp_123@g.us_user_a`. Forwarding one verbatim would hand a
//! third party a cross-service-linkable per-user identifier, which the privacy
//! contract in `docs/book/src/contributing/privacy.md` forbids. Providers
//! therefore send a token derived from the scope, never the scope.

use zeroclaw_config::secrets::SecretStore;

/// Bytes of digest output kept in a token. 128 bits is far beyond what backend
/// selection needs and keeps the token short.
pub(crate) const TOKEN_BYTES: usize = 16;

/// The ambient conversation scope, or `None` outside one or when it is blank.
///
/// This reads a `tokio` task-local, which **does not** cross `tokio::spawn`.
/// The streaming provider paths build their requests inside
/// `zeroclaw_spawn::spawn!`, whose macro propagates only the tracing span, so a
/// read from inside a spawned task silently sees no conversation. Resolve the
/// value before the spawn and move it in.
pub(crate) fn scope() -> Option<String> {
    zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|key| !key.trim().is_empty())
}

/// Token for `scope`, keyed with the install secret, as lowercase hex; `None`
/// when the install has no secret key.
///
/// Session keys are structured and low-entropy, so a plain digest of one could
/// be confirmed by anyone who knows the format and hashes candidates. Keying
/// the digest with the install secret removes that: the recipient cannot test
/// a guess without a secret that never leaves the host. The secret is
/// persistent, so the token is the same after a daemon restart, which is what
/// lets a resumed conversation find its cache; it differs between installs.
///
/// A missing key is never provisioned from here. The caller sends no token,
/// which costs the affinity and exposes nothing.
pub(crate) fn keyed_token(secrets: &SecretStore, domain: &str, scope: &str) -> Option<String> {
    let digest = secrets
        .keyed_digest(domain.as_bytes(), scope.as_bytes())
        .ok()?;
    Some(hex::encode(&digest[..TOKEN_BYTES]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_with_key() -> (tempfile::TempDir, SecretStore) {
        let dir = tempfile::tempdir().unwrap();
        let secrets = SecretStore::new(dir.path(), true);
        secrets
            .keyed_digest_or_create(b"zeroclaw.test.provision", b"")
            .expect("provision the install key");
        (dir, secrets)
    }

    #[test]
    fn keyed_token_is_stable_per_install_and_differs_between_installs() {
        let (_dir_a, install_a) = install_with_key();
        let (_dir_b, install_b) = install_with_key();
        let scope = "telegram_1001_user_a";

        let token = keyed_token(&install_a, "zeroclaw.test.one.v1", scope).expect("token");
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
        assert_eq!(
            keyed_token(&install_a, "zeroclaw.test.one.v1", scope),
            Some(token.clone()),
            "one conversation on one install must keep one token"
        );
        assert_ne!(
            keyed_token(&install_b, "zeroclaw.test.one.v1", scope),
            Some(token.clone()),
            "the same session key on another install must not yield the same token"
        );
        assert_ne!(
            keyed_token(&install_a, "zeroclaw.test.two.v1", scope),
            Some(token),
            "one scope must not yield one token in two domains"
        );
    }

    #[test]
    fn keyed_token_is_absent_without_an_install_key_and_does_not_create_one() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = SecretStore::new(dir.path(), true);

        assert_eq!(
            keyed_token(&secrets, "zeroclaw.test.one.v1", "gw_one"),
            None
        );
        assert!(
            !dir.path().join(".secret_key").exists(),
            "deriving a token must not provision the install key"
        );
    }

    #[tokio::test]
    async fn scope_is_absent_outside_a_conversation_and_when_blank() {
        assert_eq!(scope(), None);
        for blank in [None, Some("   ".to_string())] {
            let seen = zeroclaw_api::TOOL_LOOP_SESSION_KEY
                .scope(blank, async { scope() })
                .await;
            assert_eq!(seen, None);
        }
        let seen = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("gw_one".to_string()), async { scope() })
            .await;
        assert_eq!(seen.as_deref(), Some("gw_one"));
    }
}
