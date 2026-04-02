//! Integration test: filesystem plugin reads from mounted input path and writes
//! to mounted output path.
//!
//! Loads `fs_plugin.wasm` with two `allowed_paths` mappings — one for reading
//! (`/input` -> host fixtures dir) and one for writing (`/output` -> a temp dir).
//! Calls `tool_read_file` to read from the input mount, then `tool_write_file`
//! to write to the output mount, verifying the round-trip.

use std::path::Path;

const FS_PLUGIN_WASM: &str = "tests/plugins/artifacts/fs_plugin.wasm";
const FIXTURES_DIR: &str = "tests/fixtures";

fn wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FS_PLUGIN_WASM)
}

fn fixtures_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR)
}

#[test]
fn fs_plugin_reads_from_input_and_writes_to_output() {
    let wasm = wasm_path();
    assert!(
        wasm.is_file(),
        "fs_plugin.wasm not found at {}",
        wasm.display()
    );

    let fixtures = fixtures_path();
    assert!(
        fixtures.is_dir(),
        "fixtures directory not found at {}",
        fixtures.display()
    );

    // Create a temp directory for the output mount
    let output_dir = tempfile::tempdir().expect("failed to create temp dir");

    // Map /input -> host fixtures dir, /output -> temp dir
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm)])
        .with_timeout(std::time::Duration::from_secs(5))
        .with_allowed_path(fixtures.to_string_lossy().into_owned(), "/input")
        .with_allowed_path(output_dir.path().to_string_lossy().into_owned(), "/output");

    let mut plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate fs-plugin");

    // --- Read from input mount ---
    let read_input = r#"{"path": "/input/sample.txt"}"#;
    let read_output = plugin
        .call::<&str, &str>("tool_read_file", read_input)
        .expect("tool_read_file call failed");

    let read_parsed: serde_json::Value =
        serde_json::from_str(read_output).expect("read output is not valid JSON");

    let contents = read_parsed["contents"]
        .as_str()
        .expect("contents should be a string");

    assert_eq!(
        contents.trim(),
        "Hello from ZeroClaw test fixture!",
        "expected fixture file contents, got: {contents}"
    );

    // --- Write to output mount ---
    let write_payload = serde_json::json!({
        "path": "/output/result.txt",
        "contents": contents.trim()
    });
    let write_input = serde_json::to_string(&write_payload).unwrap();

    let write_output = plugin
        .call::<&str, &str>("tool_write_file", &write_input)
        .expect("tool_write_file call failed");

    let write_parsed: serde_json::Value =
        serde_json::from_str(write_output).expect("write output is not valid JSON");

    let bytes_written = write_parsed["bytes_written"]
        .as_u64()
        .expect("bytes_written should be a number");
    assert!(bytes_written > 0, "expected bytes_written > 0");

    // --- Verify the file was actually written on the host ---
    let host_output_file = output_dir.path().join("result.txt");
    assert!(
        host_output_file.is_file(),
        "output file not found at {}",
        host_output_file.display()
    );

    let host_contents =
        std::fs::read_to_string(&host_output_file).expect("failed to read output file on host");
    assert_eq!(
        host_contents, "Hello from ZeroClaw test fixture!",
        "round-trip mismatch: host file contents differ from what was read"
    );
}
