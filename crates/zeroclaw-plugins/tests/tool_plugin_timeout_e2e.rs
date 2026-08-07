//! Real component-boundary regressions for host-owned plugin wall time.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::{PluginCapability, PluginPermission};

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/tool-timeout-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tool-timeout-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-tool-timeout-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the timeout component fixture");
            assert!(
                status.success(),
                "timeout fixture must build; install the wasm32-wasip2 target"
            );
            let wasm = target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_timeout_fixture.wasm");
            assert!(wasm.is_file(), "timeout fixture WASM was not produced");
            wasm
        })
        .clone()
}

fn limits(call_timeout: Duration, call_fuel: u64) -> PluginLimits {
    PluginLimits {
        call_fuel,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 32,
        call_timeout,
    }
}

async fn plugin(call_timeout: Duration) -> runtime::Plugin {
    plugin_with_fuel(call_timeout, 1_000_000_000).await
}

async fn plugin_with_fuel(call_timeout: Duration, call_fuel: u64) -> runtime::Plugin {
    let temp = tempfile::tempdir().expect("temp plugin root");
    let plugin_dir = temp.path().join("tool-timeout-fixture");
    std::fs::create_dir_all(&plugin_dir).expect("create plugin directory");
    std::fs::copy(fixture(), plugin_dir.join("fixture.wasm")).expect("copy fixture component");
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        "name = \"tool-timeout-fixture\"\n\
         version = \"0.0.0\"\n\
         wasm_path = \"fixture.wasm\"\n\
         capabilities = [\"tool\"]\n\
         permissions = [\"http_client\"]\n",
    )
    .expect("write fixture manifest");

    let host = PluginHost::from_plugins_dir(temp.path()).expect("discover fixture");
    let details = host.tool_plugin_details();
    assert_eq!(details.len(), 1);
    let (manifest, path) = details[0];
    assert!(manifest.permissions.contains(&PluginPermission::HttpClient));
    let scope = PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Tool,
        "timeout",
        manifest.permissions.iter().copied(),
    )
    .expect("admit fixture scope");
    runtime::create_plugin(path, &scope, limits(call_timeout, call_fuel))
        .await
        .expect("instantiate timeout fixture")
}

/// Spawn a one-request server. The returned receiver resolves once the
/// request has been accepted and read, so a test can prove the guest's HTTP
/// wait actually reached the server rather than failing before the request
/// was issued.
async fn server(
    response: ServerResponse,
) -> (String, tokio::task::JoinHandle<()>, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture server");
    let address = listener.local_addr().expect("fixture server address");
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let task = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = listener.accept().await.expect("accept fixture request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        let _ = accepted_tx.send(());
        match response {
            ServerResponse::Complete => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await
                    .expect("write complete response");
            }
            ServerResponse::Drip => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\nx")
                    .await
                    .expect("write dripping response head");
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if stream.write_all(b"x").await.is_err() {
                        break;
                    }
                }
            }
            ServerResponse::NoResponse => {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    });
    (format!("http://{address}/body"), task, accepted_rx)
}

/// Assert the server-side acceptance handshake completed, proving the guest's
/// request was accepted and read before the observed outcome.
async fn assert_request_reached_server(accepted: oneshot::Receiver<()>, context: &str) {
    tokio::time::timeout(Duration::from_secs(1), accepted)
        .await
        .unwrap_or_else(|_| panic!("{context}: request never reached the fixture server"))
        .unwrap_or_else(|_| panic!("{context}: fixture server exited before acceptance"));
}

enum ServerResponse {
    Complete,
    Drip,
    NoResponse,
}

async fn execute(
    plugin: &mut runtime::Plugin,
    value: serde_json::Value,
) -> anyhow::Result<zeroclaw_api::tool::ToolResult> {
    runtime::call_execute(
        plugin,
        &serde_json::to_vec(&value).expect("serialize fixture input"),
        &HashMap::new(),
    )
    .await
}

#[tokio::test]
async fn dripping_response_is_stopped_by_host_deadline() {
    let (url, server, _accepted) = server(ServerResponse::Drip).await;
    let mut plugin = plugin(Duration::from_millis(250)).await;
    let started = Instant::now();
    let error = execute(&mut plugin, serde_json::json!({"mode": "http", "url": url}))
        .await
        .expect_err("drip must hit the host deadline");
    assert!(
        error.to_string().contains("wall-clock deadline"),
        "unexpected error: {error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let unavailable = execute(&mut plugin, serde_json::json!({"mode": "spin"}))
        .await
        .expect_err("timed-out tool instance must not resume its store");
    assert!(
        unavailable.to_string().contains("instance is unavailable"),
        "unexpected post-timeout error: {unavailable:#}"
    );
    server.abort();
}

#[tokio::test]
async fn normal_response_completes_before_host_deadline() {
    let (url, server, _accepted) = server(ServerResponse::Complete).await;
    let mut plugin = plugin(Duration::from_secs(2)).await;
    let result = execute(&mut plugin, serde_json::json!({"mode": "http", "url": url}))
        .await
        .expect("normal response completes");
    assert_eq!(&*result.output, "2 bytes");
    server.await.expect("server task");
}

#[tokio::test]
async fn guest_first_byte_timeout_can_shorten_but_not_extend_host_deadline() {
    const GUEST_DEADLINE: Duration = Duration::from_millis(100);
    let (short_guest_url, short_guest_server, guest_accepted) =
        server(ServerResponse::NoResponse).await;
    let mut guest_first = plugin(Duration::from_secs(2)).await;
    let started = Instant::now();
    let guest_error = execute(
        &mut guest_first,
        serde_json::json!({
            "mode": "raw-first-byte",
            "url": short_guest_url,
            "guest_timeout_ms": GUEST_DEADLINE.as_millis() as u64
        }),
    )
    .await
    .expect_err("guest timeout must fire");
    let elapsed = started.elapsed();
    // The fixture maps the wasi:http first-byte deadline to this stable
    // classification; an immediate pre-request or transport failure surfaces
    // under a different message and must fail this assertion.
    assert!(
        guest_error.to_string().contains("guest-first-byte-timeout"),
        "expected the guest's own first-byte deadline, got: {guest_error:#}"
    );
    assert!(
        !guest_error.to_string().contains("wall-clock deadline"),
        "guest timeout was incorrectly replaced by the host ceiling: {guest_error:#}"
    );
    assert!(
        elapsed >= GUEST_DEADLINE,
        "an error faster than the guest deadline is not a guest timeout: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "guest deadline must fire well before the 2 s host ceiling: {elapsed:?}"
    );
    assert_request_reached_server(guest_accepted, "guest-shorter deadline").await;
    short_guest_server.abort();

    let (host_url, host_server, host_accepted) = server(ServerResponse::NoResponse).await;
    let mut host_first = plugin(Duration::from_millis(250)).await;
    let host_error = execute(
        &mut host_first,
        serde_json::json!({
            "mode": "raw-first-byte",
            "url": host_url,
            "guest_timeout_ms": 2_000
        }),
    )
    .await
    .expect_err("host deadline must cap the longer guest timeout");
    assert!(
        host_error.to_string().contains("wall-clock deadline"),
        "unexpected error: {host_error:#}"
    );
    assert_request_reached_server(host_accepted, "host-shorter deadline").await;
    host_server.abort();
}

#[tokio::test]
async fn uninterrupted_guest_compute_cannot_starve_wall_clock_deadline() {
    let mut plugin = plugin_with_fuel(Duration::from_millis(250), u64::MAX).await;
    let started = Instant::now();
    let error = execute(&mut plugin, serde_json::json!({"mode": "spin"}))
        .await
        .expect_err("spinning guest must hit wall-clock deadline");
    assert!(
        error.to_string().contains("wall-clock deadline"),
        "unexpected error: {error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// Regression for the contracts of the `log-record` boundary.
///
/// Phase 0 (attribution): deferring delivery to the drain thread must not
/// change what an event means. A record emitted under a host attribution
/// span has to keep that scope through the queue: the terminal label and the
/// structured broadcast frame must carry the host attribution, not degrade
/// to system-level.
///
/// Phase 1 (liveness): the host wall-clock deadline is cooperative, so an
/// implementation that wrote to a stalled terminal consumer inline would pin
/// the export inside its first poll and let it overrun `call_timeout_ms`
/// indefinitely. With the bounded plugin-log queue, the export hands the
/// record off without blocking and the stall lands on the drain thread.
///
/// Phase 2 (memory): the queue bound must be a byte bound, not just a record
/// count. While the drain thread is stalled, a guest flooding large records
/// may retain at most the aggregate byte budget on the host; records above
/// the per-record cap are never queued at all. Both rejections are counted
/// and reported as drops once the drain resumes.
///
/// Phase 4 (loss observability): a rejection must be reported even when no
/// accepted record ever follows it; the drain thread's idle wake surfaces
/// the drop count on its own schedule.
#[tokio::test]
async fn blocked_host_log_writer_cannot_stall_export_or_exhaust_host_memory() {
    const ATTRIB_MARKER: &str = "zc-log-attribution-marker";
    const ATTRIB_ALIAS: &str = "e2e-plugin-agent";
    const STALL_MARKER: &str = "zc-blocked-log-writer-marker";
    const FLOOD_MARKER: &str = "zc-log-flood-marker";
    const OVERSIZED_MARKER: &str = "zc-log-oversized-marker";

    /// Test-local attributable whose role maps to the `agent_alias`
    /// attribution field, mirroring how a host agent scope labels events.
    struct E2eAgent;
    impl zeroclaw_api::attribution::Attributable for E2eAgent {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Agent
        }
        fn alias(&self) -> &str {
            ATTRIB_ALIAS
        }
    }
    /// Long enough that an inline (blocking) write visibly breaks the phase 1
    /// timing assertion and that phase 2 can flood a stalled queue; short
    /// enough that a regressed run still terminates.
    const WRITER_STALL: Duration = Duration::from_secs(10);
    /// Mirror of the crate-private bounds in `component_logging.rs`; keep in
    /// sync with `MAX_PLUGIN_LOG_RECORD_BYTES` / `PLUGIN_LOG_QUEUE_BYTE_BUDGET`.
    const RECORD_CAP_BYTES: usize = 64 * 1024;
    const BYTE_BUDGET: usize = 8 * 1024 * 1024;
    /// Under the per-record cap so only the aggregate budget limits the flood.
    const FLOOD_MSG_BYTES: usize = 60 * 1024;
    const FLOOD_CALLS: usize = 2;
    const RECORDS_PER_CALL: u64 = 100;

    let saw_stall = Arc::new(AtomicBool::new(false));
    let flood_delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let oversized_delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let overflow_reports = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attrib_line = Arc::new(std::sync::Mutex::new(None::<String>));
    let (saw, flood, oversized, overflow, attrib) = (
        Arc::clone(&saw_stall),
        Arc::clone(&flood_delivered),
        Arc::clone(&oversized_delivered),
        Arc::clone(&overflow_reports),
        Arc::clone(&attrib_line),
    );
    let installed = zeroclaw_log::try_install_line_sink_for_tests(move |line| {
        if line.contains(ATTRIB_MARKER) {
            *attrib
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(line.to_string());
        }
        if line.contains(FLOOD_MARKER) {
            flood.fetch_add(1, Ordering::SeqCst);
        }
        if line.contains(OVERSIZED_MARKER) {
            oversized.fetch_add(1, Ordering::SeqCst);
        }
        if line.contains("plugin log queue overflowed") {
            overflow.fetch_add(1, Ordering::SeqCst);
        }
        if line.contains(STALL_MARKER) && !saw.swap(true, Ordering::SeqCst) {
            std::thread::sleep(WRITER_STALL);
        }
    });
    assert!(
        installed,
        "this test must own the process-global subscriber; no other test in \
         this binary may install one"
    );

    let mut stall_plugin = plugin(Duration::from_millis(250)).await;
    let mut flood_plugin = plugin(Duration::from_secs(2)).await;

    // Phase 0: a record emitted under a host attribution span keeps that
    // scope through the queue, on both the structured and terminal outputs.
    let mut structured_rx = zeroclaw_log::subscribe_or_install();
    let span = zeroclaw_log::attribution_span!(&E2eAgent);
    let result = zeroclaw_log::Instrument::instrument(
        execute(
            &mut flood_plugin,
            serde_json::json!({"mode": "log", "message": ATTRIB_MARKER}),
        ),
        span,
    )
    .await
    .expect("attributed logging export completes");
    assert_eq!(&*result.output, "logged");
    let deadline = Instant::now() + Duration::from_secs(5);
    let attributed_line = loop {
        if let Some(line) = attrib_line
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            break line;
        }
        assert!(
            Instant::now() < deadline,
            "attributed plugin record was never delivered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(
        attributed_line.contains(&format!("[{ATTRIB_ALIAS}]")),
        "the terminal label must carry the host attribution, not [system]: \
         {attributed_line:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match structured_rx.try_recv() {
            Ok(frame) => {
                let frame = frame.to_string();
                if frame.contains(ATTRIB_MARKER) {
                    assert!(
                        frame.contains(ATTRIB_ALIAS),
                        "the structured frame must retain host attribution: {frame}"
                    );
                    break;
                }
            }
            Err(_) => {
                assert!(
                    Instant::now() < deadline,
                    "attributed record never reached the structured broadcast"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    drop(structured_rx);

    // Phase 1: the deadline holds through a stalled writer.
    let started = Instant::now();
    let result = execute(
        &mut stall_plugin,
        serde_json::json!({"mode": "log", "message": STALL_MARKER}),
    )
    .await
    .expect("a logging export must complete despite the stalled writer");
    assert_eq!(&*result.output, "logged");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a stalled log writer must not extend a guest export: {:?}",
        started.elapsed()
    );

    // Delivery is asynchronous but must still happen: the record has to reach
    // the subscriber (and begin its stall there) shortly after the call.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !saw_stall.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "plugin log record was never delivered to the subscriber"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Phase 2: with the drain thread stalled, flood roughly twice the byte
    // budget in under-cap records plus a few over-cap records. Every export
    // must still return promptly; retention is bounded by the budget.
    let flood_started = Instant::now();
    let flood_message = format!("{FLOOD_MARKER} {}", "x".repeat(FLOOD_MSG_BYTES));
    for _ in 0..FLOOD_CALLS {
        let result = execute(
            &mut flood_plugin,
            serde_json::json!({
                "mode": "log",
                "message": flood_message,
                "count": RECORDS_PER_CALL
            }),
        )
        .await
        .expect("flooding the stalled log queue must not fail the export");
        assert_eq!(&*result.output, "logged");
    }
    let oversized_message = format!("{OVERSIZED_MARKER} {}", "x".repeat(RECORD_CAP_BYTES + 1024));
    let result = execute(
        &mut flood_plugin,
        serde_json::json!({
            "mode": "log",
            "message": oversized_message,
            "count": 3
        }),
    )
    .await
    .expect("over-cap records must be absorbed, not fail the export");
    assert_eq!(&*result.output, "logged");
    assert!(
        flood_started.elapsed() < Duration::from_secs(8),
        "the flood must land while the writer is still stalled for the byte \
         bound below to be deterministic: {:?}",
        flood_started.elapsed()
    );

    // Phase 3: once the stall ends the drain reports the drops and delivers
    // only what the byte budget admitted.
    let deadline = Instant::now() + WRITER_STALL + Duration::from_secs(10);
    while overflow_reports.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the drain thread never reported the dropped records"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Let delivery settle: the flood count must be stable for a while.
    let mut settled = flood_delivered.load(Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let now_delivered = flood_delivered.load(Ordering::SeqCst);
        if now_delivered == settled {
            break;
        }
        settled = now_delivered;
        assert!(
            Instant::now() < deadline,
            "flood delivery never settled: {now_delivered} records so far"
        );
    }

    let delivered = flood_delivered.load(Ordering::SeqCst);
    let sent = FLOOD_CALLS * RECORDS_PER_CALL as usize;
    let max_within_budget = BYTE_BUDGET / FLOOD_MSG_BYTES + 2;
    assert!(
        delivered > 0,
        "records inside the byte budget must still be delivered"
    );
    assert!(
        delivered <= max_within_budget,
        "the stalled queue retained more than its byte budget: \
         {delivered} of {sent} records delivered (budget admits ~{max_within_budget})"
    );
    assert!(
        delivered < sent,
        "flooding past the byte budget must drop records, got all {sent}"
    );
    assert_eq!(
        oversized_delivered.load(Ordering::SeqCst),
        0,
        "a record above the per-record cap must never be queued or delivered"
    );

    // Phase 4: rejections must be reported even when no accepted record ever
    // follows them. Emit only over-cap records, keep the queue otherwise
    // idle, and require a fresh overflow report from the drain thread's
    // idle wake within a couple of its 2 s intervals.
    let reports_before_idle = overflow_reports.load(Ordering::SeqCst);
    let result = execute(
        &mut flood_plugin,
        serde_json::json!({
            "mode": "log",
            "message": oversized_message,
            "count": 3
        }),
    )
    .await
    .expect("rejected-only logging export still completes");
    assert_eq!(&*result.output, "logged");
    let deadline = Instant::now() + Duration::from_secs(10);
    while overflow_reports.load(Ordering::SeqCst) == reports_before_idle {
        assert!(
            Instant::now() < deadline,
            "drops with no following accepted record were never reported"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
