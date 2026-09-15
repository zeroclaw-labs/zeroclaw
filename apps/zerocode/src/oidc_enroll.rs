//! Device-grant enrollment against the daemon gateway's OIDC API.
//!
//! zerocode holds no IdP client credentials and, by gate, no zeroclaw
//! crate dependencies, so it enrolls through the gateway's cross-surface
//! enrollment API (`/api/oidc/{alias}/device/*`), which proxies the
//! RFC 8628 device grant with the `[oidc.<alias>]` entry's credentials.
//! The resulting access token is held in memory for this session only
//! and presented as `auth_token` in the RPC handshake; nothing is
//! stored.
//!
//! A device code travels out on this leg and a bearer token comes back, so
//! it is held to the same transport rules as the WSS leg it precedes:
//!
//! * the configured TLS material is reused verbatim — an installation whose
//!   gateway presents a private CA certificate, or expects a mutual-TLS
//!   client identity, enrolls without anyone having to turn verification
//!   off for this one request;
//! * the URL must be `https`, with plain `http` accepted only for a gateway
//!   on loopback (a developer running the daemon on the same host);
//! * redirects are never followed, because a redirect can relocate or
//!   downgrade the exchange that carries the code and the token.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// Longest device-code lifetime this client will wait out. RFC 8628 sets no
/// ceiling on `expires_in`, so an IdP or gateway advertising hours would
/// otherwise park the enrollment loop for that long; an hour is far above
/// any real device code and still refuses the pathological values that make
/// `Instant + Duration` meaningless.
const MAX_DEVICE_CODE_LIFETIME_SECS: u64 = 3600;

/// Longest advertised poll interval this client will honor, for the same
/// reason: RFC 8628 puts no ceiling on `interval`, and one measured in hours
/// turns the flow into an indefinite sleep.
const MAX_POLL_INTERVAL_SECS: u64 = 300;

/// Per-request timeout for one enrollment call.
const ENROLL_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Deserialize)]
pub(crate) struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_poll_interval")]
    pub interval: u64,
}

/// `device_code` is the bearer-equivalent secret of the pending grant: anyone
/// holding it can redeem the approval, so it never reaches a log line or a
/// panic message. The rest is the prompt the user is already reading off the
/// screen and stays visible so a failed enrollment is diagnosable.
impl std::fmt::Debug for DeviceStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceStart")
            .field("device_code", &"<redacted>")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("verification_uri_complete", &self.verification_uri_complete)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct PollResponse {
    status: String,
    #[serde(default)]
    token: Option<PollToken>,
}

#[derive(Deserialize)]
struct PollToken {
    access_token: String,
}

#[derive(Deserialize)]
struct GatewayError {
    error: String,
}

/// Validate and normalize the gateway's enrollment origin.
///
/// Returns the origin with any trailing slash stripped, keeping a path prefix
/// so a gateway served under one (`https://host/zeroclaw`) still works.
/// Rejects anything that would put the device code or the returned bearer
/// token on the wire in the clear, or in a place they can be logged: a
/// non-`https` scheme (except to a loopback gateway), embedded credentials,
/// a query string, or a fragment.
fn validate_enroll_url(base: &str) -> Result<String> {
    let url = url::Url::parse(base)
        .with_context(|| format!("the enrollment URL is not a valid URL: {base}"))?;
    let Some(host) = url.host() else {
        bail!("the enrollment URL must include a host: {base}");
    };
    let loopback = match &host {
        url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(addr) => addr.is_loopback(),
        url::Host::Ipv6(addr) => addr.is_loopback(),
    };
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        bail!(
            "the enrollment URL must use https, because a device code and the access token \
             travel on it; plain http is accepted only for a loopback gateway (127.0.0.0/8, \
             ::1, or localhost): {base}"
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("the enrollment URL must not embed credentials: {base}");
    }
    if url.query().is_some() {
        bail!("the enrollment URL must not carry a query string: {base}");
    }
    if url.fragment().is_some() {
        bail!("the enrollment URL must not carry a fragment: {base}");
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub(crate) struct GatewayEnrollment {
    base: String,
    http: reqwest::Client,
}

impl GatewayEnrollment {
    /// `base` is the gateway's HTTP origin (`[connection.wss] enroll_url`),
    /// held to [`validate_enroll_url`]'s policy.
    ///
    /// `tls` is the very same [`crate::client::ClientTls`] the WSS connection
    /// uses, so the enrollment origin gets exactly that trust: the configured
    /// private CA, `skip_verify`, and the mutual-TLS client certificate. A
    /// default `ClientTls` keeps reqwest's public webpki roots. Redirects are
    /// not followed: the bearer-carrying exchange stays on the origin the
    /// operator configured.
    pub fn new(base: &str, tls: &crate::client::ClientTls) -> Result<Self> {
        let base = validate_enroll_url(base)?;
        let mut builder = reqwest::Client::builder()
            .timeout(ENROLL_HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if !tls.is_default() {
            let config = crate::client::RpcClient::wss_tls_config(tls)?;
            builder = builder.use_preconfigured_tls((*config).clone());
        }
        Ok(Self {
            base,
            http: builder.build()?,
        })
    }

    async fn gateway_error(response: reqwest::Response) -> anyhow::Error {
        let status = response.status();
        match response.json::<GatewayError>().await {
            Ok(err) => anyhow::Error::msg(err.error),
            Err(_) => anyhow::Error::msg(format!("gateway returned HTTP {status}")),
        }
    }

    pub async fn device_start(&self, alias: &str) -> Result<DeviceStart> {
        let response = self
            .http
            .post(format!("{}/api/oidc/{alias}/device/start", self.base))
            .send()
            .await
            .context("cannot reach the gateway enrollment API")?;
        if !response.status().is_success() {
            return Err(Self::gateway_error(response).await);
        }
        response
            .json()
            .await
            .context("device start response is not valid JSON")
    }

    /// One poll, keeping RFC 8628 `slow_down` distinct from
    /// `authorization_pending`: the former obliges the caller to add five
    /// seconds to its interval, the latter to keep the current one.
    ///
    /// The gateway enforces a per-client poll budget and answers HTTP 429 with
    /// a `Retry-After` once it is exceeded. That is the transport spelling of
    /// the same obligation, so it maps to `slow_down` and a legitimate client
    /// backs off instead of failing the enrollment outright, waiting out the
    /// `Retry-After` the gateway named rather than the RFC's five seconds: a
    /// lockout can hold the client off for minutes, and polling through it
    /// only keeps the lockout alive.
    pub async fn device_poll(&self, alias: &str, device_code: &str) -> Result<DevicePoll> {
        let response = self
            .http
            .post(format!("{}/api/oidc/{alias}/device/poll", self.base))
            .json(&serde_json::json!({ "device_code": device_code }))
            .send()
            .await
            .context("cannot reach the gateway enrollment API")?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Ok(DevicePoll::SlowDown {
                retry_after: retry_after_secs(response.headers()),
            });
        }
        if !response.status().is_success() {
            return Err(Self::gateway_error(response).await);
        }
        let poll: PollResponse = response
            .json()
            .await
            .context("device poll response is not valid JSON")?;
        match poll.status.as_str() {
            "granted" => match poll.token {
                Some(token) => Ok(DevicePoll::Granted(token.access_token)),
                None => bail!("gateway reported a grant without a token"),
            },
            "pending" => Ok(DevicePoll::Pending),
            // An IdP-originated `slow_down` names no delay of its own, so the
            // RFC's five-second increment is all there is to go on.
            "slow_down" => Ok(DevicePoll::SlowDown { retry_after: None }),
            other => bail!("unexpected poll status from the gateway: {other}"),
        }
    }
}

/// The `Retry-After` delay in whole seconds, if the header names one.
///
/// RFC 9110 also allows an HTTP-date, which the gateway never sends; a date,
/// a malformed value, or a missing header all yield `None` and leave the
/// caller on the RFC 8628 back-off it would have used anyway.
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// One device-grant poll outcome. RFC 8628 §3.5 treats `slow_down` and
/// `authorization_pending` differently, so the two must stay distinct.
#[derive(PartialEq, Eq)]
pub(crate) enum DevicePoll {
    Granted(String),
    Pending,
    /// `retry_after` is the gateway's `Retry-After` in seconds when the
    /// throttling came from the gateway's own budget or lockout, and `None`
    /// when the IdP asked for the slow-down and named no delay.
    SlowDown {
        retry_after: Option<u64>,
    },
}

/// The granted variant carries the access token itself, so formatting it would
/// put a live credential into whatever consumed the `{:?}`. The variant name
/// is kept because it is what makes a trace readable.
impl std::fmt::Debug for DevicePoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Granted(_) => f.write_str("Granted(<redacted>)"),
            Self::Pending => f.write_str("Pending"),
            // The back-off delay is a timing hint, not a secret, and it is
            // what makes a throttled trace readable.
            Self::SlowDown { retry_after } => write!(f, "SlowDown({retry_after:?})"),
        }
    }
}

/// RFC 8628 §3.5 interval update. The client must never poll faster than
/// the advertised `minimum`; `slow_down` obliges it to add five seconds,
/// while `pending` keeps the current wait. The result is never capped
/// below the floor, so an IdP advertising 60s is honored and a throttled
/// client backs off instead of polling early until the code expires.
///
/// A gateway `Retry-After` is the longer obligation of the two whenever it
/// exceeds the RFC increment: it names when the budget or the lockout
/// actually frees a slot, so anything sooner is a poll that can only be
/// refused. The wait is still never shorter than the RFC increment, so a
/// small or zero `Retry-After` cannot talk this client into polling faster.
fn next_poll_interval(current: u64, minimum: u64, outcome: &DevicePoll) -> u64 {
    let next = match outcome {
        DevicePoll::SlowDown { retry_after } => current
            .saturating_add(5)
            .max(retry_after.unwrap_or_default()),
        DevicePoll::Pending | DevicePoll::Granted(_) => current,
    };
    next.max(minimum)
}

/// Bound the timings a gateway can put this client on.
///
/// RFC 8628 lets a server advertise any `expires_in` and `interval`, and the
/// client is otherwise obliged to follow both; without a ceiling a remote
/// value can leave zerocode sleeping between polls, or waiting for approval,
/// for as long as the remote side likes.
fn validate_device_start(start: &DeviceStart) -> Result<()> {
    if start.expires_in == 0 {
        bail!("the gateway advertised a device code that is already expired (expires_in = 0)");
    }
    if start.expires_in > MAX_DEVICE_CODE_LIFETIME_SECS {
        bail!(
            "the gateway advertised a device code lifetime of {}s, above the \
             {MAX_DEVICE_CODE_LIFETIME_SECS}s this client will wait for approval",
            start.expires_in
        );
    }
    if start.interval > MAX_POLL_INTERVAL_SECS {
        bail!(
            "the gateway advertised a poll interval of {}s, above the \
             {MAX_POLL_INTERVAL_SECS}s this client will wait between polls",
            start.interval
        );
    }
    Ok(())
}

/// The polling driver, with no HTTP in it: `poll` performs one exchange and
/// everything else here is timing, so the deadline behavior is testable on a
/// paused clock without a server.
///
/// Every wait is clipped to what is left of the device code's lifetime, and
/// the deadline is re-checked immediately before each call to `poll`, so no
/// request ever goes out carrying a code that is already dead — a gateway
/// advertising `expires_in = 1, interval = 60` gets zero polls, not one a
/// minute late.
async fn poll_until_granted(
    start: &DeviceStart,
    mut poll: impl AsyncFnMut(&str) -> Result<DevicePoll>,
) -> Result<String> {
    validate_device_start(start)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(start.expires_in);
    // RFC 8628's default interval is the floor, not one second: an advertised
    // `0` or `1` would otherwise put this client on a once-a-second poll, well
    // past the gateway's per-client budget and straight into its lockout.
    let minimum = start.interval.max(default_poll_interval());
    let mut interval = minimum;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("the device code expired before approval");
        }
        tokio::time::sleep(Duration::from_secs(interval).min(remaining)).await;
        // A wait clipped to the remaining lifetime lands exactly on the
        // deadline, so re-check here rather than at the top of the loop: the
        // code is dead by now and the request must not be sent.
        if tokio::time::Instant::now() >= deadline {
            bail!("the device code expired before approval");
        }
        let outcome = poll(&start.device_code).await?;
        if let DevicePoll::Granted(token) = outcome {
            return Ok(token);
        }
        interval = next_poll_interval(interval, minimum, &outcome);
    }
}

/// Run the whole device flow: start, hand the prompt data to `on_prompt`
/// (rendered by the caller so wording stays with the i18n layer), poll
/// until granted, denied, or the code expires.
///
/// `tls` is the WSS connection's own TLS material, reused for the enrollment
/// origin so a private-CA or mutual-TLS installation needs no second setting.
pub(crate) async fn run_device_flow(
    base: &str,
    tls: &crate::client::ClientTls,
    alias: &str,
    on_prompt: impl Fn(&DeviceStart),
) -> Result<String> {
    let gateway = GatewayEnrollment::new(base, tls)?;
    let start = gateway.device_start(alias).await?;
    // Bounds first: an unusable lifetime or interval must not reach the user
    // as a prompt they would wait on for nothing.
    validate_device_start(&start)?;
    on_prompt(&start);
    poll_until_granted(&start, async |code: &str| {
        gateway.device_poll(alias, code).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ClientTls;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn device_start_fixture(expires_in: u64, interval: u64) -> DeviceStart {
        DeviceStart {
            device_code: "dev-1".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://sso.example.com/activate".into(),
            verification_uri_complete: None,
            expires_in,
            interval,
        }
    }

    #[tokio::test]
    async fn device_start_parses_the_gateway_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "dev-1",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://sso.example.com/activate",
                "expires_in": 600,
                "interval": 5,
            })))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), &ClientTls::default()).unwrap();
        let start = gateway.device_start("corp").await.unwrap();
        assert_eq!(start.user_code, "ABCD-EFGH");
        assert_eq!(start.device_code, "dev-1");
    }

    #[tokio::test]
    async fn device_poll_maps_pending_and_granted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("waiting"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "status": "pending" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("done"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "granted",
                "provider": "oidc.corp",
                "token": { "access_token": "at-tui", "expires_in": 3600 },
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("throttled"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "status": "slow_down" })),
            )
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), &ClientTls::default()).unwrap();
        assert_eq!(
            gateway.device_poll("corp", "waiting").await.unwrap(),
            DevicePoll::Pending
        );
        // slow_down must survive as its own outcome: folding it into
        // pending would lose the RFC 8628 §3.5 back-off obligation. It comes
        // from the IdP and names no delay, so only the RFC increment applies.
        assert_eq!(
            gateway.device_poll("corp", "throttled").await.unwrap(),
            DevicePoll::SlowDown { retry_after: None }
        );
        assert_eq!(
            gateway.device_poll("corp", "done").await.unwrap(),
            DevicePoll::Granted("at-tui".into())
        );
    }

    /// The gateway's per-client poll budget answers 429 rather than a JSON
    /// `slow_down`, and a client that failed on it would abandon an
    /// enrollment the user is about to approve. The `Retry-After` it sends
    /// names when a slot frees, so it has to survive the mapping.
    #[tokio::test]
    async fn http_429_is_treated_as_slow_down() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("budget"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "10")
                    .set_body_json(serde_json::json!({
                        "error": "poll budget exceeded",
                        "retry_after": 10,
                    })),
            )
            .mount(&server)
            .await;
        // A `Retry-After` that is not a plain count of seconds (RFC 9110 also
        // allows an HTTP-date) leaves the client on the RFC 8628 back-off
        // rather than on a delay it guessed at.
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .and(body_string_contains("dated"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "Wed, 21 Oct 2015 07:28:00 GMT")
                    .set_body_json(serde_json::json!({ "error": "poll budget exceeded" })),
            )
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), &ClientTls::default()).unwrap();
        assert_eq!(
            gateway.device_poll("corp", "budget").await.unwrap(),
            DevicePoll::SlowDown {
                retry_after: Some(10)
            }
        );
        assert_eq!(
            gateway.device_poll("corp", "dated").await.unwrap(),
            DevicePoll::SlowDown { retry_after: None }
        );
    }

    #[test]
    fn poll_interval_follows_rfc_8628() {
        let slow_down = DevicePoll::SlowDown { retry_after: None };
        // slow_down adds five seconds (5 -> 10), never one.
        assert_eq!(next_poll_interval(5, 5, &slow_down), 10);
        // Repeated throttling keeps backing off and is not capped at 30.
        assert_eq!(next_poll_interval(30, 5, &slow_down), 35);
        // pending keeps the current wait rather than growing it.
        assert_eq!(next_poll_interval(10, 5, &DevicePoll::Pending), 10);
        // An advertised interval above 30 is honored, never shrunk.
        assert_eq!(next_poll_interval(60, 60, &DevicePoll::Pending), 60);
        // The advertised minimum is a floor the interval never drops below.
        assert_eq!(next_poll_interval(1, 5, &DevicePoll::Pending), 5);
    }

    /// A gateway `Retry-After` outranks the RFC increment when it is longer,
    /// and can never shorten the wait below it.
    #[test]
    fn retry_after_outranks_the_rfc_increment_when_longer() {
        let retry_after = |secs: u64| DevicePoll::SlowDown {
            retry_after: Some(secs),
        };
        // A lockout's 300s is honored in full, not shaved to 10s.
        assert_eq!(next_poll_interval(5, 5, &retry_after(300)), 300);
        // The longer of the two obligations wins: 30 + 5 beats a 12s hint.
        assert_eq!(next_poll_interval(30, 5, &retry_after(12)), 35);
        // A zero or tiny `Retry-After` cannot make this client poll sooner.
        assert_eq!(next_poll_interval(5, 5, &retry_after(0)), 10);
        assert_eq!(next_poll_interval(5, 5, &retry_after(1)), 10);
        // The advertised floor still applies to the result.
        assert_eq!(next_poll_interval(5, 60, &retry_after(20)), 60);
    }

    #[tokio::test]
    async fn gateway_denials_surface_the_error_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/poll"))
            .respond_with(ResponseTemplate::new(502).set_body_json(serde_json::json!({
                "error": "device grant failed: access_denied (user rejected the request)",
            })))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), &ClientTls::default()).unwrap();
        let err = gateway.device_poll("corp", "dev-1").await.unwrap_err();
        assert!(err.to_string().contains("access_denied"), "{err}");
    }

    #[tokio::test]
    async fn unknown_alias_is_surfaced() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/nope/device/start"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": "unknown oidc provider alias",
            })))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), &ClientTls::default()).unwrap();
        let err = gateway.device_start("nope").await.unwrap_err();
        assert!(
            err.to_string().contains("unknown oidc provider alias"),
            "{err}"
        );
    }

    // ── URL policy ───────────────────────────────────────────────

    #[test]
    fn enroll_url_requires_https_away_from_loopback() {
        // https anywhere, with the port and any path prefix preserved and a
        // trailing slash stripped so the joined paths stay well formed.
        assert_eq!(
            validate_enroll_url("https://gw.example.com:9090").unwrap(),
            "https://gw.example.com:9090"
        );
        assert_eq!(
            validate_enroll_url("https://gw.example.com/prefix/").unwrap(),
            "https://gw.example.com/prefix"
        );
        // Plain http only to a gateway on this host.
        assert_eq!(
            validate_enroll_url("http://127.0.0.1:9090").unwrap(),
            "http://127.0.0.1:9090"
        );
        assert_eq!(
            validate_enroll_url("http://localhost:9090").unwrap(),
            "http://localhost:9090"
        );
        assert_eq!(
            validate_enroll_url("http://[::1]:9090").unwrap(),
            "http://[::1]:9090"
        );
        // A remote http origin would put the device code and the bearer token
        // on the wire in the clear.
        for remote in ["http://203.0.113.5:9090", "http://gw.example.com"] {
            let err = validate_enroll_url(remote).unwrap_err().to_string();
            assert!(err.contains("https"), "{remote}: {err}");
        }
        assert!(validate_enroll_url("ftp://gw.example.com").is_err());
        assert!(validate_enroll_url("https://gw.example.com/?x=1").is_err());
        assert!(validate_enroll_url("https://gw.example.com/#frag").is_err());
        assert!(validate_enroll_url("https://").is_err());
    }

    // ── Deadline handling ────────────────────────────────────────

    #[test]
    fn device_start_bounds_are_enforced() {
        let err = validate_device_start(&device_start_fixture(0, 5))
            .unwrap_err()
            .to_string();
        assert!(err.contains("expired"), "{err}");
        let err = validate_device_start(&device_start_fixture(86_400, 5))
            .unwrap_err()
            .to_string();
        assert!(err.contains("lifetime"), "{err}");
        let err = validate_device_start(&device_start_fixture(600, 3_600))
            .unwrap_err()
            .to_string();
        assert!(err.contains("interval"), "{err}");
        assert!(validate_device_start(&device_start_fixture(600, 5)).is_ok());
    }

    /// `expires_in = 1, interval = 60`: the code dies a minute before the
    /// first wait would end, so the driver must fail at the deadline and send
    /// nothing at all.
    #[tokio::test(start_paused = true)]
    async fn an_expired_code_produces_no_late_poll() {
        let start = device_start_fixture(1, 60);
        let calls = std::cell::Cell::new(0u32);
        let err = poll_until_granted(&start, async |_code: &str| {
            calls.set(calls.get() + 1);
            Ok(DevicePoll::Pending)
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("expired before approval"), "{err}");
        assert_eq!(calls.get(), 0, "a poll went out after the code expired");
    }

    /// `expires_in = 7, interval = 5`: one poll at 5s fits, the second wait is
    /// clipped to the 2s left and the deadline then refuses the request.
    #[tokio::test(start_paused = true)]
    async fn waits_are_clipped_to_the_remaining_lifetime() {
        let start = device_start_fixture(7, 5);
        let calls = std::cell::Cell::new(0u32);
        let began = tokio::time::Instant::now();
        let err = poll_until_granted(&start, async |_code: &str| {
            calls.set(calls.get() + 1);
            Ok(DevicePoll::Pending)
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("expired before approval"), "{err}");
        assert_eq!(calls.get(), 1, "expected exactly one in-lifetime poll");
        assert_eq!(began.elapsed(), Duration::from_secs(7));
    }

    /// A `slow_down` must actually lengthen the next wait: 5s to the first
    /// poll, then 10s to the second.
    #[tokio::test(start_paused = true)]
    async fn slow_down_lengthens_the_next_wait() {
        let start = device_start_fixture(600, 5);
        let calls = std::cell::Cell::new(0u32);
        let began = tokio::time::Instant::now();
        let token = poll_until_granted(&start, async |_code: &str| {
            let n = calls.get();
            calls.set(n + 1);
            Ok(if n == 0 {
                DevicePoll::SlowDown { retry_after: None }
            } else {
                DevicePoll::Granted("tok".into())
            })
        })
        .await
        .unwrap();
        assert_eq!(token, "tok");
        assert_eq!(calls.get(), 2);
        assert_eq!(began.elapsed(), Duration::from_secs(15));
    }

    /// A 429 carrying `Retry-After: 300` (the gateway's lockout ceiling) has
    /// to hold the next poll for the full five minutes: polling sooner is a
    /// request the gateway can only refuse, and each refusal keeps the
    /// lockout alive.
    #[tokio::test(start_paused = true)]
    async fn retry_after_sets_the_next_wait() {
        let start = device_start_fixture(600, 5);
        let calls = std::cell::Cell::new(0u32);
        let began = tokio::time::Instant::now();
        let token = poll_until_granted(&start, async |_code: &str| {
            let n = calls.get();
            calls.set(n + 1);
            Ok(if n == 0 {
                DevicePoll::SlowDown {
                    retry_after: Some(300),
                }
            } else {
                DevicePoll::Granted("tok".into())
            })
        })
        .await
        .unwrap();
        assert_eq!(token, "tok");
        assert_eq!(calls.get(), 2);
        // 5s to the throttled poll, then the gateway's own 300s.
        assert_eq!(began.elapsed(), Duration::from_secs(305));
    }

    /// The `Retry-After` wait is still clipped to the device code's remaining
    /// lifetime: a lockout longer than the code outlives it, and the driver
    /// fails at the deadline rather than sleeping past it.
    #[tokio::test(start_paused = true)]
    async fn retry_after_is_clipped_to_the_remaining_lifetime() {
        let start = device_start_fixture(100, 5);
        let calls = std::cell::Cell::new(0u32);
        let began = tokio::time::Instant::now();
        let err = poll_until_granted(&start, async |_code: &str| {
            calls.set(calls.get() + 1);
            Ok(DevicePoll::SlowDown {
                retry_after: Some(300),
            })
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("expired before approval"), "{err}");
        assert_eq!(calls.get(), 1, "expected exactly one in-lifetime poll");
        assert_eq!(began.elapsed(), Duration::from_secs(100));
    }

    /// RFC 8628's default interval is the floor: an advertised `0` or `1`
    /// must not put this client on a once-a-second poll, which the gateway's
    /// 20-per-minute budget would refuse outright.
    #[tokio::test(start_paused = true)]
    async fn an_advertised_interval_below_the_default_is_floored() {
        for advertised in [0, 1] {
            let start = device_start_fixture(600, advertised);
            let calls = std::cell::Cell::new(0u32);
            let began = tokio::time::Instant::now();
            let token = poll_until_granted(&start, async |_code: &str| {
                let n = calls.get();
                calls.set(n + 1);
                Ok(if n == 0 {
                    DevicePoll::Pending
                } else {
                    DevicePoll::Granted("tok".into())
                })
            })
            .await
            .unwrap();
            assert_eq!(token, "tok");
            assert_eq!(calls.get(), 2);
            assert_eq!(
                began.elapsed(),
                Duration::from_secs(10),
                "interval {advertised} should be floored at the RFC 8628 default"
            );
        }
    }

    // ── Secret redaction ─────────────────────────────────────────

    #[test]
    fn secret_bearing_types_redact_their_secrets() {
        let granted = format!("{:?}", DevicePoll::Granted("sentinel-token".into()));
        assert!(!granted.contains("sentinel-token"), "{granted}");
        assert!(granted.contains("Granted"), "{granted}");

        // The back-off delay is a timing hint rather than a secret, so it
        // stays legible: a throttled enrollment is diagnosed from it.
        let throttled = format!(
            "{:?}",
            DevicePoll::SlowDown {
                retry_after: Some(300)
            }
        );
        assert!(throttled.contains("SlowDown"), "{throttled}");
        assert!(throttled.contains("300"), "{throttled}");
        assert!(
            format!("{:?}", DevicePoll::SlowDown { retry_after: None }).contains("SlowDown"),
            "the variant name is what makes a trace readable"
        );

        let start = DeviceStart {
            device_code: "sentinel-device".into(),
            ..device_start_fixture(600, 5)
        };
        let rendered = format!("{start:?}");
        assert!(!rendered.contains("sentinel-device"), "{rendered}");
        // The user code is the prompt the user is reading; it stays legible.
        assert!(rendered.contains("ABCD-EFGH"), "{rendered}");
    }

    // ── Transport policy ─────────────────────────────────────────

    /// A one-shot HTTPS responder: it completes the TLS handshake, reads the
    /// request head, and answers `body` as JSON. The join handle resolves to
    /// the number of responses it actually wrote, so a test can assert that a
    /// refused handshake produced no exchange at all.
    async fn https_stub(
        acceptor: tokio_rustls::TlsAcceptor,
        body: String,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<usize>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub listener");
        let addr = listener.local_addr().expect("stub local addr");
        let handle = tokio::spawn(async move {
            let Ok((tcp, _)) = listener.accept().await else {
                return 0;
            };
            let Ok(mut tls) = acceptor.accept(tcp).await else {
                return 0;
            };
            let mut buf = vec![0u8; 8192];
            let mut filled = 0usize;
            while filled < buf.len() {
                let Ok(n) = tls.read(&mut buf[filled..]).await else {
                    return 0;
                };
                if n == 0 {
                    return 0;
                }
                filled += n;
                if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            if tls.write_all(response.as_bytes()).await.is_err() {
                return 0;
            }
            let _ = tls.shutdown().await;
            1
        });
        (addr, handle)
    }

    fn device_start_body() -> String {
        serde_json::json!({
            "device_code": "dev-tls",
            "user_code": "TLS-CODE",
            "verification_uri": "https://sso.example.com/activate",
            "expires_in": 600,
            "interval": 5,
        })
        .to_string()
    }

    /// An mTLS acceptor: the server demands a client certificate issued by
    /// `client_ca_pem`, the way a mutual-TLS gateway does.
    fn mtls_acceptor(
        cert_pem: &str,
        key_pem: &str,
        client_ca_pem: &str,
    ) -> tokio_rustls::TlsAcceptor {
        use std::sync::Arc;

        let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("parse server certs");
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
            .expect("parse server key")
            .expect("server private key present");
        let mut roots = rustls::RootCertStore::empty();
        for ca in rustls_pemfile::certs(&mut client_ca_pem.as_bytes()) {
            roots
                .add(ca.expect("parse client CA"))
                .expect("add client CA");
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .expect("client certificate verifier");
        let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("safe protocols")
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .expect("server cert/key");
        tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
    }

    /// A client certificate with the ClientAuth EKU, signed by the test CA.
    fn gen_client_cert(ca: &rcgen::Certificate, ca_key: &rcgen::KeyPair) -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("generate client key");
        let mut params =
            rcgen::CertificateParams::new(Vec::<String>::new()).expect("client certificate params");
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, "zerocode-enrollment-client");
        params.distinguished_name = dn;
        params.is_ca = rcgen::IsCa::NoCa;
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let cert = params
            .signed_by(&key, ca, ca_key)
            .expect("sign client cert");
        (cert.pem(), key.serialize_pem())
    }

    fn write_pem(dir: &tempfile::TempDir, name: &str, pem: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, pem).expect("write PEM");
        path.to_str().expect("utf-8 temp path").to_string()
    }

    /// The gateway's own CA must be enough: an installation on a private CA
    /// enrolls without anyone disabling verification for this leg.
    #[tokio::test]
    async fn a_private_ca_is_trusted_for_enrollment() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (ca_pem, ca, ca_key) = crate::client_crypto::test_pki::gen_ca();
        let (cert, key) = crate::client_crypto::test_pki::gen_server_cert(
            &ca,
            &ca_key,
            &["localhost".into(), "127.0.0.1".into()],
        );
        let acceptor = crate::client_crypto::test_pki::tls_acceptor(&cert, &key);
        let (addr, server) = https_stub(acceptor, device_start_body()).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let tls = ClientTls {
            ca_cert_path: Some(write_pem(&dir, "ca.pem", &ca_pem)),
            ..Default::default()
        };
        let gateway = GatewayEnrollment::new(&format!("https://{addr}"), &tls).unwrap();
        let start = gateway.device_start("corp").await.unwrap();
        assert_eq!(start.user_code, "TLS-CODE");
        assert_eq!(server.await.expect("stub joined"), 1);
    }

    /// A certificate from some other CA is not the gateway: the exchange must
    /// fail rather than hand a device code to whoever answered.
    #[tokio::test]
    async fn an_untrusted_certificate_fails_enrollment() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (_, server_ca, server_ca_key) = crate::client_crypto::test_pki::gen_ca();
        let (other_ca_pem, _, _) = crate::client_crypto::test_pki::gen_ca();
        let (cert, key) = crate::client_crypto::test_pki::gen_server_cert(
            &server_ca,
            &server_ca_key,
            &["localhost".into(), "127.0.0.1".into()],
        );
        let acceptor = crate::client_crypto::test_pki::tls_acceptor(&cert, &key);
        let (addr, server) = https_stub(acceptor, device_start_body()).await;

        let dir = tempfile::tempdir().expect("temp dir");
        let tls = ClientTls {
            ca_cert_path: Some(write_pem(&dir, "other-ca.pem", &other_ca_pem)),
            ..Default::default()
        };
        let gateway = GatewayEnrollment::new(&format!("https://{addr}"), &tls).unwrap();
        gateway
            .device_start("corp")
            .await
            .expect_err("a certificate from an unrelated CA must be refused");
        assert_eq!(
            server.await.expect("stub joined"),
            0,
            "the stub answered a handshake the client should have refused"
        );
    }

    /// A gateway behind mutual TLS gets the configured client identity, and a
    /// client without one is turned away.
    #[tokio::test]
    async fn mutual_tls_presents_the_configured_client_identity() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (ca_pem, ca, ca_key) = crate::client_crypto::test_pki::gen_ca();
        let (cert, key) = crate::client_crypto::test_pki::gen_server_cert(
            &ca,
            &ca_key,
            &["localhost".into(), "127.0.0.1".into()],
        );
        let (client_cert, client_key) = gen_client_cert(&ca, &ca_key);

        let dir = tempfile::tempdir().expect("temp dir");
        let ca_path = write_pem(&dir, "ca.pem", &ca_pem);
        let cert_path = write_pem(&dir, "client.crt", &client_cert);
        let key_path = write_pem(&dir, "client.key", &client_key);

        let (addr, server) =
            https_stub(mtls_acceptor(&cert, &key, &ca_pem), device_start_body()).await;
        let with_identity = ClientTls {
            ca_cert_path: Some(ca_path.clone()),
            client_cert_path: Some(cert_path),
            client_key_path: Some(key_path),
            ..Default::default()
        };
        let gateway = GatewayEnrollment::new(&format!("https://{addr}"), &with_identity).unwrap();
        let start = gateway.device_start("corp").await.unwrap();
        assert_eq!(start.user_code, "TLS-CODE");
        assert_eq!(server.await.expect("stub joined"), 1);

        let (addr, server) =
            https_stub(mtls_acceptor(&cert, &key, &ca_pem), device_start_body()).await;
        let without_identity = ClientTls {
            ca_cert_path: Some(ca_path),
            ..Default::default()
        };
        let gateway =
            GatewayEnrollment::new(&format!("https://{addr}"), &without_identity).unwrap();
        gateway
            .device_start("corp")
            .await
            .expect_err("a gateway requiring mutual TLS must refuse an anonymous client");
        assert_eq!(server.await.expect("stub joined"), 0);
    }

    /// The downgrade refusal happens in the constructor, before any socket is
    /// opened, so a misconfigured URL never puts a device code on the wire.
    #[test]
    fn a_remote_http_gateway_is_refused_before_any_request() {
        let Err(err) = GatewayEnrollment::new("http://203.0.113.5:9090", &ClientTls::default())
        else {
            panic!("a remote http enrollment URL must be refused");
        };
        let err = err.to_string();
        assert!(err.contains("https"), "{err}");
    }

    /// A redirect could relocate or downgrade the bearer-carrying exchange, so
    /// it is surfaced as an error and the client never calls the target.
    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/start"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", "/api/oidc/corp/device/start-elsewhere"),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/oidc/corp/device/start-elsewhere"))
            .respond_with(ResponseTemplate::new(200).set_body_string(device_start_body()))
            .mount(&server)
            .await;
        let gateway = GatewayEnrollment::new(&server.uri(), &ClientTls::default()).unwrap();
        gateway
            .device_start("corp")
            .await
            .expect_err("a redirected enrollment must fail rather than follow");
        let seen = server
            .received_requests()
            .await
            .expect("request recording is on");
        assert_eq!(seen.len(), 1, "the redirect target was called");
        assert_eq!(seen[0].url.path(), "/api/oidc/corp/device/start");
    }
}
