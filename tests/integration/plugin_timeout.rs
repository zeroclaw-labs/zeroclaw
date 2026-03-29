//! Integration test: timeout enforcement kills tool_infinite_loop.
//!
//! Loads `bad_actor_plugin.wasm` with a short `timeout_ms`, calls
//! `tool_infinite_loop`, and asserts it returns an error within the
//! timeout window rather than hanging forever.

use std::path::Path;
use std::time::{Duration, Instant};

const BAD_ACTOR_WASM: &str = "tests/plugins/artifacts/bad_actor_plugin.wasm";

fn bad_actor_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(BAD_ACTOR_WASM)
}

#[test]
fn infinite_loop_is_killed_by_timeout() {
    let wasm_path = bad_actor_wasm_path();
    assert!(
        wasm_path.is_file(),
        "bad_actor_plugin.wasm not found at {}",
        wasm_path.display()
    );

    let timeout = Duration::from_secs(2);

    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(timeout);

    let mut plugin = extism::Plugin::new(&manifest, [], true)
        .expect("failed to instantiate bad-actor plugin");

    let start = Instant::now();
    let result = plugin.call::<&str, &str>("tool_infinite_loop", "{}");
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "tool_infinite_loop should fail with a timeout error, but succeeded"
    );

    let err_msg = result.unwrap_err().to_string().to_lowercase();
    assert!(
        err_msg.contains("timeout") || err_msg.contains("timed out"),
        "error should indicate a timeout, got: {}",
        err_msg
    );

    // The call should complete within a reasonable window around the timeout
    // (timeout + 2s grace to account for WASM teardown overhead).
    let max_allowed = timeout + Duration::from_secs(2);
    assert!(
        elapsed < max_allowed,
        "call took {:?}, expected it to finish within {:?}",
        elapsed,
        max_allowed
    );
}
