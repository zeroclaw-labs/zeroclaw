//! Cross-crate proof that a channel plugin's outbound `wasi:http` reaches the
//! network only when the operator grants its destination.
//!
//! This is the channel analogue of `egress_plugin_e2e.rs` (which proves the
//! same boundary for tool plugins), run through the whole activation path an
//! operator exercises: a `[channels.plugin.<alias>]` declaration plus an
//! installed package plus a `[[plugins.entries]]` egress grant, in; a live
//! `Channel` backed by a compiled component that issued one real GET, out.
//!
//! Nothing is stubbed. The allow path opens a TCP connection to a listener in
//! this process; the deny path is the same host code the shipped daemon runs.
//! The fixture always constructs — only whether the packet leaves the sandbox,
//! and what the guest observed, changes with the grant. That is what proves the
//! host-owned egress policy gates *reach*, not construction.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use tempfile::TempDir;
use zeroclaw_config::providers::{ChannelRef, ModelProviderRef};
use zeroclaw_config::schema::{
    AliasedAgentConfig, AnthropicModelProviderConfig, Config, PluginChannelConfig,
    PluginEntryConfig, RiskProfileConfig,
};
use zeroclaw_plugins::PluginCapability;
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;

const MANIFEST: &str =
    "crates/zeroclaw-plugins/tests/fixtures/channel-egress-fixture/plugin-manifest.toml";

// ── fixture provisioning ──────────────────────────────────────────

/// Build the channel egress component once per test binary.
///
/// The fixture is a workspace member built into its own target directory so the
/// nested Cargo invocation cannot contend with this test process's build lock.
/// A missing wasm target is a failure, never a skip — a security boundary that
/// silently stops being tested is worse than one that is loudly broken.
fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/zeroclaw-plugins/tests/fixtures/channel-egress-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("channel-egress-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-channel-egress-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the channel egress component fixture");
            assert!(
                status.success(),
                "channel egress fixture must build; install the wasm32-wasip2 target"
            );

            let wasm =
                target_dir.join("wasm32-wasip2/debug/zeroclaw_channel_egress_plugin_fixture.wasm");
            assert!(
                wasm.is_file(),
                "channel egress fixture WASM was not produced"
            );
            wasm
        })
        .clone()
}

/// Install the fixture as a real plugin package: the canonical manifest copied
/// verbatim, next to the component it names.
fn install_fixture_package() -> TempDir {
    let plugins = TempDir::new().expect("create plugin package root");
    let package = plugins.path().join("channel-egress-fixture");
    std::fs::create_dir_all(&package).expect("create plugin package");
    std::fs::copy(fixture(), package.join("channel-egress-fixture.wasm"))
        .expect("copy channel egress component fixture");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST),
        package.join("manifest.toml"),
    )
    .expect("install the canonical fixture manifest");
    plugins
}

// ── operator config ───────────────────────────────────────────────

/// The instance-key `[[plugins.entries]]` row for the configured channel.
///
/// The channel's admitted scope is `(package, Channel, alias)`; the egress
/// resolver and the config loader both key that instance by its
/// `config_entry_key()`, so this row's grant is exactly the one a channel store
/// resolves at request time. Building the key here from the same manifest the
/// loader admits is what proves the two sides address the same row.
fn entry_row(
    plugins: &TempDir,
    alias: &str,
    url: &str,
    egress_hosts: &[&str],
    egress_allow_private: &[&str],
) -> PluginEntryConfig {
    let host = PluginHost::from_plugins_dir(plugins.path()).expect("admit fixture package");
    let manifest = host
        .manifest("channel-egress-fixture")
        .expect("fixture manifest is admitted");
    let scope = PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Channel,
        alias,
        manifest.permissions.iter().copied(),
    )
    .expect("admit configured logical channel");

    PluginEntryConfig {
        name: scope
            .id()
            .config_entry_key()
            .expect("derive canonical fixture config key"),
        config: HashMap::from([("url".to_string(), url.to_string())]),
        egress_hosts: egress_hosts.iter().map(|h| (*h).to_string()).collect(),
        egress_allow_private: egress_allow_private
            .iter()
            .map(|h| (*h).to_string())
            .collect(),
    }
}

/// A realistic operator config: one enabled agent routing to one configured
/// channel plugin, granted the destinations in its instance-key row. Passes
/// `Config::validate`, so this is the config an operator would actually write.
fn activation_config(plugins: &TempDir, alias: &str, entry: PluginEntryConfig) -> Config {
    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.auto_discover = false;
    config.plugins.max_active_instances = 1;
    config.plugins.plugins_dir = plugins.path().display().to_string();
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());
    config.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig::default(),
    );
    config.channels.plugin.insert(
        alias.to_string(),
        PluginChannelConfig {
            package: "channel-egress-fixture".to_string(),
            enabled: true,
        },
    );
    config.agents.insert(
        "operator".to_string(),
        AliasedAgentConfig {
            channels: vec![ChannelRef::new(format!("plugin.{alias}"))],
            model_provider: ModelProviderRef::new("anthropic.default"),
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );
    config.plugins.entries.push(entry);
    config
}

// ── local test server ─────────────────────────────────────────────

/// A minimal HTTP/1.1 responder. Deliberately raw `std::net` rather than a
/// server framework: this file proves what the host's client does, so the
/// server adds no machinery of its own. `hits` counts every accepted connection
/// — the load-bearing signal for "the packet actually left the sandbox".
struct TestServer {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback test server");
        let port = listener.local_addr().expect("local addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();

        std::thread::spawn(move || {
            const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(OK.as_bytes());
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

/// Construct the single configured channel and return its guest-observed egress
/// outcome (recorded in `configure`, surfaced through the cached `self-handle`).
async fn construct_and_probe(config: Config) -> (usize, Option<String>) {
    // fixture()/build happen through activation; validate first so a broken
    // operator config fails as config, not as a mystery empty channel list.
    config
        .validate()
        .expect("the activation declaration is valid operator config");
    let channels =
        zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await;
    assert_eq!(
        channels.len(),
        1,
        "the configured channel must construct regardless of the egress verdict"
    );
    let outcome = channels[0].self_handle();
    (channels.len(), outcome)
}

// ── the proof ─────────────────────────────────────────────────────

/// Granted: the operator lists the destination (and opens its private address
/// class), so the guest's GET reaches the listener. The hit count is the
/// load-bearing half — it proves a real packet arrived, not merely that the
/// guest saw no error.
#[tokio::test]
async fn a_granted_destination_reaches_the_server() {
    let plugins = install_fixture_package();
    let server = TestServer::start();
    let entry = entry_row(
        &plugins,
        "operations",
        &server.url("/hook"),
        &["127.0.0.1"],
        &["127.0.0.1"],
    );
    let config = activation_config(&plugins, "operations", entry);

    let (_, outcome) = construct_and_probe(config).await;

    assert_eq!(
        outcome.as_deref(),
        Some("egress:status=200"),
        "a granted channel must reach the server; got: {outcome:?}"
    );
    assert_eq!(
        server.hits(),
        1,
        "the granted request must reach the socket exactly once; got: {outcome:?}"
    );
}

/// Ungranted: `http_client` grants the `wasi:http` surface, but with no
/// `egress_hosts` the channel reaches nothing. The denial surfaces to the guest
/// (its outcome names the policy) and the listener is never touched — refused
/// before any packet left, which is the property that shuts the self-grant path.
#[tokio::test]
async fn an_ungranted_destination_is_denied_before_the_network() {
    let plugins = install_fixture_package();
    let server = TestServer::start();
    let entry = entry_row(&plugins, "operations", &server.url("/hook"), &[], &[]);
    let config = activation_config(&plugins, "operations", entry);

    let (_, outcome) = construct_and_probe(config).await;

    let outcome = outcome.expect("the guest must report an egress outcome");
    assert!(
        outcome.contains("error=") && !outcome.contains("status=200"),
        "an ungranted channel must be denied; got: {outcome}"
    );
    assert!(
        outcome.contains("egress policy"),
        "the guest's denial must name the policy; got: {outcome}"
    );
    assert_eq!(
        server.hits(),
        0,
        "a denied request must never reach the socket; got: {outcome}"
    );
}

/// Gated by host, not on/off: the operator grants a *different* host than the
/// server runs on, so the same store — surface granted, a policy present —
/// still cannot reach the listener. This distinguishes "policy says no" from
/// "no policy", and proves the allowlist match is by destination.
#[tokio::test]
async fn a_grant_for_a_different_host_still_denies_the_server() {
    let plugins = install_fixture_package();
    let server = TestServer::start();
    let entry = entry_row(
        &plugins,
        "operations",
        &server.url("/hook"),
        &["api.example.com"],
        &["api.example.com"],
    );
    let config = activation_config(&plugins, "operations", entry);

    let (_, outcome) = construct_and_probe(config).await;

    let outcome = outcome.expect("the guest must report an egress outcome");
    assert!(
        outcome.contains("error=") && !outcome.contains("status=200"),
        "a grant for a different host must not reach this server; got: {outcome}"
    );
    assert_eq!(
        server.hits(),
        0,
        "the policy must gate by host: a different grant reaches nothing; got: {outcome}"
    );
}
