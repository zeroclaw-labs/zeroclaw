//! Integration tests for the Codex CLI provider subprocess behavior.
//!
//! Each test writes a small shell script that emulates the `codex` binary,
//! then invokes `CodexCliProvider::new_for_test()` with the script path
//! and verifies the output of `chat_with_system()`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

use zeroclaw::providers::codex_cli::CodexCliProvider;
use zeroclaw::providers::traits::Provider;

/// Write a shell script to a temp directory and return the owning `TempDir`
/// plus the absolute path of the script as a `String`.
///
/// The file is explicitly flushed and synced to avoid races when the tokio
/// runtime spawns the subprocess on another thread.
fn write_fake_codex(name: &str, body: &str) -> (TempDir, String) {
    let dir = TempDir::new().expect("failed to create temp dir");
    let path = dir.path().join(name);
    let mut file = fs::File::create(&path).expect("failed to create script file");
    file.write_all(body.as_bytes())
        .expect("failed to write script");
    file.sync_all().expect("failed to sync script to disk");
    drop(file);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("failed to chmod script");
    let path_str = path.to_str().expect("non-UTF-8 temp path").to_owned();
    (dir, path_str)
}

#[tokio::test]
async fn codex_cli_provider_returns_last_agent_message() {
    let (_dir, script) = write_fake_codex(
        "fake_codex",
        r#"#!/bin/bash
echo '{"type":"item.created","item":{"type":"message","role":"assistant"}}'
echo '{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello from fake codex"}]}}'
"#,
    );

    let provider = CodexCliProvider::new_for_test(&script);
    let text = provider
        .chat_with_system(None, "hi", "gpt-5.3-codex-spark", 0.0)
        .await
        .unwrap();
    assert_eq!(text, "hello from fake codex");
}

#[tokio::test]
async fn codex_cli_provider_surfaces_stderr_on_nonzero_exit() {
    let (_dir, script) = write_fake_codex(
        "fake_codex_fail",
        r#"#!/bin/bash
echo "model not supported" >&2
exit 1
"#,
    );

    let provider = CodexCliProvider::new_for_test(&script);
    let err = provider
        .chat_with_system(None, "hi", "gpt-5.3-codex-spark", 0.0)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("not supported") || err.to_string().contains("model"),
        "Expected error to mention 'not supported' or 'model', got: {err}"
    );
}

#[tokio::test]
async fn codex_cli_provider_handles_empty_output() {
    let (_dir, script) = write_fake_codex("fake_codex_empty", "#!/bin/bash\n");

    let provider = CodexCliProvider::new_for_test(&script);
    let err = provider
        .chat_with_system(None, "hi", "gpt-5.3-codex-spark", 0.0)
        .await;
    assert!(err.is_err(), "Expected error for empty output, got Ok");
}

#[tokio::test]
async fn codex_cli_provider_with_system_prompt() {
    let (_dir, script) = write_fake_codex(
        "fake_codex_sys",
        r#"#!/bin/bash
echo '{"type":"item.completed","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"got system prompt"}]}}'
"#,
    );

    let provider = CodexCliProvider::new_for_test(&script);
    let text = provider
        .chat_with_system(Some("You are helpful"), "hello", "gpt-5.3-codex", 0.0)
        .await
        .unwrap();
    assert_eq!(text, "got system prompt");
}
