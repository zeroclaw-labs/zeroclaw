//! Native-wins regression for the real channel-plugin startup path.
//!
//! Build the standalone fixture before running this feature-gated test:
//!
//! ```text
//! cargo build \
//!   --manifest-path crates/zeroclaw-plugins/tests/fixtures/channel-fixture/Cargo.toml \
//!   --target wasm32-wasip2 --release
//! cp crates/zeroclaw-plugins/tests/fixtures/channel-fixture/target/wasm32-wasip2/release/channel_fixture.wasm \
//!   crates/zeroclaw-plugins/tests/fixtures/channel-fixture.wasm
//! cargo test --features plugins-wasm-cranelift --test integration \
//!   shadowed_channel_plugin_never_runs_configure
//! ```

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tempfile::tempdir;
use zeroclaw_config::schema::Config;

const PLUGIN_NAME: &str = "shadow-probe";
const CONFIGURE_MARKER: &str = "channel-fixture configure export invoked";

fn fixture() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/zeroclaw-plugins/tests/fixtures/channel-fixture.wasm");
    path.exists().then_some(path)
}

fn install_fixture(wasm: &Path, plugins_dir: &Path) {
    let plugin_dir = plugins_dir.join(PLUGIN_NAME);
    fs::create_dir_all(&plugin_dir).expect("create throwaway plugin directory");
    fs::copy(wasm, plugin_dir.join("channel-fixture.wasm"))
        .expect("copy channel fixture into throwaway plugin directory");
    fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "name = \"{PLUGIN_NAME}\"\n\
             version = \"0.1.0\"\n\
             wasm_path = \"channel-fixture.wasm\"\n\
             capabilities = [\"channel\"]\n"
        ),
    )
    .expect("write throwaway plugin manifest");
}

fn has_configure_marker(event: &Value) -> bool {
    event.get("message").and_then(Value::as_str) == Some(CONFIGURE_MARKER)
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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn shadowed_channel_plugin_never_runs_configure() {
    let Some(wasm) = fixture() else {
        eprintln!(
            "channel-fixture.wasm absent; skipping. Build it with the commands in this module's docs."
        );
        return;
    };

    let tmp = tempdir().expect("create throwaway plugin root");
    let plugins_dir = tmp.path().join("plugins");
    install_fixture(&wasm, &plugins_dir);

    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.plugins_dir = plugins_dir.to_string_lossy().into_owned();

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
