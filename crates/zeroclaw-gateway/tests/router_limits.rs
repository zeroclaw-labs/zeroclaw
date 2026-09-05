//! Router-level proofs for two gateway boundaries that previously had only
//! constant-level coverage: the 64 KB request-body limit and the long-running
//! timeout exception carried by `POST /api/cron/{id}/run`.
//!
//! `MAX_BODY_SIZE` and `gateway_long_running_request_timeout_secs()` are
//! already asserted as values elsewhere, but a constant proves nothing about
//! the assembled router. These tests boot the real gateway through
//! `run_gateway` — the same front-door pattern the `/acp` test in
//! `src/acp.rs` uses — and speak raw HTTP/1.1 to it, so every assertion here
//! crosses `RequestBodyLimitLayer` and `TimeoutLayer` exactly as a network
//! client would. Two separate `RequestBodyLimitLayer::new(MAX_BODY_SIZE)`
//! calls and two separate `TimeoutLayer`s exist in `run_gateway` (one pair on
//! the main router, one pair on the long-running sub-router); each is covered
//! here, because dropping either one is a silent DoS-shaped regression.
//!
//! Raw sockets rather than an HTTP client crate: proving a timeout requires
//! sending a request head, then *stalling* mid-body, which a normal client
//! API does not expose.
//!
//! Every timeout assertion pins the *configured* budget rather than merely
//! "longer than the default". Each one either straddles a single workload
//! with two different configured values, or bounds the observed latency on
//! both sides of the configured value, so a regression that hard-codes some
//! other constant fails at least one test rather than sliding through.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroclaw_config::schema::Config;
use zeroclaw_gateway::MAX_BODY_SIZE;

const AGENT: &str = "test-agent";

/// HTTP status codes asserted below, named so failures read clearly.
const PAYLOAD_TOO_LARGE: u16 = 413;
const REQUEST_TIMEOUT: u16 = 408;
const BAD_REQUEST: u16 = 400;
const NOT_FOUND: u16 = 404;
#[cfg(unix)]
const OK: u16 = 200;

// ── Harness ────────────────────────────────────────────────────────────────

/// Aborts the gateway task when the test scope ends, including on a panicking
/// assertion, so a failing test cannot leave a listener bound.
struct GatewayGuard {
    task: tokio::task::JoinHandle<Result<(), String>>,
    addr: SocketAddr,
    _tmp: tempfile::TempDir,
}

impl Drop for GatewayGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Minimal on-disk-isolated config: pairing off so `require_auth` admits an
/// unauthenticated request and the assertions are about the layers, not auth.
fn base_config(root: &std::path::Path) -> Config {
    let mut cfg = Config {
        data_dir: root.join("data"),
        config_path: root.join("config.toml"),
        ..Config::default()
    };
    cfg.gateway.require_pairing = false;
    std::fs::create_dir_all(&cfg.data_dir).expect("create data dir");
    cfg
}

/// Adds one agent whose risk profile allows the `sleep` shell command, so a
/// shell cron job can be created through `POST /api/cron` and then triggered
/// through `POST /api/cron/{id}/run` with a known, controllable duration.
#[cfg(unix)]
fn with_shell_cron_agent(mut cfg: Config) -> Config {
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, AnthropicModelProviderConfig, ModelProviderConfig, RiskProfileConfig,
        RuntimeProfileConfig,
    };

    cfg.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("claude-haiku-4-5".to_string()),
                ..ModelProviderConfig::default()
            },
        },
    );
    cfg.risk_profiles.insert(
        "cron-shell".to_string(),
        RiskProfileConfig {
            level: AutonomyLevel::Full,
            allowed_commands: vec!["sleep".to_string()],
            ..RiskProfileConfig::default()
        },
    );
    cfg.runtime_profiles
        .insert("default".to_string(), RuntimeProfileConfig::default());
    cfg.agents.insert(
        AGENT.to_string(),
        AliasedAgentConfig {
            model_provider: "anthropic.default".into(),
            risk_profile: "cron-shell".into(),
            runtime_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );
    cfg
}

/// Boot the real gateway on an OS-assigned port and wait until it reports the
/// address it actually bound.
///
/// Port 0 plus `GatewayReadinessReporter` rather than pre-reserving a port:
/// binding a probe listener, dropping it, and handing the number to the
/// gateway leaves a window in which another process can take the port. The
/// reporter is a plain `pub` constructor over a closure
/// (`zeroclaw_runtime::daemon::GatewayReadinessReporter::new`), and
/// `run_gateway` hands it `listener.local_addr()`, so the test learns the real
/// address with no race and no production change. It also fires after gateway
/// setup completes, which is a stricter readiness signal than "the port
/// accepts TCP".
async fn boot(tmp: tempfile::TempDir, cfg: Config) -> GatewayGuard {
    let (ready_tx, mut ready_rx) = tokio::sync::watch::channel(None);
    let readiness = zeroclaw_runtime::daemon::GatewayReadinessReporter::new(move |addr| {
        let _ = ready_tx.send(Some(addr));
    });

    // The gateway never returns under normal operation; the guard aborts it.
    // The error is stringified inside the task so the handle's type stays
    // nameable here without importing `anyhow`.
    let mut task = zeroclaw_spawn::spawn!(async move {
        zeroclaw_gateway::run_gateway(
            "127.0.0.1",
            0,
            cfg,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(readiness),
        )
        .await
        .map_err(|error| format!("{error:#}"))
    });

    // `map(|_| ())` so no watch `Ref` outlives the borrow of `ready_rx`.
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        ready_rx.wait_for(Option::is_some).await.map(|_| ())
    })
    .await;

    // Two ways to fail, and the gateway task holds the reason for both. The
    // reporter owns the watch sender, so a `run_gateway` that returns early —
    // a bad config, a failed bind — drops it and closes the channel; reporting
    // "channel closed" there would bury the actual error one join away.
    if !matches!(ready, Ok(Ok(()))) {
        let reason = match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
            Ok(Ok(Err(error))) => format!("run_gateway returned an error: {error}"),
            Ok(Ok(Ok(()))) => "run_gateway returned before reporting a bound address".to_string(),
            Ok(Err(error)) => format!("the gateway task panicked or was cancelled: {error}"),
            Err(_) => "it never reported a bound address and is still running".to_string(),
        };
        panic!("gateway did not boot: {reason}");
    }

    let addr = ready_rx.borrow().expect("readiness reported an address");

    GatewayGuard {
        task,
        addr,
        _tmp: tmp,
    }
}

// ── Raw HTTP/1.1 helpers ───────────────────────────────────────────────────

/// Open a connection and write only the request head, declaring
/// `content_length` bytes of body. The caller decides when — and whether — to
/// finish the body, which is what makes the timeout assertions possible.
async fn open_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    content_length: usize,
) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.expect("connect to gateway");
    let head = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .await
        .expect("write request head");
    stream
}

/// Read whatever the server sends until EOF or `budget` expires. Returns the
/// status code and the full raw response text, or `None` when nothing at all
/// arrived inside the budget (which is itself an assertable outcome).
async fn read_response(stream: &mut TcpStream, budget: Duration) -> Option<(u16, String)> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
            Ok(Ok(n)) => raw.extend_from_slice(&chunk[..n]),
        }
    }
    if raw.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, text))
}

/// Send a complete request and read the response.
async fn post(addr: SocketAddr, path: &str, body: &[u8], budget: Duration) -> (u16, String) {
    let mut stream = open_request(addr, "POST", path, body.len()).await;
    // `RequestBodyLimitLayer` rejects on the declared `Content-Length` alone
    // and the server then closes, so a short write is an expected outcome
    // here rather than a test failure.
    let _ = stream.write_all(body).await;
    read_response(&mut stream, budget)
        .await
        .unwrap_or_else(|| panic!("no response for POST {path} within {budget:?}"))
}

/// Stall an ordinary main-router route and assert it is cut off at exactly
/// `configured_secs` — the gateway's `request_timeout_secs`.
///
/// `handle_api_cron_add` takes a `Json<CronAddBody>` extractor, so a request
/// whose body never finishes keeps the response future pending until the layer
/// fires; the 408's arrival time is therefore the budget itself.
///
/// Two-sided on purpose. Requiring only "a 408 eventually" would pass under
/// any hard-coded main-router budget, so the arrival is bounded on both sides
/// of the configured value: the floor rules out a shorter budget, the ceiling
/// rules out a longer constant. The upside margin is deliberately generous —
/// three whole seconds over the budget, against observed timer jitter in the
/// low milliseconds — while still excluding every constant of five seconds or
/// more, including the 30s shipping default.
///
/// Doubles as the in-process control for the long-running assertions: without
/// it, "the cron run survived N seconds" could just mean no timeout is
/// configured anywhere in this gateway.
async fn assert_main_router_times_out_at(addr: SocketAddr, configured_secs: u64) {
    let budget = Duration::from_secs(configured_secs);
    let started = std::time::Instant::now();

    // Declare 512 bytes, send 13, never finish.
    let mut control = open_request(addr, "POST", "/api/cron", 512).await;
    control
        .write_all(br#"{"agent":"a","#)
        .await
        .expect("write partial body");
    let (status, raw) = read_response(&mut control, Duration::from_secs(30))
        .await
        .expect("the main-router TimeoutLayer must answer a stalled request");
    let elapsed = started.elapsed();

    assert_eq!(
        status, REQUEST_TIMEOUT,
        "an ordinary API route must be bounded by request_timeout_secs; \
         response was:\n{raw}"
    );
    let floor = budget.mul_f64(0.8);
    let ceiling = budget + Duration::from_secs(3);
    assert!(
        (floor..=ceiling).contains(&elapsed),
        "the 408 must arrive at the configured {configured_secs}s budget \
         (window {floor:?}..={ceiling:?}), not at some other hard-coded value; \
         it arrived after {elapsed:?}"
    );
}

/// A syntactically valid `CronAddBody` JSON document of exactly `total_len`
/// bytes, padded through the optional `name` field. Naming a deliberately
/// unconfigured agent means the handler's answer is a deterministic 400 whose
/// body quotes the alias — which is how the at-the-limit test proves the whole
/// body reached the handler rather than being truncated by the limit layer.
fn padded_cron_add_body(total_len: usize) -> Vec<u8> {
    let prefix = br#"{"agent":"no-such-agent","schedule":"0 0 * * *","name":""#;
    let suffix = br#""}"#;
    let pad = total_len
        .checked_sub(prefix.len() + suffix.len())
        .expect("requested body length must fit the JSON scaffolding");
    let mut body = Vec::with_capacity(total_len);
    body.extend_from_slice(prefix);
    body.resize(prefix.len() + pad, b'x');
    body.extend_from_slice(suffix);
    assert_eq!(body.len(), total_len);
    body
}

/// Create a shell cron job through the gateway's own API and return its id.
#[cfg(unix)]
async fn create_sleep_job(addr: SocketAddr, seconds: u32) -> String {
    let body = format!(
        r#"{{"agent":"{AGENT}","schedule":"0 0 * * *","job_type":"shell","command":"sleep {seconds}"}}"#
    );
    let (status, raw) = post(addr, "/api/cron", body.as_bytes(), Duration::from_secs(30)).await;
    assert_eq!(
        status, OK,
        "creating the shell cron job must succeed; response was:\n{raw}"
    );
    raw.split(r#""id":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no job id in cron-add response:\n{raw}"))
}

// ── Body limit: main router ────────────────────────────────────────────────

/// One byte over `MAX_BODY_SIZE` must be refused by
/// `RequestBodyLimitLayer::new(MAX_BODY_SIZE)` on the main router, before the
/// handler sees anything. Without that layer this request reaches
/// `handle_api_cron_add` and answers 400 (unknown agent) — which
/// `body_at_exactly_the_limit_reaches_the_handler` demonstrates empirically on
/// this same route, so 413-vs-400 is a sharp signal.
#[tokio::test]
async fn oversized_body_is_rejected_with_413_on_an_ordinary_api_route() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = base_config(tmp.path());
    let gw = boot(tmp, cfg).await;

    let body = padded_cron_add_body(MAX_BODY_SIZE + 1);
    let (status, raw) = post(gw.addr, "/api/cron", &body, Duration::from_secs(30)).await;

    assert_eq!(
        status,
        PAYLOAD_TOO_LARGE,
        "a {} byte body (MAX_BODY_SIZE + 1) must be rejected at the router \
         boundary, not handled; response was:\n{raw}",
        body.len()
    );
}

/// The complementary half: a body of exactly `MAX_BODY_SIZE` passes the limit
/// layer intact. The 400 comes from the handler rejecting an unconfigured
/// agent alias, and the alias appearing in the response body proves the full
/// 64 KB document was parsed rather than truncated.
#[tokio::test]
async fn body_at_exactly_the_limit_reaches_the_handler() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = base_config(tmp.path());
    let gw = boot(tmp, cfg).await;

    let body = padded_cron_add_body(MAX_BODY_SIZE);
    let (status, raw) = post(gw.addr, "/api/cron", &body, Duration::from_secs(30)).await;

    assert_eq!(
        status, BAD_REQUEST,
        "a body of exactly MAX_BODY_SIZE ({MAX_BODY_SIZE}) must pass the limit \
         layer and reach the handler; response was:\n{raw}"
    );
    assert!(
        raw.contains("no-such-agent"),
        "the handler must have parsed the whole {MAX_BODY_SIZE} byte document \
         (its error quotes the alias); response was:\n{raw}"
    );
}

// ── Body limit: long-running sub-router ────────────────────────────────────

/// `POST /api/cron/{id}/run` lives on a *separate* sub-router that carries its
/// own `RequestBodyLimitLayer::new(MAX_BODY_SIZE)`. Deleting that second call
/// would leave the cron-trigger endpoint unprotected while every main-router
/// test still passed, so it gets its own proof.
///
/// The under-limit probe runs first and is the point of the test: it fixes
/// what this route answers when the limit layer does *not* fire (404 for an
/// unknown job id), so the 413 that follows cannot be explained by anything
/// except the layer.
#[tokio::test]
async fn oversized_body_is_rejected_with_413_on_the_cron_run_route() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cfg = base_config(tmp.path());
    let gw = boot(tmp, cfg).await;

    let under = padded_cron_add_body(MAX_BODY_SIZE);
    let (under_status, under_raw) = post(
        gw.addr,
        "/api/cron/no-such-job/run",
        &under,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(
        under_status, NOT_FOUND,
        "control probe: a body at the limit must reach the handler, which \
         answers 404 for an unknown job id; response was:\n{under_raw}"
    );

    let body = padded_cron_add_body(MAX_BODY_SIZE + 1);
    let (status, raw) = post(
        gw.addr,
        "/api/cron/no-such-job/run",
        &body,
        Duration::from_secs(30),
    )
    .await;

    assert_eq!(
        status, PAYLOAD_TOO_LARGE,
        "the long-running sub-router carries its own RequestBodyLimitLayer; \
         without it this would be the same 404 the control probe just got. \
         Response was:\n{raw}"
    );
}

// ── Timeout wiring ─────────────────────────────────────────────────────────

/// Control for the timeout differential: an ordinary API route really is
/// bounded by `gateway.request_timeout_secs`, and by *that* value rather than
/// some other constant — see `assert_main_router_times_out_at` for how the
/// arrival window is bounded on both sides.
///
/// `long_running_request_timeout_secs` is set far away from the default here,
/// so a wiring swap that handed the main router the long budget would leave
/// the stalled request unanswered instead of landing in the window.
#[tokio::test]
async fn ordinary_api_routes_time_out_at_the_configured_request_timeout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut cfg = base_config(tmp.path());
    cfg.gateway.request_timeout_secs = 1;
    cfg.gateway.long_running_request_timeout_secs = 600;
    let gw = boot(tmp, cfg).await;

    assert_main_router_times_out_at(gw.addr, 1).await;
}

/// The long-running sub-router carries `long_running_request_timeout_secs`
/// rather than the gateway-wide default. `/a2a/{alias}` is the sibling route
/// registered on that same sub-router alongside `/api/cron/{id}/run`, and
/// unlike the cron route it takes a body extractor, so a stalled body makes
/// the sub-router's budget directly observable: the request can never finish,
/// so the 408 arrives exactly when the layer fires.
///
/// The latency window is bounded on both sides of the configured 6s, which is
/// what pins the *configured* value: a leaked 1s default lands below it, and a
/// hard-coded 10s or the 30s shipping default lands above it.
#[cfg(feature = "a2a")]
#[tokio::test]
async fn long_running_router_uses_the_configured_long_running_timeout() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut cfg = base_config(tmp.path());
    cfg.gateway.request_timeout_secs = 1;
    cfg.gateway.long_running_request_timeout_secs = 6;
    let gw = boot(tmp, cfg).await;

    let mut stream = open_request(gw.addr, "POST", &format!("/a2a/{AGENT}"), 512).await;
    stream
        .write_all(br#"{"jsonrpc":"2.0","#)
        .await
        .expect("write partial body");

    let started = std::time::Instant::now();

    // Three seconds is three times the default budget. A response here means
    // the sub-router inherited `request_timeout_secs` — the regression this
    // test exists to catch.
    let early = read_response(&mut stream, Duration::from_secs(3)).await;
    assert!(
        early.is_none(),
        "the long-running sub-router must not be bounded by \
         request_timeout_secs (1s); got an early response: {early:?}"
    );

    let (status, raw) = read_response(&mut stream, Duration::from_secs(40))
        .await
        .expect("the long-running TimeoutLayer must eventually answer");
    let elapsed = started.elapsed();

    assert_eq!(
        status, REQUEST_TIMEOUT,
        "the long-running sub-router must still enforce \
         long_running_request_timeout_secs; response was:\n{raw}"
    );
    assert!(
        (Duration::from_millis(4000)..=Duration::from_millis(9000)).contains(&elapsed),
        "the 408 must arrive at the configured 6s budget, not at some other \
         hard-coded value; it arrived after {elapsed:?}"
    );
}

// The two tests below run a real shell cron job to occupy the handler for a
// known duration. `sleep` is a POSIX utility with no cmd.exe equivalent, and
// the native runtime builds Windows commands through cmd.exe, so they are
// unix-only. `/api/cron/{id}/run` timeout coverage on Windows therefore rests
// on `long_running_router_uses_the_configured_long_running_timeout`, which
// covers the same sub-router through its `/a2a/{alias}` sibling.

/// Half one of the straddle, and the load-bearing cron-exemption assertion:
/// `POST /api/cron/{id}/run` runs a 5s job to completion under a 10s
/// long-running budget, while the same gateway cuts an ordinary route off at
/// 1s. The route takes no body extractor, so a stalled body proves nothing
/// about it — the request has to actually occupy the handler.
///
/// Paired with `cron_run_route_is_cut_off_when_the_budget_is_below_the_job`,
/// which runs the *identical* 5s job under a 3s budget. Any regression that
/// hard-codes a constant instead of reading
/// `long_running_request_timeout_secs` fails one of the two: a constant below
/// 5s times this test out, and a constant above 5s lets the other one through.
#[cfg(unix)]
#[tokio::test]
async fn cron_run_route_completes_when_the_budget_exceeds_the_job() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut cfg = base_config(tmp.path());
    cfg.gateway.request_timeout_secs = 1;
    cfg.gateway.long_running_request_timeout_secs = 10;
    let cfg = with_shell_cron_agent(cfg);
    let gw = boot(tmp, cfg).await;

    assert_main_router_times_out_at(gw.addr, 1).await;

    let job_id = create_sleep_job(gw.addr, 5).await;

    let started = std::time::Instant::now();
    let (status, raw) = post(
        gw.addr,
        &format!("/api/cron/{job_id}/run"),
        b"{}",
        Duration::from_secs(60),
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(
        status, OK,
        "a 5s job under a 10s long-running budget must run to completion; \
         response was:\n{raw}"
    );
    assert!(
        raw.contains(r#""success":true"#),
        "the shell job must have actually run — otherwise the request \
         finished quickly for an unrelated reason and proves nothing about \
         the timeout. Response was:\n{raw}"
    );
    assert!(
        (Duration::from_millis(4000)..=Duration::from_millis(9500)).contains(&elapsed),
        "the request must have been in flight for roughly the job's 5s, well \
         past the 1s default budget, but it returned after {elapsed:?}; \
         response was:\n{raw}"
    );
}

/// Half two of the straddle: the *identical* 5s job, now under a 3s
/// long-running budget, must be cut off at roughly 3s. Together with
/// `cron_run_route_completes_when_the_budget_exceeds_the_job` this pins the
/// configured value — the only workload is the same in both, so the differing
/// outcome can only come from `long_running_request_timeout_secs` being read.
///
/// It also shows the exemption is a configured budget rather than a removed
/// one, and the latency floor of 2.2s separates the 3s budget from the 1s
/// default that the control probe proves is live on the main router.
#[cfg(unix)]
#[tokio::test]
async fn cron_run_route_is_cut_off_when_the_budget_is_below_the_job() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let mut cfg = base_config(tmp.path());
    cfg.gateway.request_timeout_secs = 1;
    cfg.gateway.long_running_request_timeout_secs = 3;
    let cfg = with_shell_cron_agent(cfg);
    let gw = boot(tmp, cfg).await;

    assert_main_router_times_out_at(gw.addr, 1).await;

    let job_id = create_sleep_job(gw.addr, 5).await;

    let started = std::time::Instant::now();
    let (status, raw) = post(
        gw.addr,
        &format!("/api/cron/{job_id}/run"),
        b"{}",
        Duration::from_secs(60),
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(
        status, REQUEST_TIMEOUT,
        "the same 5s job under a 3s long-running budget must be cut off; a 200 \
         here means the budget is not the configured one. Response was:\n{raw}"
    );
    // Lower bound only. A correctly fired 3s timeout can be *observed* late
    // under scheduler contention, so an upper wall-clock bound here would turn
    // wakeup latency into a test failure. The configured value is pinned by
    // the pair instead: the same 5s job returns 200 under the 10s budget and
    // 408 here, and the lower bound rules out the 1s default.
    assert!(
        elapsed >= Duration::from_millis(2200),
        "the 408 must arrive no earlier than the configured 3s budget (later \
         than the 1s default), but it arrived after {elapsed:?}"
    );
}
