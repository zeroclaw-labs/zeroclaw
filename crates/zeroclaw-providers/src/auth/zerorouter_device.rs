//! ZeroRouter device-flow login (RFC 8628).
//!
//! Structurally the sibling of `xai_oauth`'s device arm, with the vendor
//! pinning inverted: nothing here names a fixed host. The issuer is the
//! operator's own router — derived from the configured
//! `providers.models.zerorouter` URI (or the family default) by stripping
//! the `/v1` API suffix — discovery comes from the issuer's
//! `/.well-known/openid-configuration`, and every discovered endpoint must
//! stay on the issuer's own origin (scheme + host + port). HTTPS is
//! required except for loopback issuers, so the self-hosted default
//! (`http://localhost:8080`) works while a spoofed discovery document
//! cannot redirect the poll off-box over plain HTTP. That origin pinning
//! only holds if no HTTP redirect is followed, so this flow uses its own
//! client that refuses them ([`device_flow_client`]).
//!
//! The "access token" this flow yields is not an OAuth token: ZeroRouter
//! mints a fresh `zcr_` API key at claim time (the plaintext never rests in
//! the router's database) with no expiry and no refresh arm. It is stored
//! as a Token-kind auth profile, not a `TokenSet`.

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::redirect::Policy;
use serde::Deserialize;
use std::time::{Duration, Instant};

/// Client id ZeroRouter allowlists by default (`ZEROROUTER_DEVICE_CLIENT_IDS`).
pub const ZEROROUTER_DEVICE_CLIENT_ID: &str = "zeroclaw";
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// RFC 8628 default poll interval when the router does not name one.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
/// Ceiling on the authorization lifetime the router is allowed to advertise.
/// `expires_in` is server-controlled, so it is clamped rather than trusted:
/// an oversized value would otherwise overflow `Instant + Duration` (a
/// panic) or keep the CLI polling far past any plausible approval window.
const MAX_DEVICE_LIFETIME_SECS: u64 = 30 * 60;
/// Ceiling on the poll interval, including `slow_down` back-off. Also
/// server-controlled, so it is clamped for the same reason.
const MAX_POLL_INTERVAL_SECS: u64 = 60;
/// Ceiling on how much remote text is copied into a CLI error.
const MAX_REMOTE_DETAIL_CHARS: usize = 200;
/// Ceiling on how many bytes of a remote body are buffered at all, before
/// any of it is displayed or parsed. An OAuth error object is tens of bytes;
/// a body that needs more than this is not one this flow can act on.
const MAX_REMOTE_BODY_BYTES: usize = 8 * 1024;
/// Ceiling on the length of a displayed RFC 8628 user code. The operator
/// types it into a browser, so it is short by construction (ZeroRouter mints
/// eight characters plus a group separator); the bound is generous enough
/// for other grouped forms and still far too small to flood a terminal.
const MAX_USER_CODE_CHARS: usize = 64;
/// Timeout for the one-shot discovery and device-start requests. Polling
/// gets a tighter, lifetime-derived bound of its own.
const SETUP_REQUEST_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct ZerorouterDeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone)]
pub struct ZerorouterDeviceDiscovery {
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct DiscoveryResponse {
    device_authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// The issuer for a configured ZeroRouter provider URI: the `/v1` API base
/// minus the `/v1`. Tolerates a trailing slash and a bare host.
pub fn issuer_from_provider_uri(uri: &str) -> String {
    let trimmed = uri.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v1")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

/// True only for a host that genuinely cannot leave this machine: the exact
/// name `localhost`, or an IP literal whose own loopback property says so.
/// A prefix test on `127.` is not that test — `127.example.com` is an
/// ordinary DNS name its owner can point anywhere, and accepting it handed
/// an attacker-controlled host the plaintext-HTTP exception.
///
/// A host that does not parse as an IP literal is not loopback, so anything
/// unrecognized fails closed into the HTTPS requirement. That includes
/// IPv4-mapped IPv6 forms such as `[::ffff:127.0.0.1]`, which `is_loopback`
/// reports as false; the self-hosted default has no need of them.
fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    // A URL host position carries IPv6 literals bracketed.
    let literal = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    literal
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// Every endpoint this flow talks to must live on the issuer's own origin:
/// same scheme, host, and port. HTTPS is required unless the issuer itself
/// is loopback (the self-hosted default). This is the parameterized
/// equivalent of the xAI flow's `require_trusted_endpoint` — trust follows
/// the operator's configured issuer instead of a vendor domain.
pub fn require_issuer_origin(issuer: &str, endpoint: &str, label: &str) -> Result<String> {
    let issuer_url = reqwest::Url::parse(issuer)
        .with_context(|| format!("Invalid ZeroRouter issuer: {issuer}"))?;
    // `endpoint` is remote text on every call but the issuer's own, so it is
    // sanitized wherever it is echoed, parsed or not.
    let url = reqwest::Url::parse(endpoint).with_context(|| {
        format!(
            "Invalid ZeroRouter {label}: {}",
            sanitize_remote_detail(endpoint)
        )
    })?;
    let issuer_host = issuer_url.host_str().unwrap_or_default().to_string();
    if issuer_url.scheme() != "https" && !is_loopback_host(&issuer_host) {
        anyhow::bail!("ZeroRouter issuer must be HTTPS (or loopback for self-hosted): {issuer}");
    }
    if url.scheme() != issuer_url.scheme()
        || url.host_str().unwrap_or_default() != issuer_host
        || url.port_or_known_default() != issuer_url.port_or_known_default()
    {
        anyhow::bail!(
            "ZeroRouter discovery returned {label} off the issuer's origin: {} (issuer {issuer})",
            sanitize_remote_detail(endpoint)
        );
    }
    // The parsed URL, never the remote string. Parsing already neutralized
    // what makes a URL dangerous to print — the WHATWG parser drops tabs and
    // newlines and percent-encodes C0 controls — but only its serialization
    // carries that through to the caller, which both dials and prints this.
    Ok(url.to_string())
}

/// The RFC 8628 user code, checked before it can be printed.
///
/// It is short, router-supplied, and displayed verbatim for the operator to
/// type into a browser, so it gets a charset allowlist rather than a
/// stripping pass: a code that arrives mangled is not usable anyway, and
/// silently rewriting it would show the operator something to type that the
/// router will not recognize. ZeroRouter mints `XXXX-XXXX`; the allowlist is
/// widened to ASCII alphanumerics plus `-` and `_` so it covers RFC 8628
/// §6.1's recommended alphabet and the grouped forms other routers emit,
/// and nothing that can move a terminal cursor.
fn validate_user_code(raw: &str) -> Result<String> {
    let code = raw.trim();
    if code.is_empty()
        || code.chars().count() > MAX_USER_CODE_CHARS
        || !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        anyhow::bail!(
            "ZeroRouter returned an unusable user code (expected up to {MAX_USER_CODE_CHARS} \
             characters of A-Z, a-z, 0-9, `-` or `_`): {}",
            sanitize_remote_detail(raw)
        );
    }
    Ok(code.to_string())
}

/// The canonical identity of a ZeroRouter issuer, as a comparable string.
///
/// Identity is the *whole* endpoint, not its origin: `base.uri` is the full
/// operator-supplied URL, so one origin can front several independently
/// operated routers behind path prefixes
/// (`https://gateway.example/router-a` and `.../router-b`). Scheme, host and
/// effective port are normalized because a URL says the same thing either
/// way; the path is normalized only for a trailing slash, because that is
/// the only path difference that is not a different location. Percent
/// encoding is compared as written — `%2D` and `-` differ here, which
/// refuses a key rather than widening a match.
///
/// A string that does not parse falls back to itself, so an operator typo
/// still matches only itself and never a parsed URL.
fn issuer_identity(raw: &str) -> String {
    let trimmed = raw.trim();
    let Ok(url) = reqwest::Url::parse(trimmed) else {
        return trimmed.trim_end_matches('/').to_string();
    };
    format!(
        "{}://{}:{}{}{}",
        url.scheme(),
        url.host_str().unwrap_or_default().to_ascii_lowercase(),
        url.port_or_known_default().unwrap_or_default(),
        url.path().trim_end_matches('/'),
        url.query().map(|q| format!("?{q}")).unwrap_or_default(),
    )
}

/// True when two URLs name the same ZeroRouter deployment — the comparison
/// that binds a stored key to the router that minted it. The profile records
/// its issuer at login, and credential consumption checks that record
/// against the request's own base URL, so this must distinguish two routers
/// that share an origin.
///
/// Deliberately stricter than the origin check in [`require_issuer_origin`]:
/// that one constrains where a *discovered* endpoint may live (necessarily
/// origin-scoped, since a router's own endpoints need not share its path
/// prefix), while this one decides who may be handed a secret.
pub fn issuer_identities_match(left: &str, right: &str) -> bool {
    issuer_identity(left) == issuer_identity(right)
}

/// HTTP client for the device flow. It refuses every redirect:
/// [`require_issuer_origin`] pins the URL this process dials, but a
/// followed 307/308 preserves the POST body, so a same-origin endpoint
/// could hand `key_name` or `device_code` to a host the origin check never
/// saw. No hop is followed, so there is no hop to re-validate.
pub fn device_flow_client() -> Result<Client> {
    Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(SETUP_REQUEST_TIMEOUT_SECS))
        .build()
        .context("Failed to build the ZeroRouter device-flow HTTP client")
}

/// Refuse a 3xx rather than reporting it as a generic failure, so the
/// operator learns the router tried to move the request off its own origin.
/// The `Location` value is remote text and is deliberately not echoed.
fn reject_redirect(status: reqwest::StatusCode, issuer: &str, label: &str) -> Result<()> {
    if status.is_redirection() {
        anyhow::bail!(
            "ZeroRouter {label} answered with a redirect ({status}); this flow does not follow \
             redirects, so nothing left the issuer's origin ({issuer})"
        );
    }
    Ok(())
}

/// Remote text — an OAuth `error_description`, an error body — is untrusted
/// and lands in a terminal. Drop control characters (which carry ANSI escape
/// sequences and line breaks) and cap the length so a hostile router cannot
/// repaint or flood the console.
fn sanitize_remote_detail(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control())
        .take(MAX_REMOTE_DETAIL_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return "<no detail>".to_string();
    }
    if raw.chars().count() > MAX_REMOTE_DETAIL_CHARS {
        return format!("{trimmed} (truncated)");
    }
    trimmed.to_string()
}

/// Read at most [`MAX_REMOTE_BODY_BYTES`] of a remote body.
///
/// `Response::text` buffers whatever the router chooses to send and only
/// then can a cap apply, which leaves the allocation itself unbounded. The
/// bound therefore has to be enforced chunk by chunk, as the body arrives.
/// A body that ends early or errors mid-read still yields what did arrive:
/// the result is only ever fed to [`sanitize_remote_detail`] or to a JSON
/// parse whose failure is already a fail-closed path.
async fn read_bounded_body(mut response: reqwest::Response) -> String {
    let mut body: Vec<u8> = Vec::with_capacity(0);
    while body.len() < MAX_REMOTE_BODY_BYTES {
        let Ok(Some(chunk)) = response.chunk().await else {
            break;
        };
        let room = MAX_REMOTE_BODY_BYTES - body.len();
        body.extend_from_slice(&chunk[..room.min(chunk.len())]);
    }
    // Truncation can split a multi-byte character; the replacement character
    // it becomes is inert, and the caller sanitizes regardless.
    String::from_utf8_lossy(&body).into_owned()
}

/// How long this authorization may run, clamped to `max_lifetime`. Uses no
/// unchecked arithmetic: `expires_in` is server-controlled and
/// `Duration::from_secs(u64::MAX)` added to an `Instant` panics.
fn device_lifetime(expires_in: u64, max_lifetime: Duration) -> Duration {
    Duration::from_secs(expires_in.max(1)).min(max_lifetime)
}

/// Poll spacing, clamped at both ends. Also server-controlled.
fn poll_interval(interval: u64) -> Duration {
    Duration::from_secs(interval.clamp(1, MAX_POLL_INTERVAL_SECS))
}

pub async fn fetch_device_discovery(
    client: &Client,
    issuer: &str,
) -> Result<ZerorouterDeviceDiscovery> {
    // The issuer itself passes through the origin check first so a plain-HTTP
    // non-loopback issuer is rejected before any request leaves the machine.
    let discovery_url = require_issuer_origin(
        issuer,
        &format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ),
        "discovery document",
    )?;
    let response = client
        .get(&discovery_url)
        .header("Accept", "application/json")
        .send()
        .await
        .context("Failed to fetch ZeroRouter OAuth discovery")?;
    reject_redirect(response.status(), issuer, "discovery document")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = read_bounded_body(response).await;
        anyhow::bail!(
            "ZeroRouter OAuth discovery failed ({status}): {}",
            sanitize_remote_detail(&body)
        );
    }
    let parsed: DiscoveryResponse = response
        .json()
        .await
        .context("Failed to parse ZeroRouter OAuth discovery")?;
    let device_authorization_endpoint = require_issuer_origin(
        issuer,
        parsed
            .device_authorization_endpoint
            .as_deref()
            .ok_or_else(|| {
                anyhow::Error::msg("ZeroRouter discovery missing device_authorization_endpoint")
            })?,
        "device authorization endpoint",
    )?;
    let token_endpoint = require_issuer_origin(
        issuer,
        parsed
            .token_endpoint
            .as_deref()
            .ok_or_else(|| anyhow::Error::msg("ZeroRouter discovery missing token_endpoint"))?,
        "token endpoint",
    )?;
    Ok(ZerorouterDeviceDiscovery {
        device_authorization_endpoint,
        token_endpoint,
    })
}

/// Start the flow. `key_name` is ZeroRouter's RFC 8628 extension: the label
/// the minted key will carry in the portal (this machine's hostname).
pub async fn start_device_flow(
    client: &Client,
    issuer: &str,
    device_authorization_endpoint: &str,
    key_name: &str,
) -> Result<ZerorouterDeviceStart> {
    let form = [
        ("client_id", ZEROROUTER_DEVICE_CLIENT_ID),
        ("key_name", key_name),
    ];
    let response = client
        .post(device_authorization_endpoint)
        .form(&form)
        .send()
        .await
        .context("Failed to start ZeroRouter device-code flow")?;
    reject_redirect(response.status(), issuer, "device authorization endpoint")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = read_bounded_body(response).await;
        anyhow::bail!(
            "ZeroRouter device-code start failed ({status}): {}",
            sanitize_remote_detail(&body)
        );
    }
    let parsed: DeviceCodeResponse = response
        .json()
        .await
        .context("Failed to parse ZeroRouter device-code response")?;
    Ok(ZerorouterDeviceStart {
        device_code: parsed.device_code,
        // Printed for the operator to read back, so it is validated here
        // rather than at the print site: every field of this struct is
        // display-safe by construction.
        user_code: validate_user_code(&parsed.user_code)?,
        // The user opens this in a browser; it must not point off-router.
        verification_uri: require_issuer_origin(
            issuer,
            &parsed.verification_uri,
            "verification URI",
        )?,
        verification_uri_complete: parsed
            .verification_uri_complete
            .as_deref()
            .map(|uri| require_issuer_origin(issuer, uri, "complete verification URI"))
            .transpose()?,
        expires_in: parsed.expires_in,
        interval: parsed.interval.unwrap_or(DEFAULT_POLL_INTERVAL_SECS),
    })
}

/// Poll until approval. The returned string IS the minted `zcr_` API key.
/// The advertised lifetime is capped at `MAX_DEVICE_LIFETIME_SECS`.
pub async fn poll_device_key(
    client: &Client,
    issuer: &str,
    token_endpoint: &str,
    device: &ZerorouterDeviceStart,
) -> Result<String> {
    poll_device_key_within(
        client,
        issuer,
        token_endpoint,
        device,
        Duration::from_secs(MAX_DEVICE_LIFETIME_SECS),
    )
    .await
}

/// The polling loop with its lifetime ceiling supplied by the caller;
/// [`poll_device_key`] is the production entry point and passes
/// [`MAX_DEVICE_LIFETIME_SECS`]. Splitting the ceiling out keeps it a value
/// rather than a hard-coded constant inside the loop, so the bound itself is
/// exercisable rather than merely asserted.
///
/// The deadline is a bound on the whole loop, not just on the decision to
/// sleep: it is re-derived before each sleep and again before each request,
/// and it caps that request's own timeout. A router that stalls a response
/// therefore cannot hold the CLI past the lifetime it advertised.
pub(crate) async fn poll_device_key_within(
    client: &Client,
    issuer: &str,
    token_endpoint: &str,
    device: &ZerorouterDeviceStart,
    max_lifetime: Duration,
) -> Result<String> {
    let deadline = Instant::now()
        .checked_add(device_lifetime(device.expires_in, max_lifetime))
        .context("ZeroRouter device authorization lifetime is not representable")?;
    let mut interval = poll_interval(device.interval);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("ZeroRouter device authorization expired before approval");
        }
        tokio::time::sleep(interval.min(remaining)).await;

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("ZeroRouter device authorization expired before approval");
        }

        let form = [
            ("grant_type", DEVICE_CODE_GRANT_TYPE),
            ("device_code", device.device_code.as_str()),
            ("client_id", ZEROROUTER_DEVICE_CLIENT_ID),
        ];
        let response = client
            .post(token_endpoint)
            .form(&form)
            .timeout(remaining)
            .send()
            .await
            .context("Failed to poll ZeroRouter device token endpoint")?;
        reject_redirect(response.status(), issuer, "token endpoint")?;

        if response.status().is_success() {
            let parsed: TokenResponse = response
                .json()
                .await
                .context("Failed to parse ZeroRouter device token response")?;
            return Ok(parsed.access_token);
        }

        let status = response.status();
        let body = read_bounded_body(response).await;
        let Ok(error) = serde_json::from_str::<OAuthErrorResponse>(&body) else {
            anyhow::bail!(
                "ZeroRouter device token poll failed ({status}): {}",
                sanitize_remote_detail(&body)
            );
        };
        match error.error.as_str() {
            "authorization_pending" => {}
            // RFC 8628 §3.5: back off by 5 seconds on slow_down.
            "slow_down" => {
                interval = interval
                    .saturating_add(Duration::from_secs(5))
                    .min(Duration::from_secs(MAX_POLL_INTERVAL_SECS));
            }
            "access_denied" => anyhow::bail!("ZeroRouter device authorization was denied"),
            "expired_token" => {
                anyhow::bail!("ZeroRouter device authorization expired before approval")
            }
            // Every remaining arm reflects router-supplied text, so both the
            // code and its description are sanitized and capped first.
            other => anyhow::bail!(
                "ZeroRouter device token poll failed ({status}): {}{}",
                sanitize_remote_detail(other),
                error
                    .error_description
                    .map(|detail| format!(" — {}", sanitize_remote_detail(&detail)))
                    .unwrap_or_default()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A host the flow must never reach. Counts connections and keeps every
    /// body it was handed, so a test can assert on both.
    #[derive(Default)]
    struct OffOriginProbe {
        hits: AtomicUsize,
        bodies: Mutex<Vec<String>>,
    }

    async fn record_off_origin_request(
        State(probe): State<Arc<OffOriginProbe>>,
        body: String,
    ) -> impl IntoResponse {
        probe.hits.fetch_add(1, Ordering::SeqCst);
        probe
            .bodies
            .lock()
            .expect("off-origin probe bodies")
            .push(body);
        StatusCode::OK
    }

    async fn spawn_test_server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let handle = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.expect("serve test server");
        });
        // Loopback, so the flow's HTTPS requirement does not apply — this is
        // the self-hosted shape the origin check is written for.
        (format!("http://{addr}"), handle)
    }

    /// A same-origin endpoint that answers with a 307 to `target`: the exact
    /// shape that preserves a POST body across hosts.
    fn redirecting_router(path: &str, target: String) -> Router {
        Router::new().route(
            path,
            post(move || {
                let target = target.clone();
                async move { (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, target)]) }
            }),
        )
    }

    fn device_start(device_code: &str, expires_in: u64, interval: u64) -> ZerorouterDeviceStart {
        ZerorouterDeviceStart {
            device_code: device_code.to_string(),
            user_code: "USER-CODE".to_string(),
            verification_uri: "http://127.0.0.1/verify".to_string(),
            verification_uri_complete: None,
            expires_in,
            interval,
        }
    }

    #[tokio::test]
    async fn off_origin_redirect_never_receives_the_device_start_body() {
        let probe = Arc::new(OffOriginProbe::default());
        let (off_origin, off_origin_server) = spawn_test_server(
            Router::new()
                .route("/auth/device/code", post(record_off_origin_request))
                .with_state(Arc::clone(&probe)),
        )
        .await;
        let (issuer, issuer_server) = spawn_test_server(redirecting_router(
            "/auth/device/code",
            format!("{off_origin}/auth/device/code"),
        ))
        .await;

        let client = device_flow_client().expect("device-flow client");
        let error = start_device_flow(
            &client,
            &issuer,
            &format!("{issuer}/auth/device/code"),
            "secret-machine-label",
        )
        .await
        .expect_err("an off-origin redirect must not be followed");

        assert_eq!(
            probe.hits.load(Ordering::SeqCst),
            0,
            "the off-origin host was connected to despite the redirect being refused"
        );
        assert!(
            probe
                .bodies
                .lock()
                .expect("off-origin probe bodies")
                .is_empty(),
            "the off-origin host received a request body"
        );
        let message = error.to_string();
        assert!(
            message.contains("redirect"),
            "the refusal should name the redirect: {message}"
        );
        assert!(
            !message.contains("secret-machine-label"),
            "the key label must not be echoed back into the error: {message}"
        );

        issuer_server.abort();
        off_origin_server.abort();
    }

    #[tokio::test]
    async fn off_origin_redirect_never_receives_the_token_poll_body() {
        let probe = Arc::new(OffOriginProbe::default());
        let (off_origin, off_origin_server) = spawn_test_server(
            Router::new()
                .route("/token", post(record_off_origin_request))
                .with_state(Arc::clone(&probe)),
        )
        .await;
        let (issuer, issuer_server) =
            spawn_test_server(redirecting_router("/token", format!("{off_origin}/token"))).await;

        let client = device_flow_client().expect("device-flow client");
        let device = device_start("secret-device-code", 60, 1);
        let error = poll_device_key_within(
            &client,
            &issuer,
            &format!("{issuer}/token"),
            &device,
            Duration::from_secs(30),
        )
        .await
        .expect_err("an off-origin redirect must not be followed");

        assert_eq!(
            probe.hits.load(Ordering::SeqCst),
            0,
            "the off-origin host was connected to despite the redirect being refused"
        );
        assert!(
            probe
                .bodies
                .lock()
                .expect("off-origin probe bodies")
                .is_empty(),
            "the off-origin host received the device_code"
        );
        assert!(
            !error.to_string().contains("secret-device-code"),
            "the device code must not be echoed back into the error: {error}"
        );

        issuer_server.abort();
        off_origin_server.abort();
    }

    #[tokio::test]
    async fn oversized_expires_in_terminates_instead_of_panicking() {
        let (issuer, server) = spawn_test_server(Router::new().route(
            "/token",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "authorization_pending"})),
                )
            }),
        ))
        .await;

        let client = device_flow_client().expect("device-flow client");
        // The old code computed `Instant::now() + Duration::from_secs(expires_in)`,
        // which panics for a value this size.
        let device = device_start("device-code", u64::MAX, 1);
        let error = poll_device_key_within(
            &client,
            &issuer,
            &format!("{issuer}/token"),
            &device,
            Duration::from_millis(1500),
        )
        .await
        .expect_err("an oversized lifetime must end the poll, not extend it");

        assert!(
            error.to_string().contains("expired"),
            "expected a lifetime-expiry error, got: {error}"
        );

        server.abort();
    }

    /// The lifetime bounds the requests, not just the sleeps: once the
    /// sleep lands on the deadline, no further poll may be issued. With a
    /// 3s lifetime and a 1s interval there is room for polls at ~1s and
    /// ~2s; the sleep that ends at ~3s must produce an expiry instead of a
    /// third poll.
    #[tokio::test]
    async fn no_poll_is_issued_once_the_lifetime_is_spent() {
        let polls = Arc::new(AtomicUsize::new(0));
        let (issuer, server) = spawn_test_server(
            Router::new()
                .route(
                    "/token",
                    post(|State(polls): State<Arc<AtomicUsize>>| async move {
                        polls.fetch_add(1, Ordering::SeqCst);
                        (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({"error": "authorization_pending"})),
                        )
                    }),
                )
                .with_state(Arc::clone(&polls)),
        )
        .await;

        let client = device_flow_client().expect("device-flow client");
        let device = device_start("device-code", 3600, 1);
        let error = poll_device_key_within(
            &client,
            &issuer,
            &format!("{issuer}/token"),
            &device,
            Duration::from_secs(3),
        )
        .await
        .expect_err("the authorization must expire");

        assert!(
            error.to_string().contains("expired"),
            "expected a lifetime-expiry error, got: {error}"
        );
        assert!(
            polls.load(Ordering::SeqCst) <= 2,
            "a poll was issued after the authorization lifetime ran out ({} polls in 3s at a \
             1s interval)",
            polls.load(Ordering::SeqCst)
        );

        server.abort();
    }

    #[tokio::test]
    async fn stalled_token_endpoint_cannot_outlive_the_authorization() {
        let (issuer, server) = spawn_test_server(Router::new().route(
            "/token",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(600)).await;
                StatusCode::OK
            }),
        ))
        .await;

        let client = device_flow_client().expect("device-flow client");
        let device = device_start("device-code", 3600, 1);
        let started = Instant::now();
        let error = poll_device_key_within(
            &client,
            &issuer,
            &format!("{issuer}/token"),
            &device,
            Duration::from_secs(2),
        )
        .await
        .expect_err("a stalled token endpoint must not hang the login");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "the poll outlived the 2s authorization by far too much: {elapsed:?} ({error})"
        );

        server.abort();
    }

    /// A device-code endpoint that answers on its own origin (taken from the
    /// request's `Host`, so it always matches the issuer) with the fields a
    /// test supplies. Everything here is router-supplied text that the
    /// success path prints.
    fn device_code_router(user_code: &'static str, uri_suffix: &'static str) -> Router {
        Router::new().route(
            "/auth/device/code",
            post(move |headers: axum::http::HeaderMap| async move {
                let host = headers
                    .get(header::HOST)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Json(serde_json::json!({
                    "device_code": "device-code",
                    "user_code": user_code,
                    "verification_uri": format!("http://{host}/verify{uri_suffix}"),
                    "verification_uri_complete":
                        format!("http://{host}/verify{uri_suffix}?code=WDJB-MJHT"),
                    "expires_in": 600,
                }))
            }),
        )
    }

    /// The success path prints `user_code` verbatim, so a hostile code is a
    /// terminal-injection vector no different from an error body.
    #[tokio::test]
    async fn a_control_laden_user_code_never_reaches_the_terminal() {
        let (issuer, server) =
            spawn_test_server(device_code_router("\u{1b}[2JWDJB-MJHT", "")).await;

        let client = device_flow_client().expect("device-flow client");
        let error = start_device_flow(
            &client,
            &issuer,
            &format!("{issuer}/auth/device/code"),
            "machine-label",
        )
        .await
        .expect_err("a user code carrying terminal controls must be refused");
        assert!(
            !error.to_string().chars().any(char::is_control),
            "the refusal itself leaked the control characters: {error:?}"
        );

        server.abort();
    }

    /// The verification URIs are printed too, and the router controls their
    /// path even when the origin check passes.
    #[tokio::test]
    async fn control_characters_in_the_verification_uris_never_reach_the_terminal() {
        let (issuer, server) =
            spawn_test_server(device_code_router("WDJB-MJHT", "\u{1b}[2Jwiped")).await;

        let client = device_flow_client().expect("device-flow client");
        let device = start_device_flow(
            &client,
            &issuer,
            &format!("{issuer}/auth/device/code"),
            "machine-label",
        )
        .await
        .expect("a same-origin verification URI is accepted");

        for (label, printed) in [
            ("user_code", device.user_code.clone()),
            ("verification_uri", device.verification_uri.clone()),
            (
                "verification_uri_complete",
                device
                    .verification_uri_complete
                    .clone()
                    .expect("the router supplied a complete URI"),
            ),
        ] {
            assert!(
                !printed.chars().any(char::is_control),
                "{label} carried a control character to the terminal: {printed:?}"
            );
        }

        server.abort();
    }

    /// The cap has to bind on what is buffered, not on what is displayed:
    /// the display cap alone still let the router decide the allocation.
    #[tokio::test]
    async fn a_flooding_error_body_is_bounded_before_it_is_buffered() {
        let oversized = MAX_REMOTE_BODY_BYTES * 8;
        let (issuer, server) = spawn_test_server(Router::new().route(
            "/token",
            post(move || async move { (StatusCode::BAD_REQUEST, "a".repeat(oversized)) }),
        ))
        .await;

        let client = device_flow_client().expect("device-flow client");
        let response = client
            .post(format!("{issuer}/token"))
            .send()
            .await
            .expect("the flooding endpoint answers");
        let body = read_bounded_body(response).await;

        assert_eq!(
            body.len(),
            MAX_REMOTE_BODY_BYTES,
            "the remote body was buffered past the cap ({} bytes of an offered {oversized})",
            body.len()
        );

        server.abort();
    }

    #[test]
    fn device_lifetime_clamps_a_hostile_expires_in() {
        let cap = Duration::from_secs(MAX_DEVICE_LIFETIME_SECS);
        assert_eq!(device_lifetime(u64::MAX, cap), cap);
        assert_eq!(device_lifetime(0, cap), Duration::from_secs(1));
        assert_eq!(device_lifetime(60, cap), Duration::from_secs(60));
        // The clamped value must stay addable to an Instant; the unclamped
        // one is exactly what used to panic.
        assert!(
            Instant::now()
                .checked_add(device_lifetime(u64::MAX, cap))
                .is_some()
        );
    }

    #[test]
    fn poll_interval_is_clamped_at_both_ends() {
        assert_eq!(poll_interval(0), Duration::from_secs(1));
        assert_eq!(poll_interval(5), Duration::from_secs(5));
        assert_eq!(
            poll_interval(u64::MAX),
            Duration::from_secs(MAX_POLL_INTERVAL_SECS)
        );
    }

    #[test]
    fn remote_detail_is_stripped_and_capped() {
        let escape = "\u{1b}[2Jwiped\nthe screen";
        let cleaned = sanitize_remote_detail(escape);
        assert!(
            !cleaned.contains('\u{1b}'),
            "ANSI escape survived: {cleaned}"
        );
        assert!(!cleaned.contains('\n'), "newline survived: {cleaned}");
        assert!(cleaned.contains("wiped"));

        let flood = "a".repeat(MAX_REMOTE_DETAIL_CHARS * 4);
        let capped = sanitize_remote_detail(&flood);
        assert!(
            capped.chars().count() <= MAX_REMOTE_DETAIL_CHARS + " (truncated)".len(),
            "detail was not capped: {} chars",
            capped.chars().count()
        );

        assert_eq!(sanitize_remote_detail("   "), "<no detail>");
    }

    #[test]
    fn issuer_identities_compare_urls_not_strings() {
        assert!(issuer_identities_match(
            "https://router.example.com",
            "https://router.example.com/"
        ));
        assert!(issuer_identities_match(
            "https://Router.Example.com",
            "https://router.example.com"
        ));
        assert!(issuer_identities_match(
            "https://router.example.com:443",
            "https://router.example.com"
        ));
        assert!(!issuer_identities_match(
            "https://router-a.example.com",
            "https://router-b.example.com"
        ));
        assert!(!issuer_identities_match(
            "http://localhost:8080",
            "http://localhost:9090"
        ));
        assert!(!issuer_identities_match(
            "http://router.example.com",
            "https://router.example.com"
        ));
    }

    /// One origin can host several independently operated routers behind a
    /// path prefix, so the path is part of the issuer's identity for
    /// stored-key selection. Origin-only matching made router A's key
    /// selectable for router B.
    /// Each clause of the allowlist, pinned separately: the end-to-end
    /// terminal-safety test only exercises the charset arm.
    #[test]
    fn user_code_allowlist_covers_every_rejection_arm() {
        // ZeroRouter's own shape, and the ungrouped form.
        assert_eq!(
            validate_user_code("WDJB-MJHT").expect("the router's own code shape is accepted"),
            "WDJB-MJHT"
        );
        assert_eq!(
            validate_user_code("  WDJB-MJHT\n").expect("surrounding whitespace is trimmed"),
            "WDJB-MJHT"
        );
        assert_eq!(
            validate_user_code("wdjbmjht9").expect("an ungrouped alphanumeric code is accepted"),
            "wdjbmjht9"
        );

        validate_user_code("").expect_err("an empty code has nothing to display");
        validate_user_code("   ").expect_err("a whitespace-only code has nothing to display");
        validate_user_code(&"A".repeat(MAX_USER_CODE_CHARS + 1))
            .expect_err("an over-long code cannot be a user code");
        validate_user_code("WDJB MJHT").expect_err("an interior space is outside the allowlist");
        validate_user_code("\u{1b}[2JWDJB").expect_err("an ANSI escape is outside the allowlist");
        validate_user_code("WDJB\u{7f}MJHT").expect_err("DEL is outside the allowlist");

        // The refusal itself must not carry the hostile text through.
        let error = validate_user_code("\u{1b}[2Jwiped").expect_err("refused");
        assert!(
            !error.to_string().chars().any(char::is_control),
            "the refusal leaked control characters: {error:?}"
        );
    }

    #[test]
    fn issuer_identity_separates_routers_sharing_one_origin() {
        assert!(!issuer_identities_match(
            "https://gateway.example/router-a",
            "https://gateway.example/router-b"
        ));
        assert!(!issuer_identities_match(
            "https://gateway.example",
            "https://gateway.example/router-b"
        ));
        assert!(!issuer_identities_match(
            "https://gateway.example/router-a",
            "https://gateway.example/router-a/nested"
        ));
        // Trailing slashes and default ports are the same deployment.
        assert!(issuer_identities_match(
            "https://gateway.example/router-a",
            "https://gateway.example/router-a/"
        ));
        assert!(issuer_identities_match(
            "https://gateway.example:443/router-a/",
            "https://Gateway.Example/router-a"
        ));
    }

    #[test]
    fn plain_http_exception_needs_a_real_loopback_address() {
        // A DNS name that merely starts with `127.` is not loopback; it
        // resolves wherever its owner points it.
        require_issuer_origin(
            "http://127.example.com",
            "http://127.example.com/auth/device/code",
            "device authorization endpoint",
        )
        .expect_err("a `127.`-prefixed DNS name is not a loopback address");
        require_issuer_origin(
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/auth/device/code",
            "device authorization endpoint",
        )
        .expect("IPv4 loopback stays allowed");
        require_issuer_origin(
            "http://[::1]:8080",
            "http://[::1]:8080/auth/device/code",
            "device authorization endpoint",
        )
        .expect("IPv6 loopback stays allowed");
        require_issuer_origin(
            "http://localhost:8080",
            "http://localhost:8080/auth/device/code",
            "device authorization endpoint",
        )
        .expect("the exact `localhost` exception stays allowed");
    }

    /// The origin check parses the endpoint, so it must hand back what it
    /// parsed. Returning the remote string let terminal controls that the
    /// URL parser had already neutralized survive into the display.
    #[test]
    fn origin_check_returns_the_parsed_url_not_the_remote_string() {
        let checked = require_issuer_origin(
            "https://r.example.com",
            "https://r.example.com/verify\u{1b}[2Jwiped",
            "verification URI",
        )
        .expect("same origin passes");
        assert!(
            !checked.chars().any(char::is_control),
            "a control character survived the origin check: {checked:?}"
        );
    }

    #[test]
    fn issuer_derivation_strips_the_api_suffix() {
        assert_eq!(
            issuer_from_provider_uri("https://router.example.com/v1"),
            "https://router.example.com"
        );
        assert_eq!(
            issuer_from_provider_uri("http://localhost:8080/v1/"),
            "http://localhost:8080"
        );
        // A bare host stays as-is.
        assert_eq!(
            issuer_from_provider_uri("https://router.example.com"),
            "https://router.example.com"
        );
    }

    #[test]
    fn origin_check_pins_endpoints_to_the_issuer() {
        // Same origin: fine.
        require_issuer_origin(
            "https://r.example.com",
            "https://r.example.com/auth/device/token",
            "token endpoint",
        )
        .expect("same origin passes");
        // Different host: rejected — a spoofed discovery document must not
        // be able to send the poll (and the key) elsewhere.
        require_issuer_origin(
            "https://r.example.com",
            "https://evil.example.net/token",
            "token endpoint",
        )
        .expect_err("cross-origin endpoint is rejected");
        // Different port on the same host: rejected.
        require_issuer_origin(
            "http://localhost:8080",
            "http://localhost:9999/token",
            "token endpoint",
        )
        .expect_err("cross-port endpoint is rejected");
    }

    #[test]
    fn plain_http_is_loopback_only() {
        require_issuer_origin(
            "http://localhost:8080",
            "http://localhost:8080/auth/device/code",
            "device authorization endpoint",
        )
        .expect("loopback HTTP is the self-hosted default");
        require_issuer_origin(
            "http://router.example.com",
            "http://router.example.com/auth/device/code",
            "device authorization endpoint",
        )
        .expect_err("non-loopback HTTP issuer is rejected");
    }
}
