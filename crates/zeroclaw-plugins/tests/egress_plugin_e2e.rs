//! End-to-end proof of the plugin egress boundary (ADR-013 gate G2).
//!
//! Every test here drives a **real WASM component** through a **real
//! `wasi:http` call** into ZeroClaw's hooks. Nothing is stubbed: the deny path
//! is the same code the shipped host runs, and the allow path really opens a
//! TCP connection to a listener in this process.
//!
//! The fixture is a workspace member built on demand into its own target
//! directory so the nested Cargo invocation cannot contend with this test
//! process's build lock. A missing wasm target is a failure, never a skip — a
//! security boundary that silently stops being tested is worse than one that
//! is loudly broken.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::egress::{EgressHostService, EgressPolicy, EgressPolicyResolver};
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

// ── fixture provisioning ──────────────────────────────────────────

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/egress-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("egress-plugin-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-egress-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the egress component fixture");
            assert!(
                status.success(),
                "egress fixture must build; install the wasm32-wasip2 target"
            );

            let wasm = target_dir.join("wasm32-wasip2/debug/zeroclaw_egress_plugin_fixture.wasm");
            assert!(wasm.is_file(), "egress fixture WASM was not produced");
            wasm
        })
        .clone()
}

fn limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 32,
    }
}

/// The operator's grant for one instance, as the host-owned egress boundary
/// sees it. Built through the shared service rather than a test double: these
/// tests exist to prove the shipped authorization path, so the allowlist match,
/// the address-class verdict, and the pin all run for real.
fn policy(hosts: &[&str], allow_private: &[&str]) -> EgressHostService {
    let hosts: Vec<String> = hosts.iter().map(|h| (*h).to_string()).collect();
    let allow_private: Vec<String> = allow_private.iter().map(|h| (*h).to_string()).collect();
    EgressHostService::new(EgressPolicyResolver::new(move |_| {
        // No NAT64 prefixes and the shipped default connection ceiling: this
        // slice varies only the destination grant.
        EgressPolicy::new(&hosts, &allow_private, &[], 16)
    }))
}

/// Run the fixture's `execute` under `egress` and return its report line.
async fn probe(url: &str, follow: bool, egress: Option<EgressHostService>) -> String {
    let manifest = PluginManifest {
        name: "egress-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("egress-fixture.wasm".to_string()),
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
        // The binding is the `plugins.entries[].name` key the production
        // resolver looks the allowlist up by.
        "egress-fixture",
        [PluginPermission::HttpClient],
    )
    .expect("admit fixture scope");

    let mut plugin =
        zeroclaw_plugins::runtime::create_plugin_with_egress(&fixture(), &scope, limits(), egress)
            .await
            .expect("instantiate fixture tool");

    // The fixture requests no `config_read` and declares no schema, so this
    // resolves to the empty validated view production would hand the guest.
    let resolved = zeroclaw_plugins::config::resolve_plugin_config(&manifest, &scope, None)
        .expect("resolve empty fixture config");

    let args = format!(r#"{{"url":"{url}","follow":{follow}}}"#);
    let result = zeroclaw_plugins::runtime::call_execute(&mut plugin, args.as_bytes(), &resolved)
        .await
        .expect("fixture execute must return, denied or not");
    assert!(result.success, "fixture reported failure: {result:?}");
    result.output.to_string()
}

// ── local test server ─────────────────────────────────────────────

struct TestServer {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl TestServer {
    /// A minimal HTTP/1.1 responder. Deliberately raw `std::net` rather than a
    /// server framework: this file is proving what the host's client does, so
    /// the server should add no machinery of its own.
    fn start(response: &'static str) -> Self {
        Self::start_with(|_port| response.to_string())
    }

    /// Same, but the response is built from the bound port — needed when the
    /// canned response has to point a `Location` header back at this server.
    fn start_with(response: impl Fn(u16) -> String + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let port = listener.local_addr().expect("local addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();

        std::thread::spawn(move || {
            let body = response(port);
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                // Read just past the request head; the fixture sends no body.
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(body.as_bytes());
                let _ = stream.flush();
            }
        });

        Self { port, hits }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

const OK_RESPONSE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

/// A redirect pointing at the cloud metadata service — the classic SSRF pivot
/// an allowed host can offer a guest.
const REDIRECT_TO_METADATA: &str = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/iam/security-credentials/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

const METADATA_URL: &str = "http://169.254.169.254/latest/meta-data/";

/// The guest renders the host's `ErrorCode` with `{:?}`, so a denial arrives as
/// `InternalError(Some("zeroclaw plugin egress policy: ..."))`.
fn assert_denied_by_policy(report: &str, hop: &str) {
    let marker = format!("{hop}:error=");
    assert!(
        report.contains(&marker),
        "{hop} must have failed; got: {report}"
    );
    assert!(
        report.contains("egress policy"),
        "the guest's error must name the policy; got: {report}"
    );
    // The masked error must not hand the guest host internals.
    assert!(
        !report.contains("169.254.169.254") && !report.contains("non-global"),
        "denial leaked host network detail; got: {report}"
    );
}

// ── G2: deny by default ───────────────────────────────────────────

/// A store with `http_client` granted and **no** egress policy reaches nothing.
///
/// This is the property that shuts the self-grant path: the manifest permission
/// grants the `wasi:http` surface, never the destinations. The listener
/// assertion is the load-bearing half — it proves the request was refused
/// *before* any packet left, not merely that the guest saw an error.
#[tokio::test]
async fn deny_by_default_refuses_every_destination_without_a_policy() {
    let server = TestServer::start(OK_RESPONSE);
    let report = probe(&server.url("/"), false, None).await;

    assert_denied_by_policy(&report, "hop1");
    assert_eq!(
        server.hits(),
        0,
        "a denied request must never reach the socket; got: {report}"
    );
}

/// The same store, denied for a different reason: a policy exists but this
/// destination is not on it. Distinguishes "no policy" from "policy says no".
#[tokio::test]
async fn a_destination_outside_the_allowlist_is_refused() {
    let server = TestServer::start(OK_RESPONSE);
    let report = probe(
        &server.url("/"),
        false,
        Some(policy(&["api.example.com"], &[])),
    )
    .await;

    assert_denied_by_policy(&report, "hop1");
    assert_eq!(server.hits(), 0, "got: {report}");
}

// ── G2: allow via a seeded entry (and the private carveout) ────────

/// A granted destination is reachable, and the private-address carveout is what
/// makes a loopback destination legal. This is the positive control: without
/// it, every other test here would pass on a boundary that blocks everything.
#[tokio::test]
async fn an_allowlisted_destination_with_the_private_carveout_connects() {
    let server = TestServer::start(OK_RESPONSE);
    let report = probe(
        &server.url("/health"),
        false,
        Some(policy(&["127.0.0.1"], &["127.0.0.1"])),
    )
    .await;

    assert!(
        report.contains("hop1:status=200"),
        "an allowlisted destination must connect; got: {report}"
    );
    assert_eq!(server.hits(), 1, "got: {report}");
}

/// The carveout is required, not incidental: the *same* allowlist without it
/// is refused, because loopback is a blocked address class above the allowlist.
#[tokio::test]
async fn an_allowlisted_private_destination_without_the_carveout_is_refused() {
    let server = TestServer::start(OK_RESPONSE);
    let report = probe(&server.url("/"), false, Some(policy(&["127.0.0.1"], &[]))).await;

    assert_denied_by_policy(&report, "hop1");
    assert_eq!(
        server.hits(),
        0,
        "the address-class check must refuse before connecting; got: {report}"
    );
}

// ── G2: metadata is always refused ────────────────────────────────

/// Cloud metadata stays refused through an allowlist that names it explicitly
/// *and* a carveout that opens its address class. There is no configuration
/// that reaches it — that is the point of validating resolved addresses above
/// the allowlist rather than inside it.
#[tokio::test]
async fn cloud_metadata_is_refused_even_when_allowlisted_with_a_carveout() {
    let report = probe(
        METADATA_URL,
        false,
        Some(policy(&["169.254.169.254"], &["169.254.169.254"])),
    )
    .await;

    assert!(
        report.contains("hop1:error="),
        "metadata must never be reachable; got: {report}"
    );
    assert!(
        report.contains("egress policy"),
        "the guest's error must name the policy; got: {report}"
    );
}

// ── G2: redirect chase ────────────────────────────────────────────

/// An allowed host redirects toward a blocked class; the guest chases it with a
/// second request; that request is denied on its own merits.
///
/// The host never follows redirects, so hop 2 is a fresh guest-issued request
/// that passes the full policy independently. Asserting hop 1 *succeeded* is
/// what makes this meaningful: the denial is specific to the second hop, not a
/// blanket failure.
#[tokio::test]
async fn a_redirect_toward_a_blocked_class_is_denied_on_the_second_hop() {
    let server = TestServer::start(REDIRECT_TO_METADATA);
    let report = probe(
        &server.url("/redirect"),
        true,
        Some(policy(&["127.0.0.1"], &["127.0.0.1"])),
    )
    .await;

    assert!(
        report.contains("hop1:status=302"),
        "the first hop must succeed; got: {report}"
    );
    assert_denied_by_policy(&report, "hop2");
    assert_eq!(
        server.hits(),
        1,
        "only the first hop may touch the network; got: {report}"
    );
}

/// The second hop is re-checked against the **allowlist**, not merely against
/// the address classes.
///
/// This is the mutation-sensitive half of the redirect story. The two metadata
/// redirect tests above would still pass if the hooks cached the first hop's
/// approval, because metadata is refused by resolved-address validation no
/// matter what the allowlist said. Here the second hop is `localhost`, which
/// resolves to the very same loopback address the first hop was allowed to
/// reach — so the *only* thing that can deny it is re-running the allowlist
/// match for the new host. If per-hop authorization is ever skipped or cached,
/// this test connects and fails.
#[tokio::test]
async fn a_redirect_to_an_ungranted_name_on_the_same_address_is_denied() {
    let server = TestServer::start_with(|port| {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://localhost:{port}/second\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    });

    let report = probe(
        &server.url("/redirect"),
        true,
        // `127.0.0.1` is granted; the name `localhost` is not, even though the
        // two resolve to the same address.
        Some(policy(&["127.0.0.1"], &["127.0.0.1"])),
    )
    .await;

    assert!(
        report.contains("hop1:status=302"),
        "the first hop must succeed; got: {report}"
    );
    assert_denied_by_policy(&report, "hop2");
    assert_eq!(
        server.hits(),
        1,
        "the second hop must never reach the socket; got: {report}"
    );
}

/// Same redirect, but the operator granted the metadata address and its
/// carveout too. The second hop is still denied: no allowlist reaches metadata.
#[tokio::test]
async fn a_redirect_to_metadata_is_denied_even_when_metadata_is_allowlisted() {
    let server = TestServer::start(REDIRECT_TO_METADATA);
    let report = probe(
        &server.url("/redirect"),
        true,
        Some(policy(
            &["127.0.0.1", "169.254.169.254"],
            &["127.0.0.1", "169.254.169.254"],
        )),
    )
    .await;

    assert!(report.contains("hop1:status=302"), "got: {report}");
    assert!(
        report.contains("hop2:error=") && report.contains("egress policy"),
        "got: {report}"
    );
}

// ── the strict grammar, end to end ────────────────────────────────

/// An IP grant is exact, at the live boundary and not only in the matcher's
/// unit tests: a neighbouring address, and a suffix pattern that a naive
/// `ends_with` would let through, both fail to reach the listener.
#[tokio::test]
async fn ip_grants_are_exact_at_the_live_boundary() {
    let server = TestServer::start(OK_RESPONSE);

    // A different loopback address is a different destination.
    let report = probe(
        &server.url("/"),
        false,
        Some(policy(&["127.0.0.2"], &["127.0.0.2"])),
    )
    .await;
    assert_denied_by_policy(&report, "hop1");
    assert_eq!(server.hits(), 0, "got: {report}");

    // A suffix pattern must not match an IP-literal host. `*.0.0.1` would
    // match `127.0.0.1` under a plain suffix comparison.
    let report = probe(
        &server.url("/"),
        false,
        Some(policy(&["*.0.0.1"], &["127.0.0.1"])),
    )
    .await;
    assert_denied_by_policy(&report, "hop1");
    assert_eq!(server.hits(), 0, "got: {report}");
}
