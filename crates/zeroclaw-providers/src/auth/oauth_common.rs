//! Common OAuth2 utilities shared across model_providers.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::num::NonZeroUsize;
use std::time::Duration;

pub(super) struct RefreshRetryPolicy {
    pub max_attempts: NonZeroUsize,
    pub base_delay_ms: u64,
}

pub(super) struct RefreshAttemptError<'a> {
    pub attempt: usize,
    pub max_attempts: usize,
    pub non_retryable: bool,
    pub should_retry: bool,
    pub error: &'a anyhow::Error,
}

pub(super) async fn refresh_with_retries<T, Operation, OperationFuture, Classify, Observe>(
    policy: RefreshRetryPolicy,
    mut operation: Operation,
    is_non_retryable: Classify,
    mut observe_error: Observe,
) -> anyhow::Result<T>
where
    Operation: FnMut() -> OperationFuture,
    OperationFuture: Future<Output = anyhow::Result<T>>,
    Classify: Fn(&anyhow::Error) -> bool,
    Observe: FnMut(RefreshAttemptError<'_>),
{
    let max_attempts = policy.max_attempts.get();
    let mut attempt = 1;

    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let non_retryable = is_non_retryable(&error);
                let should_retry = !non_retryable && attempt < max_attempts;
                observe_error(RefreshAttemptError {
                    attempt,
                    max_attempts,
                    non_retryable,
                    should_retry,
                    error: &error,
                });

                if !should_retry {
                    return Err(error);
                }
                if policy.base_delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(
                        policy.base_delay_ms * attempt as u64,
                    ))
                    .await;
                }
                attempt += 1;
            }
        }
    }
}

/// PKCE state container for OAuth2 authorization code flow.
#[derive(Debug, Clone)]
pub struct PkceState {
    pub code_verifier: String,
    pub code_challenge: String,
    pub state: String,
}

pub fn code_challenge_for_verifier(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a new PKCE state with cryptographically random values.
/// Creates a code verifier, derives the S256 code challenge, and generates
/// a random state parameter for CSRF protection.
pub fn generate_pkce_state() -> PkceState {
    let code_verifier = random_base64url(64);
    let code_challenge = code_challenge_for_verifier(&code_verifier);

    PkceState {
        code_verifier,
        code_challenge,
        state: random_base64url(24),
    }
}

/// Generate a cryptographically random base64url-encoded string.
pub fn random_base64url(byte_len: usize) -> String {
    use chacha20poly1305::aead::{OsRng, rand_core::RngCore};

    let mut bytes = vec![0_u8; byte_len];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// URL-encode a string using percent encoding (RFC 3986).
pub fn url_encode(input: &str) -> String {
    input
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect::<String>()
}

/// URL-decode a percent-encoded string.
pub fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = bytes[i + 1] as char;
                let lo = bytes[i + 2] as char;
                if let (Some(h), Some(l)) = (hi.to_digit(16), lo.to_digit(16))
                    && let Ok(value) = u8::try_from(h * 16 + l)
                {
                    out.push(value);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).to_string()
}

/// Parse URL query parameters into a BTreeMap.
/// Handles URL-encoded keys and values.
pub fn parse_query_params(input: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(url_decode(key), url_decode(value));
    }
    out
}

pub(super) fn is_structured_callback_input(input: &str, expected_path: &str) -> bool {
    let trimmed = input.trim();
    let (target, query) = trimmed.split_once('?').unwrap_or((trimmed, trimmed));
    let path = ["http://", "https://"]
        .iter()
        .find_map(|scheme| target.strip_prefix(scheme))
        .and_then(|authority_and_path| {
            authority_and_path
                .find('/')
                .map(|index| &authority_and_path[index..])
        })
        .unwrap_or(target);
    let has_callback_param = query.split('&').any(|param| {
        ["code=", "state=", "error="]
            .iter()
            .any(|prefix| param.starts_with(prefix))
    });

    path.trim_end_matches('/') == expected_path.trim_end_matches('/') || has_callback_param
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn callback_shape_distinguishes_structured_input_from_bare_codes() {
        for input in [
            "http://localhost:1455/auth/callback",
            "http://localhost:1455/auth/callback/",
            "/auth/callback",
            "code=abc",
            "state=xyz",
            "error=access_denied",
            "?code=abc&state=xyz",
            "https://example.test/other?code=abc&state=xyz",
        ] {
            assert!(
                is_structured_callback_input(input, "/auth/callback"),
                "{input}"
            );
        }

        for input in [
            "raw-code",
            "4/0AcvDMrC1234567890abcdef",
            "/opaque-code",
            "opaque?code",
            "opaque://code",
        ] {
            assert!(
                !is_structured_callback_input(input, "/auth/callback"),
                "{input}"
            );
        }
    }

    fn retry_policy(max_attempts: usize, base_delay_ms: u64) -> RefreshRetryPolicy {
        RefreshRetryPolicy {
            max_attempts: NonZeroUsize::new(max_attempts).expect("test policy must be nonzero"),
            base_delay_ms,
        }
    }

    #[tokio::test]
    async fn refresh_retries_transient_errors_and_reports_attempt_metadata() {
        let attempts = AtomicUsize::new(0);
        let mut observed = Vec::new();

        let value = refresh_with_retries(
            retry_policy(3, 0),
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if attempt < 3 {
                        anyhow::bail!("failure {attempt}");
                    }
                    Ok("refreshed")
                }
            },
            |_| false,
            |failure| {
                observed.push((
                    failure.attempt,
                    failure.max_attempts,
                    failure.non_retryable,
                    failure.should_retry,
                    failure.error.to_string(),
                ));
            },
        )
        .await
        .expect("third attempt should succeed");

        assert_eq!(value, "refreshed");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(
            observed,
            vec![
                (1, 3, false, true, "failure 1".to_string()),
                (2, 3, false, true, "failure 2".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn refresh_returns_last_error_when_attempts_are_exhausted() {
        let attempts = AtomicUsize::new(0);
        let mut retry_decisions = Vec::new();

        let error = refresh_with_retries::<(), _, _, _, _>(
            retry_policy(3, 0),
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                async move { anyhow::bail!("failure {attempt}") }
            },
            |_| false,
            |failure| retry_decisions.push(failure.should_retry),
        )
        .await
        .expect_err("all attempts should fail");

        assert_eq!(error.to_string(), "failure 3");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert_eq!(retry_decisions, vec![true, true, false]);
    }

    #[tokio::test]
    async fn refresh_stops_immediately_for_non_retryable_error() {
        let attempts = AtomicUsize::new(0);
        let mut observed = None;

        let error = refresh_with_retries::<(), _, _, _, _>(
            retry_policy(3, 0),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async { anyhow::bail!("invalid_grant") }
            },
            |_| true,
            |failure| {
                observed = Some((failure.attempt, failure.non_retryable, failure.should_retry));
            },
        )
        .await
        .expect_err("permanent failure should be returned");

        assert_eq!(error.to_string(), "invalid_grant");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(observed, Some((1, true, false)));
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_uses_linear_attempt_based_delays() {
        let attempts = AtomicUsize::new(0);
        let started_at = tokio::time::Instant::now();

        let value = refresh_with_retries(
            retry_policy(3, 350),
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if attempt < 3 {
                        anyhow::bail!("transient");
                    }
                    Ok("refreshed")
                }
            },
            |_| false,
            |_| {},
        )
        .await
        .expect("third attempt should succeed");

        assert_eq!(value, "refreshed");
        assert_eq!(
            tokio::time::Instant::now().duration_since(started_at),
            Duration::from_millis(1_050)
        );
    }

    #[test]
    fn pkce_generation_is_valid() {
        let pkce = generate_pkce_state();
        // Code verifier should be at least 43 chars (base64url of 32 bytes)
        assert!(pkce.code_verifier.len() >= 43);
        assert!(!pkce.code_challenge.is_empty());
        assert!(!pkce.state.is_empty());
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = generate_pkce_state();
        let expected = code_challenge_for_verifier(&pkce.code_verifier);
        assert_eq!(pkce.code_challenge, expected);
    }

    #[test]
    fn url_encode_basic() {
        assert_eq!(url_encode("hello"), "hello");
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("a=b&c=d"), "a%3Db%26c%3Dd");
    }

    #[test]
    fn url_decode_basic() {
        assert_eq!(url_decode("hello"), "hello");
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("hello+world"), "hello world");
        assert_eq!(url_decode("a%3Db%26c%3Dd"), "a=b&c=d");
    }

    #[test]
    fn url_encode_decode_roundtrip() {
        let original = "hello world! @#$%^&*()";
        let encoded = url_encode(original);
        let decoded = url_decode(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn parse_query_params_basic() {
        let params = parse_query_params("code=abc123&state=xyz");
        assert_eq!(params.get("code"), Some(&"abc123".to_string()));
        assert_eq!(params.get("state"), Some(&"xyz".to_string()));
    }

    #[test]
    fn parse_query_params_encoded() {
        let params = parse_query_params("name=hello%20world&value=a%3Db");
        assert_eq!(params.get("name"), Some(&"hello world".to_string()));
        assert_eq!(params.get("value"), Some(&"a=b".to_string()));
    }

    #[test]
    fn parse_query_params_empty() {
        let params = parse_query_params("");
        assert!(params.is_empty());
    }

    #[test]
    fn random_base64url_length() {
        let s = random_base64url(32);
        // base64url encodes 3 bytes to 4 chars, so 32 bytes = ~43 chars
        assert!(s.len() >= 42);
    }
}
