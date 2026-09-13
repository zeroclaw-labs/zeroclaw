//! `x-opencode-session` — OpenCode relay session-affinity header.
//!
//! The OpenCode relay (`opencode.ai`, serving both the Zen and Go endpoints)
//! uses `x-opencode-session` to pin the requests of one conversation to the
//! same upstream backend, which is what keeps that backend's prompt cache warm
//! across the turns of a conversation. Upstream documents the header under its
//! guidance for keeping an account from being flagged, and at least one Go
//! model (`deepseek-v4-flash`) rejects header-less requests outright with
//! HTTP 400 `Model is unavailable`.
//!
//! Every OpenCode request — both wires, streaming and non-streaming — resolves
//! its value through [`session_token`] so the header cannot drift per code
//! path.
//!
//! # Value derivation
//!
//! Upstream specifies no format, length, or lifetime for the value; its only
//! normative statement is that a tool "includes the `x-opencode-session`
//! header so we can optimize prompt caching". The one datum on accepted values
//! is that a random UUID is sufficient to turn the 400 above into a 200. This
//! module therefore emits a 128-bit lowercase hex digest: opaque, fixed-length,
//! unambiguously safe as an HTTP header value, and non-reversible.
//!
//! The digest is taken over the ambient conversation scope
//! ([`zeroclaw_api::TOOL_LOOP_SESSION_KEY`]) rather than over a cached random
//! token, so the value is resolved from the canonical source at use time and
//! this module holds no per-session lookup table.
//!
//! # Why the scope is hashed rather than sent
//!
//! Session keys embed channel and user identifiers — `sanitize_session_key`
//! covers inputs shaped like `whatsapp_123@g.us_alice` and
//! `slack_C123_1.2_user one`. Forwarding one verbatim would hand a third-party
//! relay a cross-service-linkable per-user identifier, which the privacy
//! contract in `docs/book/src/contributing/privacy.md` forbids. Hashing keeps
//! the affinity behavior while sending nothing but an opaque token.
//!
//! The digest is domain-separated but unsalted, so it is deterministic across
//! restarts — which is what preserves cache affinity for a conversation that
//! spans a daemon restart. The tradeoff is explicit: this prevents plaintext
//! exposure of channel and user identifiers, but it is not a defense against a
//! party who knows ZeroClaw's session-key format brute-forcing a low-entropy
//! key space. Defeating that would require a per-install salt, which would cost
//! the cross-restart affinity this is for; it is deliberately out of scope here.

use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// The affinity header OpenCode reads to pin a conversation to one backend.
pub const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

/// Registrable domain of the OpenCode relay.
const OPENCODE_HOST: &str = "opencode.ai";

/// Domain-separation tag mixed into every digest, so a value emitted here can
/// never collide with a digest this codebase derives for another purpose.
const AFFINITY_DOMAIN: &str = "zeroclaw.opencode.session.v1";

/// Bytes of SHA-256 output kept. 128 bits is far beyond what backend selection
/// needs and keeps the header short.
const TOKEN_BYTES: usize = 16;

/// Extract the host from `base_url`, tolerating a missing scheme, userinfo, a
/// port, and an IPv6 literal.
///
/// Hand-rolled rather than pulled from a URL crate because this is the only
/// parsing this crate needs and the accepted shapes are covered by tests.
fn host_of(base_url: &str) -> Option<&str> {
    let after_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_scheme, rest)| rest);
    // The authority ends at the first path, query, or fragment delimiter.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())?;
    // `user:pass@host` — the last '@' separates userinfo from the host, so a
    // password containing '@' cannot smuggle a host past this.
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_userinfo, host)| host);

    let host = if let Some(after_bracket) = host_port.strip_prefix('[') {
        // IPv6 literal. Never the OpenCode relay, but must not be mis-split on
        // the colons inside the address.
        after_bracket.split_once(']').map(|(host, _port)| host)?
    } else {
        host_port
            .split_once(':')
            .map_or(host_port, |(host, _port)| host)
    };

    // A fully-qualified name may carry a trailing dot; `opencode.ai.` is the
    // same host as `opencode.ai`.
    let host = host.trim_end_matches('.');
    (!host.is_empty()).then_some(host)
}

/// True when `base_url` addresses the OpenCode relay.
///
/// Matches `opencode.ai` and any subdomain of it, so the built-in Zen and Go
/// endpoints and an operator's `uri` override all resolve alike. Matching is
/// on the parsed host, so a lookalike such as `opencode.ai.example.com` or
/// `notopencode.ai` is correctly rejected.
#[must_use]
pub fn is_opencode_target(base_url: &str) -> bool {
    host_of(base_url).is_some_and(|host| {
        let host = host.to_ascii_lowercase();
        host == OPENCODE_HOST || host.ends_with(&format!(".{OPENCODE_HOST}"))
    })
}

/// Domain-separated, truncated SHA-256 of one affinity scope.
fn digest_scope(scope: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(AFFINITY_DOMAIN.as_bytes());
    // A length-free separator would let a crafted scope reproduce another
    // domain's preimage; a NUL cannot appear in the tag, so it terminates it
    // unambiguously.
    hasher.update([0u8]);
    hasher.update(scope.as_bytes());
    hex::encode(&hasher.finalize()[..TOKEN_BYTES])
}

/// Affinity token for requests made outside any conversation scope.
///
/// Warmup probes and other calls that never enter the agent loop still have to
/// carry a header, or the Go models that reject header-less requests would fail
/// on exactly those paths. One process-stable random token keeps them pinned
/// together without inventing a conversation identity for them.
fn process_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| digest_scope(&uuid::Uuid::new_v4().to_string()))
}

/// Affinity token for the calling conversation, or `None` when `base_url` is
/// not an OpenCode target.
///
/// # Call site placement
///
/// This reads a `tokio` task-local, which **does not** cross `tokio::spawn`.
/// The streaming provider paths build their requests inside
/// `zeroclaw_spawn::spawn!`, whose macro propagates only the tracing span, so
/// calling this from inside a spawned task would silently fall back to
/// the process-stable fallback and lose per-conversation affinity. Resolve the
/// value before the spawn and move it in.
#[must_use]
pub fn session_token(base_url: &str) -> Option<String> {
    if !is_opencode_target(base_url) {
        return None;
    }
    let scope = zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|key| !key.trim().is_empty());
    Some(match scope {
        Some(key) => digest_scope(&key),
        None => process_token().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_builtin_zen_and_go_endpoints() {
        assert!(is_opencode_target("https://opencode.ai/zen/v1"));
        assert!(is_opencode_target("https://opencode.ai/zen/go/v1"));
        assert!(is_opencode_target("https://opencode.ai/zen/v1/responses"));
    }

    #[test]
    fn matches_subdomains_and_ignores_case_port_and_trailing_dot() {
        assert!(is_opencode_target("https://api.opencode.ai/v1"));
        assert!(is_opencode_target("https://OpenCode.AI/zen/v1"));
        assert!(is_opencode_target("https://opencode.ai:443/zen/v1"));
        assert!(is_opencode_target("https://opencode.ai./zen/v1"));
        assert!(is_opencode_target("opencode.ai/zen/v1"));
    }

    #[test]
    fn rejects_lookalike_hosts() {
        // The registrable domain appears only as a path or a prefix here.
        assert!(!is_opencode_target(
            "https://opencode.ai.example.com/zen/v1"
        ));
        assert!(!is_opencode_target("https://notopencode.ai/zen/v1"));
        assert!(!is_opencode_target(
            "https://example.com/opencode.ai/zen/v1"
        ));
        // Userinfo must not be mistaken for the host.
        assert!(!is_opencode_target("https://opencode.ai@example.com/v1"));
        assert!(!is_opencode_target("https://api.openai.com/v1"));
        assert!(!is_opencode_target(""));
    }

    #[test]
    fn ipv6_literal_is_not_a_target_and_does_not_panic() {
        assert!(!is_opencode_target("http://[::1]:8080/v1"));
        assert_eq!(host_of("http://[::1]:8080/v1"), Some("::1"));
        // Unterminated bracket must yield no host rather than panicking.
        assert_eq!(host_of("http://[::1"), None);
    }

    #[test]
    fn non_opencode_target_yields_no_header() {
        assert!(session_token("https://api.openai.com/v1").is_none());
    }

    #[test]
    fn opencode_target_always_yields_a_token() {
        // Outside any conversation scope the process token still applies, so
        // warmup-style calls are never header-less.
        let token = session_token("https://opencode.ai/zen/go/v1")
            .expect("OpenCode target must always carry an affinity token");
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[tokio::test]
    async fn token_is_stable_per_scope_and_distinct_across_scopes() {
        async fn token_for(session_key: &str) -> String {
            zeroclaw_api::TOOL_LOOP_SESSION_KEY
                .scope(Some(session_key.to_string()), async {
                    session_token("https://opencode.ai/zen/v1").expect("token")
                })
                .await
        }

        let first = token_for("telegram_1001_alice").await;
        let again = token_for("telegram_1001_alice").await;
        let other = token_for("telegram_1002_bob").await;

        assert_eq!(first, again, "one conversation must pin to one backend");
        assert_ne!(
            first, other,
            "distinct conversations must not share a scope"
        );
    }

    #[tokio::test]
    async fn token_never_contains_the_session_key() {
        // The identifiers inside a session key must not leave the process.
        let session_key = "whatsapp_123@g.us_alice";
        let token = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some(session_key.to_string()), async {
                session_token("https://opencode.ai/zen/v1").expect("token")
            })
            .await;
        assert!(!token.contains("alice"));
        assert!(!token.contains("123"));
        assert!(!token.contains(session_key));
    }

    #[tokio::test]
    async fn blank_scope_falls_back_to_the_process_token() {
        let blank = zeroclaw_api::TOOL_LOOP_SESSION_KEY
            .scope(Some("   ".to_string()), async {
                session_token("https://opencode.ai/zen/v1").expect("token")
            })
            .await;
        assert_eq!(blank, process_token());
    }

    #[tokio::test]
    async fn conversation_scope_does_not_survive_a_spawn() {
        // Guards the call-site contract in `session_token`'s docs. If a change
        // ever moves resolution inside `zeroclaw_spawn::spawn!`, the streaming
        // paths silently stop distinguishing conversations rather than
        // failing, so assert the mechanism that makes that happen.
        async fn token_in_scope<F, Fut>(f: F) -> String
        where
            F: FnOnce() -> Fut,
            Fut: std::future::Future<Output = String>,
        {
            zeroclaw_api::TOOL_LOOP_SESSION_KEY
                .scope(Some("telegram_1001_alice".to_string()), f())
                .await
        }

        let before_spawn = token_in_scope(|| async {
            session_token("https://opencode.ai/zen/v1").expect("token")
        })
        .await;
        let inside_spawn = token_in_scope(|| async {
            // The production streaming paths spawn through this same macro, so
            // assert against it rather than a bare `tokio::spawn`.
            ::zeroclaw_spawn::spawn!(async {
                session_token("https://opencode.ai/zen/v1").expect("token")
            })
            .await
            .expect("join")
        })
        .await;

        assert_ne!(
            before_spawn, inside_spawn,
            "task-local scope must not appear to cross a spawn"
        );
        assert_eq!(
            inside_spawn,
            process_token(),
            "a spawned read falls back to the process token"
        );
    }

    #[test]
    fn digest_is_domain_separated() {
        // A scope that reproduces the tag's bytes must not collide with it.
        assert_ne!(digest_scope(""), digest_scope(AFFINITY_DOMAIN));
    }
}
