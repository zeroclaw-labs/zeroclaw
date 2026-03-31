//! Verify that build-python-plugins.sh compiles Python plugins to .wasm
//! binaries targeting wasm32-wasip1.
//!
//! Acceptance criteria for US-ZCL-34 and US-ZCL-35:
//! > build-python-plugins.sh compiles plugins to .wasm binaries targeting wasm32-wasip1

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// build-python-plugins.sh must exist and be executable.
#[test]
fn build_script_exists_and_is_executable() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("build-python-plugins.sh");
    assert!(
        script.exists(),
        "build-python-plugins.sh must exist at repo root"
    );

    let mode = fs::metadata(&script)
        .expect("should read script metadata")
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "build-python-plugins.sh must be executable (mode: {:#o})",
        mode
    );
}

/// The build script must produce python_echo_plugin.wasm in the artifacts directory.
#[test]
fn build_script_produces_wasm_artifact() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_echo_plugin.wasm");
    assert!(
        artifact.exists(),
        "python_echo_plugin.wasm must exist in tests/plugins/artifacts/"
    );

    let size = fs::metadata(&artifact)
        .expect("should read artifact metadata")
        .len();
    assert!(
        size > 1024,
        "wasm artifact must be non-trivial (got {} bytes)",
        size
    );
}

/// The artifact must be a valid WebAssembly binary (magic: \0asm, version 1).
#[test]
fn wasm_artifact_has_valid_magic_bytes() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_echo_plugin.wasm");
    let bytes = fs::read(&artifact).expect("should read wasm artifact");

    // WASM magic number: \0asm
    assert!(bytes.len() >= 8, "wasm file too small to contain header");
    assert_eq!(
        &bytes[0..4],
        b"\0asm",
        "file must start with WASM magic bytes (\\0asm)"
    );
    // WASM binary format version 1 (little-endian)
    assert_eq!(&bytes[4..8], &[1, 0, 0, 0], "WASM version must be 1 (MVP)");
}

/// The compiled .wasm must target wasm32-wasip1 — verified by checking that it
/// imports from the "wasi_snapshot_preview1" module namespace.
#[test]
fn wasm_artifact_targets_wasip1() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_echo_plugin.wasm");
    let bytes = fs::read(&artifact).expect("should read wasm artifact");

    // The import section will contain the string "wasi_snapshot_preview1" as a
    // module name. A simple byte-level search is sufficient — this string only
    // appears in WASI-targeting modules.
    let needle = b"wasi_snapshot_preview1";
    let found = bytes.windows(needle.len()).any(|window| window == needle);

    assert!(
        found,
        "wasm binary must import from wasi_snapshot_preview1 (targeting wasm32-wasip1)"
    );
}

/// build-python-plugins.sh must reference wasm32-wasip1 or extism-py (which
/// implies wasip1 targeting) in its compilation pipeline.
#[test]
fn build_script_uses_wasip1_toolchain() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("build-python-plugins.sh");
    let content = fs::read_to_string(&script).expect("should read build script");

    // The script must use extism-py which compiles to wasm32-wasip1
    assert!(
        content.contains("extism-py"),
        "build script must use extism-py for wasm32-wasip1 compilation"
    );

    // Must target the python-echo-plugin
    assert!(
        content.contains("python-echo-plugin"),
        "build script must compile the python-echo-plugin"
    );

    // Must output to artifacts directory
    assert!(
        content.contains("artifacts"),
        "build script must output to artifacts directory"
    );
}

// ---------------------------------------------------------------------------
// python-sdk-example-plugin build verification (US-ZCL-35)
// ---------------------------------------------------------------------------

/// The build script must include python-sdk-example-plugin in its PLUGINS list.
#[test]
fn build_script_includes_sdk_example_plugin() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("build-python-plugins.sh");
    let content = fs::read_to_string(&script).expect("should read build script");

    assert!(
        content.contains("python-sdk-example-plugin"),
        "build script must compile the python-sdk-example-plugin"
    );
    assert!(
        content.contains("python_sdk_example_plugin.wasm"),
        "build script must output python_sdk_example_plugin.wasm"
    );
}

/// The build script must produce python_sdk_example_plugin.wasm in the artifacts directory.
#[test]
fn build_script_produces_sdk_example_wasm_artifact() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_sdk_example_plugin.wasm");
    assert!(
        artifact.exists(),
        "python_sdk_example_plugin.wasm must exist in tests/plugins/artifacts/"
    );

    let size = fs::metadata(&artifact)
        .expect("should read artifact metadata")
        .len();
    assert!(
        size > 1024,
        "wasm artifact must be non-trivial (got {} bytes)",
        size
    );
}

/// The SDK example artifact must be a valid WebAssembly binary (magic: \0asm, version 1).
#[test]
fn sdk_example_wasm_artifact_has_valid_magic_bytes() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_sdk_example_plugin.wasm");
    let bytes = fs::read(&artifact).expect("should read wasm artifact");

    assert!(bytes.len() >= 8, "wasm file too small to contain header");
    assert_eq!(
        &bytes[0..4],
        b"\0asm",
        "file must start with WASM magic bytes (\\0asm)"
    );
    assert_eq!(&bytes[4..8], &[1, 0, 0, 0], "WASM version must be 1 (MVP)");
}

/// The SDK example .wasm must target wasm32-wasip1.
#[test]
fn sdk_example_wasm_artifact_targets_wasip1() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_sdk_example_plugin.wasm");
    let bytes = fs::read(&artifact).expect("should read wasm artifact");

    let needle = b"wasi_snapshot_preview1";
    let found = bytes.windows(needle.len()).any(|window| window == needle);

    assert!(
        found,
        "wasm binary must import from wasi_snapshot_preview1 (targeting wasm32-wasip1)"
    );
}
