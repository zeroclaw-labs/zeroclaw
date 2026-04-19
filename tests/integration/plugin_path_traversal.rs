#![cfg(feature = "plugins-wasm")]
//! Security test: path traversal outside allowed_paths is denied.
//!
//! Verifies that the WASI sandbox blocks attempts to escape the mounted
//! directory via `../` path traversal, and that a plugin cannot directly
//! access sensitive paths like `/etc/passwd` when they are not in
//! `allowed_paths`.

use std::path::Path;

const FS_PLUGIN_WASM: &str = "tests/plugins/artifacts/fs_plugin.wasm";
const BAD_ACTOR_WASM: &str = "tests/plugins/artifacts/bad_actor_plugin.wasm";
const FIXTURES_DIR: &str = "tests/fixtures";

fn fs_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FS_PLUGIN_WASM)
}

fn bad_actor_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(BAD_ACTOR_WASM)
}

fn fixtures_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR)
}

/// Attempting to read `../` above the mounted directory should be denied.
#[test]
fn path_traversal_via_dotdot_is_denied() {
    let wasm = fs_wasm_path();
    assert!(
        wasm.is_file(),
        "fs_plugin.wasm not found at {}",
        wasm.display()
    );

    let fixtures = fixtures_path();
    assert!(
        fixtures.is_dir(),
        "fixtures dir not found at {}",
        fixtures.display()
    );

    // Mount only /input -> fixtures dir
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm)])
        .with_timeout(std::time::Duration::from_secs(5))
        .with_allowed_path(fixtures.to_string_lossy().into_owned(), "/input");

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate fs-plugin");

    // Try to escape via ../
    let traversal_input = r#"{"path": "/input/../../../etc/passwd"}"#;
    let result = plugin.call::<&str, &str>("tool_read_file", traversal_input);

    assert!(
        result.is_err(),
        "path traversal via ../ should be denied by WASI sandbox, but succeeded with: {:?}",
        result
    );
}

/// A plugin that directly reads /etc/passwd (not in allowed_paths) should be
/// denied by the sandbox.
#[test]
fn direct_etc_passwd_read_is_denied() {
    let wasm = bad_actor_wasm_path();
    assert!(
        wasm.is_file(),
        "bad_actor_plugin.wasm not found at {}",
        wasm.display()
    );

    let fixtures = fixtures_path();

    // Only mount /input — /etc is not mapped at all.
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm)])
        .with_timeout(std::time::Duration::from_secs(5))
        .with_allowed_path(fixtures.to_string_lossy().into_owned(), "/input");

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate bad-actor plugin");

    let result = plugin.call::<&str, &str>("tool_file_escape", "");

    assert!(
        result.is_err(),
        "direct /etc/passwd read should be denied by WASI sandbox, but succeeded with: {:?}",
        result
    );
}

/// Reading a path outside any mount (absolute path not under an allowed_path)
/// should fail.
#[test]
fn absolute_path_outside_mount_is_denied() {
    let wasm = fs_wasm_path();
    assert!(
        wasm.is_file(),
        "fs_plugin.wasm not found at {}",
        wasm.display()
    );

    let fixtures = fixtures_path();

    // Mount only /input
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm)])
        .with_timeout(std::time::Duration::from_secs(5))
        .with_allowed_path(fixtures.to_string_lossy().into_owned(), "/input");

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate fs-plugin");

    // Try to read from /etc/hostname — not mapped at all
    let input = r#"{"path": "/etc/hostname"}"#;
    let result = plugin.call::<&str, &str>("tool_read_file", input);

    assert!(
        result.is_err(),
        "reading /etc/hostname should be denied since it is not in allowed_paths, \
         but succeeded with: {:?}",
        result
    );
}
