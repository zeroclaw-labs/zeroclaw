//! End-to-end regression for several **sequential** outbound requests issued
//! from one tool-plugin `execute`, reported as intermittently failing on the
//! later calls.
//!
//! A real `wasm32-wasip2` component drives a real `waki` blocking client
//! through ZeroClaw's `wasi:http` hooks at a loopback server that keeps its
//! connections alive. Nothing here is stubbed; the send path, the egress
//! authorization, and the per-instance connection budget are the shipped ones.
//!
//! The report's own framing — a connection being reused after it is no longer
//! valid — is checked here too, and it is not what the host does: the
//! server counts one accepted connection per guest request, so there is no
//! reuse to go stale. What the sequential shape really costs is the instance's
//! **connection budget**, which is a ceiling on connections that are live at
//! the same moment, and every request that is still holding a socket open
//! spends one.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::egress::{EgressHostService, EgressPolicy, EgressPolicyResolver};
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use support::admit_fixture;

// ── fixture provisioning ──────────────────────────────────────────

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/tool-sequential-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tool-sequential-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-tool-sequential-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the sequential component fixture");
            assert!(
                status.success(),
                "sequential fixture must build; install the wasm32-wasip2 target"
            );

            let wasm = target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_sequential_fixture.wasm");
            assert!(wasm.is_file(), "sequential fixture WASM was not produced");
            wasm
        })
        .clone()
}

fn limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 256 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 32,
        call_timeout: Duration::from_secs(60),
    }
}

/// The operator's grant, built through the shipped service so the allowlist
/// match, the address-class verdict, and the connection accounting all run for
/// real.
///
/// The ceiling is the **shipped default**, read from the config schema rather
/// than written down again here: what this file is about is what a plugin
/// author meets on a stock host, so a number that drifts away from the default
/// would quietly stop testing that.
fn policy(max_connections: usize) -> EgressHostService {
    EgressHostService::new(EgressPolicyResolver::new(move |_| {
        EgressPolicy::new(
            &["127.0.0.1".to_string()],
            &["127.0.0.1".to_string()],
            &[],
            max_connections,
        )
    }))
}

fn default_max_connections_per_instance() -> usize {
    zeroclaw_config::schema::PluginLimitsConfig::default().max_connections_per_instance
}

/// Run the fixture's `execute` and return its report line.
///
/// `binding` is the `plugins.entries[].name` key that, with the package name,
/// forms the canonical instance identity the connection budget is counted
/// against. That count is process-wide, so every test here needs its own
/// binding or two tests under a parallel runner would spend each other's
/// ceiling.
async fn probe(
    binding: &str,
    url: &str,
    count: usize,
    mode: Mode,
    max_connections: usize,
) -> String {
    let manifest = PluginManifest {
        name: "tool-sequential-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("sequential-fixture.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Tool],
        permissions: vec![PluginPermission::HttpClient],
        config_schema: None,
        signature: None,
        publisher_key: None,
        egress: Default::default(),
    };
    let scope = PluginInstanceScope::from_manifest(
        &manifest,
        PluginCapability::Tool,
        binding,
        [PluginPermission::HttpClient],
    )
    .expect("admit fixture scope");

    let services = {
        let manifest = manifest.clone();
        PluginHostServices::new(PluginConfigResolver::new(move |scope| {
            resolve_plugin_config(&manifest, scope, None)
        }))
    };

    let mut plugin = zeroclaw_plugins::runtime::create_plugin_with_egress(
        &admit_fixture(&fixture(), &manifest),
        &scope,
        &services,
        limits(),
        Some(policy(max_connections)),
    )
    .await
    .expect("instantiate sequential fixture tool");

    let args = format!(
        r#"{{"url":"{url}","count":{count},"hold":{hold},"discard":{discard}}}"#,
        hold = matches!(mode, Mode::Retained),
        discard = matches!(mode, Mode::Discarded),
    );
    let result = zeroclaw_plugins::runtime::call_execute(&mut plugin, args.as_bytes())
        .await
        .expect("fixture execute must return");
    assert!(result.success, "fixture reported failure: {result:?}");
    result.output.to_string()
}

/// What the guest does with each response before issuing the next request.
#[derive(Clone, Copy)]
enum Mode {
    /// Read the body to the end and drop the response.
    Drained,
    /// Look at the status and drop the response without reading the body —
    /// the retry shape the report described.
    Discarded,
    /// Keep every response alive until the last request has been issued.
    Retained,
}

// ── local keep-alive test server ──────────────────────────────────

/// An HTTP/1.1 server that answers every request on a connection and never
/// closes it first.
///
/// Two properties are deliberate. It omits `Connection: close`, so a client is
/// free to keep the socket — which is what lets the test tell "the host opened
/// one connection per request" apart from "the host reused one". And its body
/// is larger than what a client can buffer while reading the response head, so
/// a response the guest has not finished reading is genuinely still occupying
/// a socket. A two-byte body would arrive whole with the head, and the
/// connection would then complete no matter what the guest did with the
/// response — which would make the retained case untestable rather than
/// passing.
struct KeepAliveServer {
    port: u16,
    connections: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
}

/// Comfortably past a loopback socket buffer, so the body cannot be delivered
/// in the same read as the response head.
const BODY_BYTES: usize = 512 * 1024;

impl KeepAliveServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let port = listener.local_addr().expect("local addr").port();
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::clone(&connections);
        let served = Arc::clone(&requests);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                accepted.fetch_add(1, Ordering::SeqCst);
                let served = Arc::clone(&served);
                // One thread per connection: the point of the server is that
                // several connections can be open at once, which a serial
                // accept loop could not show.
                std::thread::spawn(move || serve(stream, &served));
            }
        });

        Self {
            port,
            connections,
            requests,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/rpc", self.port)
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// Serve requests on one connection until the peer goes away.
fn serve(stream: TcpStream, served: &AtomicUsize) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone connection"));
    let mut writer = stream;
    loop {
        // Request line plus headers, then the whole request body. Draining the
        // body is not politeness: leftover body bytes on a kept-alive
        // connection would be read as the next request line, and the server
        // would count one request per frame instead of one per POST.
        let mut length = 0_usize;
        let mut chunked = false;
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        loop {
            let mut header = String::new();
            match reader.read_line(&mut header) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let trimmed = header.trim_end();
            if trimmed.is_empty() {
                break;
            }
            let Some((name, value)) = trimmed.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length")
                && let Ok(parsed) = value.parse::<usize>()
            {
                length = parsed;
            }
            if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
        // A guest-written request body reaches the wire as whichever framing
        // the host's client picked, and a body of unknown length is chunked.
        if chunked {
            if !drain_chunked(&mut reader) {
                return;
            }
        } else if length > 0 {
            let mut body = vec![0_u8; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
        }

        served.fetch_add(1, Ordering::SeqCst);
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {BODY_BYTES}\r\n\r\n");
        if writer.write_all(head.as_bytes()).is_err()
            || writer.write_all(&vec![b'x'; BODY_BYTES]).is_err()
            || writer.flush().is_err()
        {
            return;
        }
    }
}

/// Read a chunked request body up to and including its terminating chunk.
/// Returns false when the peer went away mid-body.
fn drain_chunked(reader: &mut BufReader<TcpStream>) -> bool {
    loop {
        let mut size_line = String::new();
        match reader.read_line(&mut size_line) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        let size_text = size_line.trim_end();
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            return false;
        };
        if size == 0 {
            // Trailers, then the blank line that ends the body.
            loop {
                let mut trailer = String::new();
                match reader.read_line(&mut trailer) {
                    Ok(0) | Err(_) => return false,
                    Ok(_) => {}
                }
                if trailer.trim_end().is_empty() {
                    return true;
                }
            }
        }
        let mut chunk = vec![0_u8; size + 2];
        if reader.read_exact(&mut chunk).is_err() {
            return false;
        }
    }
}

// ── the regressions ───────────────────────────────────────────────

/// Twenty sequential POSTs from one `execute`, each response read and dropped
/// before the next request starts.
///
/// This is the reported call shape at more than the default connection ceiling
/// of sixteen. Every call must succeed: the requests are sequential, so at no
/// point is more than one connection live, and a ceiling on *simultaneous*
/// connections has nothing to refuse.
///
/// The connection count is the second half of the claim. One accepted
/// connection per request proves the host never reuses one, so the report's
/// "stale reused connection" cannot be the mechanism — and it proves the
/// twenty calls really went to the network rather than being answered from
/// anywhere else.
#[tokio::test]
async fn twenty_sequential_requests_from_one_execute_all_succeed() {
    let server = KeepAliveServer::start();
    let calls = 20;
    assert!(
        calls > default_max_connections_per_instance(),
        "the probe must cross the shipped connection ceiling to be a regression"
    );

    let report = probe(
        "sequential-drained",
        &server.url(),
        calls,
        Mode::Drained,
        default_max_connections_per_instance(),
    )
    .await;

    assert_eq!(
        report,
        format!("calls={calls} ok={calls}"),
        "every sequential call must succeed; got: {report}"
    );
    assert_eq!(
        server.requests(),
        calls,
        "every call must have reached the server; got: {report}"
    );
    assert_eq!(
        server.connections(),
        calls,
        "the host opens one connection per request and reuses none; got: {report}"
    );
}

/// Twenty sequential calls under a ceiling of **one** connection.
///
/// This is the sharp form of the property above. Sequential requests need one
/// connection at a time and no more, so the ceiling they must fit under is
/// one — but only if the host returns the slot when the request that took it
/// is finished. A slot that is returned late, or only when the peer closes the
/// socket, turns the second call into a denial here, where the default ceiling
/// of sixteen would have hidden it for another fifteen.
///
/// The server's body is deliberately larger than a socket buffer, so the
/// connection is genuinely still open while the guest reads the response: this
/// is not passing because the whole exchange completed before the slot was
/// ever needed.
#[tokio::test]
async fn sequential_calls_fit_under_a_ceiling_of_one_connection() {
    let server = KeepAliveServer::start();
    let calls = 20;

    let report = probe(
        "sequential-single-slot",
        &server.url(),
        calls,
        Mode::Drained,
        1,
    )
    .await;

    assert_eq!(
        report,
        format!("calls={calls} ok={calls}"),
        "a sequential call must give its connection slot back before the next one asks for it; got: {report}"
    );
    assert_eq!(
        server.requests(),
        calls,
        "every call must have reached the server; got: {report}"
    );
}

/// Twenty sequential calls whose responses are thrown away unread.
///
/// This is the report's retry shape: a caller that looks at a response, decides
/// to try the next node, and drops it. The body was never drained, so the host
/// tears the connection down instead of letting it finish — and the slot has to
/// come back on that path too, not only on the tidy one. A ceiling of one makes
/// the very next call the one that proves it.
#[tokio::test]
async fn discarded_responses_return_their_connection_slot() {
    let server = KeepAliveServer::start();
    let calls = 20;

    let report = probe(
        "sequential-discarded",
        &server.url(),
        calls,
        Mode::Discarded,
        1,
    )
    .await;

    assert_eq!(
        report,
        format!("calls={calls} ok={calls}"),
        "a response dropped unread must still release its slot before the next call; got: {report}"
    );
    assert_eq!(
        server.requests(),
        calls,
        "every call must have reached the server; got: {report}"
    );
}

/// The same twenty calls, but every response is kept alive until the last
/// request has been issued.
///
/// A `waki::Response` owns the `wasi:http` incoming-body resource, so this is
/// the honest worst case for a plugin that collects results before parsing
/// them: at the twentieth request, nineteen earlier responses are still open.
///
/// Reaching the ceiling here is the budget working — sixteen sockets really are
/// open — so the assertion is not that the call succeeds. It is that the guest
/// is told *which* limit it hit. Reported as a destination denial, this is the
/// bug the author cannot fix: the message names the allowlist, the allowlist is
/// correct, and no edit to it will ever help. `wasi:http` has a code for a full
/// connection budget, and that is what must come back.
#[tokio::test]
async fn a_full_connection_budget_is_reported_as_a_connection_limit() {
    let server = KeepAliveServer::start();
    let calls = 20;
    let ceiling = default_max_connections_per_instance();
    assert!(
        calls > ceiling,
        "the probe must cross the shipped connection ceiling to reach the limit at all"
    );

    let report = probe(
        "sequential-retained",
        &server.url(),
        calls,
        Mode::Retained,
        ceiling,
    )
    .await;

    assert!(
        report.contains(&format!("first_error={}:", ceiling + 1)),
        "the ceiling must bind on the call after the last slot is spent; got: {report}"
    );
    assert!(
        report.contains("ConnectionLimitReached"),
        "a full connection budget must reach the guest as the wasi:http code for it; got: {report}"
    );
    assert!(
        !report.contains("egress policy"),
        "running out of connection slots must not be reported as a destination denial; got: {report}"
    );
}
