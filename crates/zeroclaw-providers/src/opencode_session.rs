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
//! Every OpenCode inference request, on both wires and on both streaming and
//! non-streaming paths, resolves its value through [`session_token`] so the
//! header cannot drift per code path. Model-catalog and warmup requests
//! (`GET /models`) do not carry it: they are not a conversation turn, so there
//! is no backend to pin.
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

/// Domain name the HTTP client will connect to for `base_url`, or `None` when
/// there is none: an unparseable or scheme-less URL, or an IP literal.
///
/// Parsed with `reqwest::Url`, the same WHATWG parser reqwest applies to every
/// request URL, so the host classified here is the host the request reaches. A
/// hand-rolled split disagrees with that parser on inputs such as
/// `https://relay.example\@opencode.ai` (the `\` ends the authority, so the
/// request goes to `relay.example`) and `https://%6fpencode.ai` (decoded to
/// `opencode.ai`), which would send the header off-relay or drop it on-relay.
fn host_of(base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    // The parser keeps a fully-qualified name's trailing dot; `opencode.ai.`
    // is the same host as `opencode.ai`.
    let host = url.domain()?.trim_end_matches('.');
    (!host.is_empty()).then(|| host.to_string())
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

/// True when `extra_headers` pins an `x-opencode-session` value that will
/// actually reach the wire.
///
/// The provider client builders skip an `extra_headers` entry whose value is not
/// a valid HTTP header value, logging a warning. Such an entry must not count as
/// a pin: suppressing the derived token for it would leave the request with no
/// affinity header at all.
#[must_use]
pub fn operator_pinned_session(extra_headers: &std::collections::HashMap<String, String>) -> bool {
    extra_headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case(OPENCODE_SESSION_HEADER)
            && reqwest::header::HeaderValue::from_str(value).is_ok()
    })
}

/// Redirect hops an OpenCode client follows before giving up; reqwest's
/// default limit.
const MAX_REDIRECTS: usize = 10;

/// Redirect policy for clients whose requests may carry the session header.
///
/// On a redirect to a different host, reqwest strips only credential headers
/// (`Authorization`, cookies, and proxy credentials), so under its default
/// policy a 3xx from an OpenCode host would carry `x-opencode-session` wherever
/// it points. This policy follows same-host redirects up to reqwest's default
/// limit and stops at the first hop to another host or port, handing that 3xx
/// back to the caller instead.
#[must_use]
pub fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let crosses_host = attempt.previous().last().is_some_and(|previous| {
            previous.host_str() != attempt.url().host_str()
                || previous.port_or_known_default() != attempt.url().port_or_known_default()
        });
        if crosses_host {
            attempt.stop()
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

/// Apply [`redirect_policy`] to `builder` when `endpoint` is an OpenCode target.
/// Every other provider keeps reqwest's default redirect handling.
pub fn restrict_redirects(
    builder: reqwest::ClientBuilder,
    endpoint: &str,
) -> reqwest::ClientBuilder {
    if is_opencode_target(endpoint) {
        builder.redirect(redirect_policy())
    } else {
        builder
    }
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

/// Affinity token for inference requests made outside any conversation scope.
///
/// Model calls that never enter the agent loop still have to carry a header, or
/// the Go models that reject header-less requests would fail on exactly those
/// paths. One process-stable random token keeps them pinned together without
/// inventing a conversation identity for them.
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
    }

    #[test]
    fn classifies_the_host_the_request_reaches() {
        // Under the parser reqwest uses, `\` ends the authority: the request
        // goes to `relay.example` and must not carry the header.
        assert_eq!(
            host_of("https://relay.example\\@opencode.ai/v1").as_deref(),
            Some("relay.example")
        );
        assert!(!is_opencode_target(
            "https://relay.example\\@opencode.ai/v1"
        ));
        // A percent-encoded host decodes to the relay and must carry it.
        assert_eq!(
            host_of("https://%6fpencode.ai/v1").as_deref(),
            Some("opencode.ai")
        );
        assert!(is_opencode_target("https://%6fpencode.ai/v1"));
        // reqwest cannot send a scheme-less URL, so there is no destination.
        assert!(!is_opencode_target("opencode.ai/zen/v1"));
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
        assert_eq!(host_of("http://[::1]:8080/v1"), None);
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
        // inference calls made outside the agent loop are never header-less.
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
    fn only_a_valid_pinned_value_counts_as_an_operator_pin() {
        let pin = |name: &str, value: &str| {
            std::collections::HashMap::from([(name.to_string(), value.to_string())])
        };
        assert!(operator_pinned_session(&pin(
            "x-opencode-session",
            "fixed-scope"
        )));
        assert!(operator_pinned_session(&pin(
            "X-Opencode-Session",
            "fixed-scope"
        )));
        // The client builders drop a value that is not a valid header value, so
        // it must not suppress the derived token.
        assert!(!operator_pinned_session(&pin(
            "x-opencode-session",
            "bad\nvalue"
        )));
        assert!(!operator_pinned_session(&pin(
            "x-other-header",
            "fixed-scope"
        )));
    }

    #[tokio::test]
    async fn redirect_policy_follows_same_host_and_stops_cross_host() {
        use axum::{Router, http::StatusCode, response::Redirect, routing::get};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        async fn serve(app: Router) -> std::net::SocketAddr {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("local addr");
            let _server = ::zeroclaw_spawn::spawn!(async move {
                axum::serve(listener, app).await.expect("serve");
            });
            addr
        }

        let elsewhere_hits = Arc::new(AtomicUsize::new(0));
        let hits = Arc::clone(&elsewhere_hits);
        let elsewhere = serve(Router::new().route(
            "/collect",
            get(move || {
                let hits = Arc::clone(&hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        ))
        .await;
        let cross_target = format!("http://{elsewhere}/collect");
        let origin = serve(
            Router::new()
                .route("/same", get(|| async { Redirect::temporary("/final") }))
                .route("/final", get(|| async { StatusCode::OK }))
                .route(
                    "/cross",
                    get(move || {
                        let target = cross_target.clone();
                        async move { Redirect::temporary(&target) }
                    }),
                ),
        )
        .await;

        let restricted = reqwest::Client::builder()
            .redirect(redirect_policy())
            .build()
            .expect("client");
        let send = |client: &reqwest::Client, path: &str| {
            client
                .get(format!("http://{origin}{path}"))
                .header(OPENCODE_SESSION_HEADER, "affinity-token")
                .send()
        };

        let same = send(&restricted, "/same").await.expect("same-host request");
        assert_eq!(
            same.status(),
            StatusCode::OK,
            "same-host redirects still follow"
        );

        let cross = send(&restricted, "/cross")
            .await
            .expect("cross-host request");
        assert_eq!(
            cross.status(),
            StatusCode::TEMPORARY_REDIRECT,
            "a cross-host redirect is handed back, not followed"
        );
        assert_eq!(
            elsewhere_hits.load(Ordering::SeqCst),
            0,
            "the header must not reach the redirect target"
        );

        // Control: reqwest's default policy does follow it, so the assertion
        // above is what keeps the header on the origin.
        let default_client = reqwest::Client::new();
        let followed = send(&default_client, "/cross")
            .await
            .expect("default request");
        assert_eq!(followed.status(), StatusCode::OK);
        assert_eq!(elsewhere_hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn digest_is_domain_separated() {
        // A scope that reproduces the tag's bytes must not collide with it.
        assert_ne!(digest_scope(""), digest_scope(AFFINITY_DOMAIN));
    }
}
