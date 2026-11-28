//! RFC 8252 §7.3 loopback callback listener for the MCP OAuth browser flow.
//!
//! Same shape as the runtime enrollment listener
//! (`zeroclaw-runtime/src/security/auth_provider/enrollment.rs`), which this
//! crate cannot import (dependency direction): bind an ephemeral 127.0.0.1
//! port before launching the browser, serve connections one at a time with a
//! short per-connection read deadline, consume exactly one callback that
//! carries this flow's state, and shut down.

use anyhow::{Context, Result, bail};
use std::time::Duration;

use super::pkce::Pkce;

/// How long one connection has to deliver its request line. The deadline
/// covers the whole read rather than each `read` call, so a client that
/// dribbles bytes cannot extend its hold on the listener either.
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// How many connections may burn that deadline before the wait gives up.
/// Localhost port probes are ordinary background noise on a developer
/// machine; only a connection that goes silent holds the port. Past this
/// many stalls the port is held by something else, and a named failure tells
/// the operator more than a silent wait until the flow deadline.
const MAX_STALLED_CALLBACK_CONNECTIONS: usize = 32;

/// Outer bound on the human half of the flow: browser start, IdP login (an
/// operator may be hunting for a passkey), and the redirect back. The
/// listener itself holds the port; nothing is consumed until the callback
/// arrives, so giving up is safe and the operator can restart the flow.
pub const CALLBACK_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

const SUCCESS_PAGE: &str = "<!doctype html><html><head><title>ZeroClaw</title></head>\
                            <body><h1>Sign-in complete</h1><p>You can close this tab.</p></body></html>";

const FAILURE_PAGE: &str = "<!doctype html><html><head><title>ZeroClaw</title></head>\
                            <body><h1>Sign-in failed</h1><p>Return to ZeroClaw for the error.</p></body></html>";

/// Identity of the flow whose callback the listener consumes: the one-shot
/// state plus the validated issuer for the RFC 9207 `iss` check.
///
/// `Debug` deliberately prints the ephemeral `state`: it rides the
/// authorization URL the operator is shown (design §6) and is a CSRF
/// gesture, not a bearer credential. The PKCE verifier inside `pkce` is
/// redacted by `Pkce`'s hand-written `Debug`.
#[derive(Debug, Clone)]
pub struct CallbackFlow {
    pub state: String,
    /// The validated authorization-server issuer, used to check the `iss`
    /// parameter (RFC 9207) — and to REQUIRE its presence when the server
    /// advertised support (see `require_iss`).
    pub issuer: String,
    /// RFC 9207 §2.2: set from AS metadata
    /// (`authorization_response_iss_parameter_supported`). When true, a
    /// callback WITHOUT `iss` fails the flow: with several MCP servers each
    /// holding a loopback flow, an `iss`-less response from an AS that
    /// promises the parameter cannot be proven to belong to this one.
    pub require_iss: bool,
    /// The flow's PKCE material, carried so the wait hands the caller one
    /// complete verdict without threading two arguments through.
    pub pkce: Pkce,
}

impl CallbackFlow {
    /// RFC 9207 §2.1: a mixed-up response (an `iss` naming another issuer)
    /// fails the flow. A server that sends no `iss` is allowed only when it
    /// did not advertise the parameter: the check is skipped because
    /// discovery already bound this flow's endpoints to the validated
    /// issuer. Trailing-slash differences are tolerated; anything else must
    /// match exactly.
    pub fn check_callback_issuer(&self, iss: Option<&str>) -> Result<()> {
        let Some(iss) = iss else {
            anyhow::ensure!(
                !self.require_iss,
                "the sign-in response omitted the `iss` parameter that this \
                 authorization server requires (RFC 9207)"
            );
            return Ok(());
        };
        let expected = self.issuer.trim_end_matches('/');
        let got = iss.trim_end_matches('/');
        anyhow::ensure!(
            got == expected,
            "the sign-in response named issuer {:?}, but this flow was started against {}",
            super::truncate_untrusted(got),
            self.issuer
        );
        Ok(())
    }
}

enum CallbackParse {
    Code(String),
    IdpError(String),
    IssuerMismatch(String),
    Ignore,
}

/// The one-shot loopback callback listener (RFC 8252). Requests that do not
/// carry this flow's state get a fixed page and the wait continues; nothing
/// from any request is echoed back into the response.
pub struct LoopbackListener {
    listener: tokio::net::TcpListener,
    port: u16,
}

impl LoopbackListener {
    /// Bind before launching the browser: the redirect URI must exist before
    /// the authorization URL leaves the process, or the callback races the
    /// bind.
    pub async fn bind() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("cannot bind a loopback callback port for MCP OAuth")?;
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    /// The redirect URI to start the flow with (registered verbatim with the
    /// authorization server, so it must not change after bind).
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Wait for the callback carrying `flow`'s state. An IdP `error` response
    /// for the matching state fails the flow, as does a response whose `iss`
    /// names another issuer (RFC 9207); everything else (wrong path, wrong
    /// state, unparsable, or never sent) is answered or dropped and skipped.
    /// `timeout` bounds the whole wait; within it each connection gets
    /// `CALLBACK_READ_TIMEOUT`, and at most `MAX_STALLED_CALLBACK_CONNECTIONS`
    /// may expire it. A request answered promptly never counts against that
    /// bound. Consumes exactly one matching callback, then the listener drops.
    pub async fn wait_for_code(self, flow: &CallbackFlow, timeout: Duration) -> Result<String> {
        match tokio::time::timeout(timeout, self.accept_loop(flow)).await {
            Ok(result) => result,
            Err(_) => bail!("timed out waiting for the MCP OAuth browser sign-in to complete"),
        }
    }

    async fn accept_loop(&self, flow: &CallbackFlow) -> Result<String> {
        let mut stalled = 0usize;
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            // A connection that does not complete a request line in time is
            // closed and costs nothing but its turn: it is not the callback,
            // so it does not consume the one-shot and the wait goes on.
            let target = tokio::time::timeout(
                CALLBACK_READ_TIMEOUT,
                Self::read_request_target(&mut stream),
            )
            .await;
            let target = match target {
                Ok(Ok(target)) => target,
                // Only a stall costs the wait real time, so only a stall is
                // counted. A client that says something and is answered
                // leaves the listener free for the browser however many of
                // them arrive, which is what a port probe does.
                Err(_) => {
                    stalled += 1;
                    if stalled >= MAX_STALLED_CALLBACK_CONNECTIONS {
                        bail!(
                            "the MCP OAuth callback port was busy with other connections before the browser reached it"
                        );
                    }
                    continue;
                }
                Ok(Err(_)) => continue,
            };
            match Self::parse_callback(&target, flow) {
                CallbackParse::Code(code) => {
                    Self::respond(&mut stream, 200, SUCCESS_PAGE).await;
                    return Ok(code);
                }
                CallbackParse::IdpError(err) => {
                    Self::respond(&mut stream, 200, FAILURE_PAGE).await;
                    bail!("the identity provider denied the sign-in: {err}");
                }
                CallbackParse::IssuerMismatch(err) => {
                    Self::respond(&mut stream, 400, FAILURE_PAGE).await;
                    bail!("{err}");
                }
                CallbackParse::Ignore => {
                    Self::respond(&mut stream, 404, FAILURE_PAGE).await;
                }
            }
        }
    }

    async fn read_request_target(stream: &mut tokio::net::TcpStream) -> Result<String> {
        use tokio::io::AsyncReadExt as _;
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 512];
        loop {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(2).any(|w| w == b"\r\n") || buf.len() > 8192 {
                break;
            }
        }
        let line = buf.split(|&b| b == b'\r').next().unwrap_or_default();
        let line = std::str::from_utf8(line).context("request line is not UTF-8")?;
        let mut parts = line.split(' ');
        if parts.next() != Some("GET") {
            bail!("not a GET request");
        }
        Ok(parts.next().unwrap_or_default().to_string())
    }

    fn parse_callback(target: &str, flow: &CallbackFlow) -> CallbackParse {
        let Ok(url) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
            return CallbackParse::Ignore;
        };
        if url.path() != "/callback" {
            return CallbackParse::Ignore;
        }
        let mut state = None;
        let mut code = None;
        let mut error = None;
        let mut error_description = None;
        let mut iss = None;
        let mut duplicate_key = false;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "state" => {
                    duplicate_key |= state.is_some();
                    state = Some(value.into_owned());
                }
                "code" => {
                    duplicate_key |= code.is_some();
                    code = Some(value.into_owned());
                }
                "error" => {
                    duplicate_key |= error.is_some();
                    error = Some(value.into_owned());
                }
                "error_description" => error_description = Some(value.into_owned()),
                "iss" => {
                    duplicate_key |= iss.is_some();
                    iss = Some(value.into_owned());
                }
                _ => {}
            }
        }
        if duplicate_key {
            // RFC 6749 forbids repeating these parameters; last-wins would
            // let one copy hide another's value, so refuse to act on it.
            return CallbackParse::Ignore;
        }
        // State gates everything: an error without this flow's state is some
        // other request's business, not a verdict on this flow.
        if state.as_deref() != Some(flow.state.as_str()) {
            return CallbackParse::Ignore;
        }
        // The issuer check precedes both the error and the code: a mixed-up
        // response is not acted on either way.
        if let Err(e) = flow.check_callback_issuer(iss.as_deref()) {
            return CallbackParse::IssuerMismatch(e.to_string());
        }
        if let Some(err) = error {
            let detail = error_description
                .map(|d| format!(" ({})", super::truncate_untrusted(&d)))
                .unwrap_or_default();
            return CallbackParse::IdpError(format!("{}{detail}", super::truncate_untrusted(&err)));
        }
        match code {
            Some(code) => CallbackParse::Code(code),
            None => CallbackParse::Ignore,
        }
    }

    async fn respond(stream: &mut tokio::net::TcpStream, status: u16, body: &str) {
        use tokio::io::AsyncWriteExt as _;
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            _ => "Not Found",
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(state: &str) -> CallbackFlow {
        CallbackFlow {
            state: state.to_string(),
            issuer: "https://auth.example.com".to_string(),
            require_iss: false,
            pkce: Pkce::from_verifier("verifier"),
        }
    }

    #[test]
    fn require_iss_refuses_callback_without_iss() {
        let mut f = flow("s1");
        f.require_iss = true;
        let err = f
            .check_callback_issuer(None)
            .expect_err("advertised support makes a missing iss a flow failure");
        assert!(err.to_string().contains("RFC 9207"), "got: {err}");
        // A present, matching iss still passes; a foreign one still fails.
        f.check_callback_issuer(Some("https://auth.example.com"))
            .expect("matching iss ok");
        f.check_callback_issuer(Some("https://other.example"))
            .expect_err("foreign iss still fails");
        // Without the advertisement, absence stays acceptable (mixed-mode AS).
        f.require_iss = false;
        f.check_callback_issuer(None).expect("optional iss ok");
    }

    async fn probe(port: u16, request: &str) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("read response");
        response
    }

    #[test]
    fn issuer_check_tolerates_absent_iss_and_trailing_slash_only() {
        let f = flow("s");
        f.check_callback_issuer(None)
            .expect("absent iss is allowed");
        f.check_callback_issuer(Some("https://auth.example.com"))
            .expect("exact match");
        f.check_callback_issuer(Some("https://auth.example.com/"))
            .expect("trailing slash tolerated");
        f.check_callback_issuer(Some("https://evil.example"))
            .expect_err("different issuer must fail");
        f.check_callback_issuer(Some("https://auth.example.com/different-path"))
            .expect_err("path difference is not a slash difference");
    }

    #[tokio::test]
    async fn aborted_wait_releases_the_port() {
        // Cancellation posture (plan Phase 3): an aborted wait must free
        // the loopback port — the listener drops with the future, no
        // zombie accept loop holding the redirect URI open.
        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("never"), Duration::from_secs(30))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        wait.abort();
        let _ = wait.await; // JoinError (cancelled) is expected
        let probe = tokio::net::TcpStream::connect(("127.0.0.1", port)).await;
        assert!(
            probe.is_err(),
            "port {port} must be refused after the wait is aborted"
        );
    }

    #[tokio::test]
    async fn callback_without_code_and_without_error_is_ignored_and_wait_continues() {
        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("abc"), CALLBACK_WAIT_TIMEOUT)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Matching state but neither `code` nor `error`: nothing to consume;
        // the wait must stay alive and answer with the fixed 404 page.
        let response = probe(
            port,
            "GET /callback?state=abc&junk=1 HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 404"), "got: {response}");
        // The real callback then completes the same wait.
        let response = probe(
            port,
            "GET /callback?code=real&state=abc HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
        let code = wait.await.expect("task").expect("flow");
        assert_eq!(code, "real");
    }

    #[tokio::test]
    async fn matching_callback_completes_the_wait() {
        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        assert!(
            listener
                .redirect_uri()
                .ends_with(&format!(":{port}/callback"))
        );
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("abc"), CALLBACK_WAIT_TIMEOUT)
                .await
        });
        // Give the accept loop a moment, then deliver the browser callback.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let response = probe(
            port,
            "GET /callback?code=code-1&state=abc HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
        let code = wait.await.expect("task").expect("flow");
        assert_eq!(code, "code-1");
    }

    #[tokio::test]
    async fn wrong_state_and_foreign_paths_are_skipped_not_consumed() {
        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("good"), CALLBACK_WAIT_TIMEOUT)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Foreign state: fixed 404 page, wait continues, no echo of anything.
        let response = probe(
            port,
            "GET /callback?code=x&state=other HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 404"), "got: {response}");
        assert!(!response.contains("other"), "response echoed the state");
        // Favicon noise: skipped.
        probe(port, "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n").await;
        // The real callback still lands.
        let response = probe(
            port,
            "GET /callback?code=real&state=good HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
        assert_eq!(wait.await.expect("task").expect("flow"), "real");
    }

    #[tokio::test]
    async fn duplicated_protocol_parameters_are_refused_not_consumed() {
        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("good"), CALLBACK_WAIT_TIMEOUT)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        // RFC 6749 forbids repeating these parameters: acting on a
        // last-wins winner would let one copy hide another's value, so the
        // request is refused with the fixed page and the one-shot stays
        // armed.
        let response = probe(
            port,
            "GET /callback?state=good&state=evil&code=x HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 404"), "got: {response}");
        // The clean callback still consumes the one-shot.
        let response = probe(
            port,
            "GET /callback?code=real&state=good HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
        assert_eq!(wait.await.expect("task").expect("flow"), "real");
    }

    #[tokio::test]
    async fn idp_error_and_issuer_mismatch_fail_the_flow() {
        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("s1"), CALLBACK_WAIT_TIMEOUT)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        probe(port, "GET /callback?error=access_denied&error_description=nope&state=s1 HTTP/1.1\r\nHost: x\r\n\r\n").await;
        let err = wait
            .await
            .expect("task")
            .expect_err("denial must fail the flow");
        assert!(
            err.to_string().contains("access_denied (nope)"),
            "got: {err}"
        );

        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("s2"), CALLBACK_WAIT_TIMEOUT)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        probe(
            port,
            "GET /callback?code=c&state=s2&iss=https://evil.example HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        let err = wait
            .await
            .expect("task")
            .expect_err("iss mismatch must fail");
        assert!(err.to_string().contains("issuer"), "got: {err}");
    }

    #[tokio::test]
    async fn silent_connections_cannot_park_the_real_callback() {
        let listener = LoopbackListener::bind().await.expect("bind");
        let port = listener.port;
        let wait = zeroclaw_spawn::spawn!(async move {
            listener
                .wait_for_code(&flow("s3"), CALLBACK_WAIT_TIMEOUT)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        // A local process that connects and says nothing: the read deadline
        // drops it, and the browser's callback still lands afterwards.
        let silent_stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("silent connect");
        tokio::time::sleep(CALLBACK_READ_TIMEOUT + Duration::from_millis(200)).await;
        let response = probe(
            port,
            "GET /callback?code=after&state=s3 HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
        assert_eq!(wait.await.expect("task").expect("flow"), "after");
        drop(silent_stream);
    }

    #[tokio::test]
    async fn wait_times_out_when_nobody_calls_back() {
        let listener = LoopbackListener::bind().await.expect("bind");
        let err = listener
            .wait_for_code(&flow("never"), Duration::from_millis(150))
            .await
            .expect_err("must time out");
        assert!(err.to_string().contains("timed out"), "got: {err}");
    }
}
