//! Native-wins regression for the real channel-plugin startup path.
//!
//! The feature-gated test builds its standalone fixture on demand:
//!
//! ```text
//! cargo build --locked \
//!   --manifest-path crates/zeroclaw-plugins/tests/fixtures/channel-fixture/Cargo.toml \
//!   --target wasm32-wasip2
//! cargo test --features plugins-wasm-cranelift --test integration \
//!   shadowed_channel_plugin_never_runs_configure
//! ```

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::RwLock;
use serde_json::Value;
use tempfile::tempdir;
use zeroclaw_config::schema::Config;

const PLUGIN_NAME: &str = "shadow-probe";
const CONFIGURE_MARKER: &str = "channel-fixture configure export invoked";
const POLL_MARKER: &str = "channel-fixture poll-message export invoked";

fn authorize_fixture_sender(config: &mut Config) {
    config.peer_groups.insert(
        "channel-plugin-fixture".to_string(),
        zeroclaw_config::multi_agent::PeerGroupConfig {
            channel: PLUGIN_NAME.into(),
            external_peers: vec!["tester".into()],
            ..Default::default()
        },
    );
}

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/zeroclaw-plugins/tests/fixtures/channel-fixture");
            let target_dir = fixture_dir.join("target");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run cargo to build channel component fixture");
            assert!(
                status.success(),
                "channel component fixture must build; install the wasm32-wasip2 target"
            );
            let wasm = target_dir.join("wasm32-wasip2/debug/channel_fixture.wasm");
            assert!(wasm.is_file(), "channel component fixture was not produced");
            wasm
        })
        .clone()
}

fn install_fixture(wasm: &Path, plugins_dir: &Path) {
    let plugin_dir = plugins_dir.join(PLUGIN_NAME);
    fs::create_dir_all(&plugin_dir).expect("create throwaway plugin directory");
    let installed_wasm = plugin_dir.join("channel-fixture.wasm");
    fs::copy(wasm, &installed_wasm).expect("copy channel fixture into throwaway plugin directory");
    let digest = zeroclaw_plugins::signature::sha256_hex(
        &fs::read(&installed_wasm).expect("read copied channel fixture"),
    );
    fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "name = \"{PLUGIN_NAME}\"\n\
             version = \"0.1.0\"\n\
             wasm_path = \"channel-fixture.wasm\"\n\
             wasm_sha256 = \"{digest}\"\n\
             capabilities = [\"channel\"]\n"
        ),
    )
    .expect("write throwaway plugin manifest");
}

fn has_configure_marker(event: &Value) -> bool {
    event.get("message").and_then(Value::as_str) == Some(CONFIGURE_MARKER)
}

fn has_poll_marker(event: &Value) -> bool {
    event.get("message").and_then(Value::as_str) == Some(POLL_MARKER)
}

async fn receive_configure_marker(rx: &mut tokio::sync::broadcast::Receiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = rx.recv().await.expect("log broadcast remains open");
            if has_configure_marker(&event) {
                return event;
            }
        }
    })
    .await
    .expect("real fixture configure export emits its marker")
}

async fn assert_no_poll_after_cancellation(
    rx: &mut tokio::sync::broadcast::Receiver<Value>,
    duration: Duration,
) {
    let result = tokio::time::timeout(duration, async {
        loop {
            let event = rx.recv().await.expect("log broadcast remains open");
            if has_poll_marker(&event) {
                return event;
            }
        }
    })
    .await;
    assert!(
        result.is_err(),
        "real WASM poll-message kept running after supervisor cancellation: {result:?}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn shadowed_channel_plugin_never_runs_configure() {
    let wasm = fixture();

    let tmp = tempdir().expect("create throwaway plugin root");
    let plugins_dir = tmp.path().join("plugins");
    install_fixture(&wasm, &plugins_dir);

    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.plugins_dir = plugins_dir.to_string_lossy().into_owned();
    let config = Arc::new(RwLock::new(config));

    // Serialize against other tests that install or clear the process-wide log
    // hook. The fixture's imported logging call is our host-visible proof that
    // the real guest `configure` export ran.
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::clear_broadcast_hook();
    let _hook_cleanup = scopeguard::guard((), |_| zeroclaw_log::clear_broadcast_hook());
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();

    // Positive control: without a native collision, the actual component is
    // instantiated and its configure export emits the marker through the host
    // logging import.
    let built =
        zeroclaw_runtime::plugin_channels::build_channel_plugins(&config, &HashSet::new()).await;
    assert_eq!(built.len(), 1, "unshadowed fixture is instantiated");
    let marker = receive_configure_marker(&mut rx).await;
    assert_eq!(marker["message"], CONFIGURE_MARKER);
    while rx.try_recv().is_ok() {}

    // Native-wins path: the same real component is discovered, but its key is
    // already occupied. The builder must reject it before `from_wasm`, so no
    // guest startup export can emit the marker.
    let occupied = HashSet::from([PLUGIN_NAME.to_string()]);
    let built = zeroclaw_runtime::plugin_channels::build_channel_plugins(&config, &occupied).await;
    assert!(built.is_empty(), "shadowed plugin is not registered");

    while let Ok(event) = rx.try_recv() {
        assert!(
            !has_configure_marker(&event),
            "shadowed fixture invoked configure before native-wins filtering"
        );
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn supervised_wasm_listener_cancels_its_only_poll_generation() {
    let wasm = fixture();

    let tmp = tempdir().expect("create throwaway plugin root");
    let plugins_dir = tmp.path().join("plugins");
    install_fixture(&wasm, &plugins_dir);

    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.plugins_dir = plugins_dir.to_string_lossy().into_owned();
    authorize_fixture_sender(&mut config);
    let config = Arc::new(RwLock::new(config));

    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::clear_broadcast_hook();
    let _hook_cleanup = scopeguard::guard((), |_| zeroclaw_log::clear_broadcast_hook());
    zeroclaw_log::try_install_capture_subscriber();
    let mut logs = zeroclaw_log::subscribe_or_install();

    let mut built =
        zeroclaw_runtime::plugin_channels::build_channel_plugins(&config, &HashSet::new()).await;
    assert_eq!(built.len(), 1, "real fixture is instantiated once");
    receive_configure_marker(&mut logs).await;
    let (id, channel) = built.pop().expect("one built channel");

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let cancel = tokio_util::sync::CancellationToken::new();
    let component = format!(
        "channel:{}.{}",
        zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE,
        id
    );
    let handle = zeroclaw_channels::orchestrator::spawn_supervised_listener_with_health_interval(
        channel,
        Some(id),
        tx,
        1,
        1,
        Duration::from_millis(20),
        cancel.clone(),
    );

    let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("real supervised listener polls the fixture")
        .expect("listener forwards fixture message");
    assert_eq!(message.content, "{}");
    assert_eq!(message.channel, zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE);
    assert_eq!(message.channel_alias.as_deref(), Some(PLUGIN_NAME));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let snapshot = zeroclaw_runtime::health::snapshot_json();
    assert_eq!(
        snapshot["components"][&component]["restart_count"]
            .as_u64()
            .unwrap_or(0),
        0,
        "a live WASM polling loop must not look like a completed listener generation"
    );
    assert!(
        !handle.is_finished(),
        "supervisor remains attached to listen"
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("supervisor cancellation completes")
        .expect("supervisor task exits cleanly");

    while logs.try_recv().is_ok() {}
    assert_no_poll_after_cancellation(&mut logs, Duration::from_millis(650)).await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn disabled_plugin_owner_blocks_guest_startup_before_configure() {
    let wasm = fixture();

    let tmp = tempdir().expect("create throwaway plugin root");
    let plugins_dir = tmp.path().join("plugins");
    install_fixture(&wasm, &plugins_dir);

    let plugin_ref = zeroclaw_api::channel::plugin_channel_ref(PLUGIN_NAME);
    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.plugins_dir = plugins_dir.to_string_lossy().into_owned();
    config
        .plugins
        .entries
        .push(zeroclaw_config::schema::PluginEntryConfig {
            name: PLUGIN_NAME.to_string(),
            config: Default::default(),
        });
    config.risk_profiles.insert(
        "default".to_string(),
        zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    config.providers.models.anthropic.insert(
        "default".to_string(),
        zeroclaw_config::schema::AnthropicModelProviderConfig::default(),
    );
    config.channels.telegram.insert(
        "main".to_string(),
        zeroclaw_config::schema::TelegramConfig::default(),
    );
    config.agents.clear();
    config.agents.insert(
        "native-owner".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec!["telegram.main".into()],
            model_provider: zeroclaw_config::providers::ModelProviderRef::new("anthropic.default"),
            risk_profile: "default".into(),
            ..Default::default()
        },
    );
    config.agents.insert(
        "plugin-owner".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            enabled: true,
            channels: vec![plugin_ref.clone().into()],
            model_provider: zeroclaw_config::providers::ModelProviderRef::new("anthropic.default"),
            risk_profile: "default".into(),
            ..Default::default()
        },
    );
    config
        .validate()
        .expect("explicit native and plugin ownership is a valid daemon config");
    let config = Arc::new(RwLock::new(config));

    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::clear_broadcast_hook();
    let _hook_cleanup = scopeguard::guard((), |_| zeroclaw_log::clear_broadcast_hook());
    zeroclaw_log::try_install_capture_subscriber();
    let mut logs = zeroclaw_log::subscribe_or_install();

    let built =
        zeroclaw_runtime::plugin_channels::build_channel_plugins(&config, &HashSet::new()).await;
    assert_eq!(built.len(), 1, "enabled plugin owner admits the guest");
    receive_configure_marker(&mut logs).await;
    while logs.try_recv().is_ok() {}

    config
        .write()
        .agents
        .get_mut("plugin-owner")
        .expect("plugin owner")
        .enabled = false;
    config
        .read()
        .validate()
        .expect("disabling the plugin owner preserves a valid daemon config");
    let built =
        zeroclaw_runtime::plugin_channels::build_channel_plugins(&config, &HashSet::new()).await;
    assert!(built.is_empty(), "disabled plugin owner blocks startup");
    while let Ok(event) = logs.try_recv() {
        assert!(
            !has_configure_marker(&event),
            "disabled owner's plugin executed configure before ownership gating"
        );
    }
}
