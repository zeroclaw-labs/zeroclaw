//! End-to-end tests for `apply_comments()` — the async TOML comment-writing
//! helper that decorates leaf keys by dotted path. Each test writes a config
//! file to a temp directory, calls `apply_comments`, and asserts the on-disk
//! result matches expectations.

use std::path::{Path, PathBuf};

use tempfile::tempdir;
use zeroclaw_config::comment_writer::apply_comments;

// ─────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────

/// Write `content` to a temp file named `config.toml` and return the temp
/// directory handle (must stay alive for the duration of the test) plus the
/// file path.
async fn write_temp_config(content: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    tokio::fs::write(&path, content).await.unwrap();
    (dir, path)
}

/// Read the content of a config file back from disk.
async fn read_config(path: &Path) -> String {
    tokio::fs::read_to_string(path).await.unwrap()
}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn apply_comments_inserts_comment_above_target_key() {
    let (dir, path) = write_temp_config("host = \"localhost\"\nport = 8080\n").await;

    apply_comments(
        &path,
        &[(String::from("host"), String::from("the server hostname"))],
    )
    .await
    .unwrap();

    let result = read_config(&path).await;
    let lines: Vec<&str> = result.lines().collect();
    let host_line_idx = lines
        .iter()
        .position(|l| l.starts_with("host ="))
        .expect("`host = ...` line must be present");
    assert!(
        host_line_idx > 0,
        "comment line must exist above the host key"
    );
    assert!(
        lines[host_line_idx - 1].contains("# the server hostname"),
        "expected comment `# the server hostname` above `host = ...`; \
         got line above: {:?}",
        lines[host_line_idx - 1]
    );
    let _: toml::Value = toml::from_str(result.trim()).expect("resulting TOML must parse");
    drop(dir);
}

#[tokio::test]
async fn apply_comments_updates_existing_comment_on_same_key() {
    let (dir, path) = write_temp_config("# old comment\nhost = \"localhost\"\nport = 8080\n").await;

    apply_comments(
        &path,
        &[(String::from("host"), String::from("updated description"))],
    )
    .await
    .unwrap();

    let result = read_config(&path).await;
    assert!(
        !result.contains("old comment"),
        "old comment must be gone; result:\n{result}"
    );
    assert!(
        result.contains("# updated description"),
        "new comment must appear; result:\n{result}"
    );
    let parsed: toml::Value = toml::from_str(result.trim()).expect("resulting TOML must parse");
    assert_eq!(
        parsed.get("host").and_then(toml::Value::as_str),
        Some("localhost")
    );
    drop(dir);
}

#[tokio::test]
async fn apply_comments_multiple_annotations_in_one_call() {
    let (dir, path) = write_temp_config("host = \"localhost\"\nport = 8080\n").await;

    apply_comments(
        &path,
        &[
            (String::from("host"), String::from("server address")),
            (String::from("port"), String::from("listen port")),
        ],
    )
    .await
    .unwrap();

    let result = read_config(&path).await;
    let lines: Vec<&str> = result.lines().collect();

    let host_idx = lines
        .iter()
        .position(|l| l.starts_with("host ="))
        .expect("host key present");
    assert!(
        host_idx > 0 && lines[host_idx - 1].contains("# server address"),
        "comment above host key must be `# server address`; got {:?}",
        lines[host_idx - 1]
    );

    let port_idx = lines
        .iter()
        .position(|l| l.starts_with("port ="))
        .expect("port key present");
    assert!(
        port_idx > 0 && lines[port_idx - 1].contains("# listen port"),
        "comment above port key must be `# listen port`; got {:?}",
        lines[port_idx - 1]
    );

    let parsed: toml::Value = toml::from_str(result.trim()).expect("resulting TOML must parse");
    assert_eq!(
        parsed.get("host").and_then(toml::Value::as_str),
        Some("localhost")
    );
    assert_eq!(
        parsed.get("port").and_then(toml::Value::as_integer),
        Some(8080)
    );
    drop(dir);
}

#[tokio::test]
async fn apply_comments_deep_dotted_traversal() {
    // Use standard (non-dotted) table headers so that both toml_edit and
    // toml::Value can parse the round-tripped output.
    let (dir, path) = write_temp_config(
        "[providers]\n\n\
         [providers.models]\n\n\
         [providers.models.anthropic]\n\n\
         [providers.models.anthropic.default]\n\
         api_key = \"sk-test\"\n\
         model = \"claude-sonnet-4\"\n",
    )
    .await;

    apply_comments(
        &path,
        &[(
            String::from("providers.models.anthropic.default.api_key"),
            String::from("Anthropic API key"),
        )],
    )
    .await
    .unwrap();

    let result = read_config(&path).await;
    let lines: Vec<&str> = result.lines().collect();

    let api_key_idx = lines
        .iter()
        .position(|l| l.starts_with("api_key ="))
        .expect("api_key key must be present");
    assert!(
        api_key_idx > 0 && lines[api_key_idx - 1].contains("# Anthropic API key"),
        "comment must appear above `api_key = ...`; line above: {:?}",
        lines[api_key_idx - 1]
    );

    let parsed: toml::Value = toml::from_str(result.trim()).expect("resulting TOML must parse");
    let api_key_val = parsed
        .get("providers")
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("models"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("anthropic"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("default"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("api_key"))
        .and_then(toml::Value::as_str);
    assert_eq!(api_key_val, Some("sk-test"));
    drop(dir);
}

#[tokio::test]
async fn apply_comments_bad_intermediate_path_is_noop() {
    // `host` is a string value, not a table. The dotted path `host.port`
    // tries to traverse through a non-table intermediate, which should be
    // silently skipped.
    let (dir, path) = write_temp_config("host = \"localhost\"\nport = 8080\n").await;

    apply_comments(
        &path,
        &[(String::from("host.port"), String::from("should not appear"))],
    )
    .await
    .unwrap();

    let result = read_config(&path).await;
    assert!(
        !result.contains("should not appear"),
        "comment must not appear for a bad intermediate path; result:\n{result}"
    );
    let parsed: toml::Value = toml::from_str(result.trim()).expect("resulting TOML must parse");
    assert_eq!(
        parsed.get("host").and_then(toml::Value::as_str),
        Some("localhost")
    );
    assert_eq!(
        parsed.get("port").and_then(toml::Value::as_integer),
        Some(8080)
    );
    drop(dir);
}

#[tokio::test]
async fn apply_comments_nonexistent_key_is_noop() {
    let (dir, path) = write_temp_config("host = \"localhost\"\nport = 8080\n").await;

    apply_comments(
        &path,
        &[(
            String::from("does_not_exist"),
            String::from("ghost comment"),
        )],
    )
    .await
    .unwrap();

    let result = read_config(&path).await;
    assert!(
        !result.contains("ghost comment"),
        "comment must not appear for a nonexistent key; result:\n{result}"
    );
    let parsed: toml::Value = toml::from_str(result.trim()).expect("resulting TOML must parse");
    assert_eq!(
        parsed.get("host").and_then(toml::Value::as_str),
        Some("localhost")
    );
    assert_eq!(
        parsed.get("port").and_then(toml::Value::as_integer),
        Some(8080)
    );
    drop(dir);
}

#[tokio::test]
async fn apply_comments_unparseable_file_is_noop() {
    let (dir, path) = write_temp_config("this is {{{ not valid TOML").await;
    let original = read_config(&path).await;

    let outcome =
        apply_comments(&path, &[(String::from("host"), String::from("irrelevant"))]).await;

    assert!(
        outcome.is_ok(),
        "apply_comments must return Ok(()) for an unparseable file"
    );

    let result = read_config(&path).await;
    assert_eq!(
        result, original,
        "unparseable file content must remain exactly as-is"
    );
    drop(dir);
}

#[tokio::test]
async fn apply_comments_preserves_unrelated_content() {
    let (dir, path) = write_temp_config(
        "# existing comment on host\n\
         host = \"localhost\"\n\
         port = 8080\n\n\
         [database]\n\
         # existing comment on url\n\
         url = \"postgres://localhost/db\"\n\
         max_connections = 10\n",
    )
    .await;

    apply_comments(
        &path,
        &[(
            String::from("database.url"),
            String::from("database connection string"),
        )],
    )
    .await
    .unwrap();

    let result = read_config(&path).await;

    assert!(
        !result.contains("existing comment on url"),
        "old comment on database.url must be gone"
    );
    assert!(
        result.contains("# database connection string"),
        "new comment on database.url must appear"
    );
    assert!(
        result.contains("# existing comment on host"),
        "unrelated comment on host must be preserved"
    );
    assert!(
        result.contains("\n\n[database]"),
        "blank line before [database] must be preserved"
    );

    let parsed: toml::Value = toml::from_str(result.trim()).expect("resulting TOML must parse");
    assert_eq!(
        parsed.get("host").and_then(toml::Value::as_str),
        Some("localhost")
    );
    assert_eq!(
        parsed.get("port").and_then(toml::Value::as_integer),
        Some(8080)
    );
    assert_eq!(
        parsed
            .get("database")
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("url"))
            .and_then(toml::Value::as_str),
        Some("postgres://localhost/db")
    );
    assert_eq!(
        parsed
            .get("database")
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("max_connections"))
            .and_then(toml::Value::as_integer),
        Some(10)
    );
    drop(dir);
}
