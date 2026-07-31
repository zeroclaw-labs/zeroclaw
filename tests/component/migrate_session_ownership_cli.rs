//! CLI boundary tests for `zeroclaw migrate session-ownership`.
//!
//! These run the real binary as a subprocess (mirroring
//! `skills_bundle_cli.rs`) against a seeded session backend, covering the
//! F5 preflight/fail-closed contract on both the sqlite and jsonl backends:
//! a claim must be rejected (non-zero exit, no new rows) when the agent alias
//! is not configured or the session does not exist, and must succeed without
//! creating ghost rows when both preconditions hold.

use std::process::{Command, Output};

use zeroclaw_infra::session_backend::ClaimOutcome;

fn run_zeroclaw(config_dir: &std::path::Path, args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    Command::new(bin)
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("LANG", "C")
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run zeroclaw")
}

/// Write a minimal config that enables a single agent `default` and selects
/// the given session backend. Returns the data dir where sessions live.
fn write_config(config_dir: &std::path::Path, backend: &str) -> std::path::PathBuf {
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"schema_version = 3

[channels]
session_backend = "{backend}"

[agents.default]
enabled = true
"#
        ),
    )
    .expect("write config");
    // data_dir defaults to <config_dir>/data.
    config_dir.join("data")
}

/// Seed a non-empty, unowned session in the given backend.
fn seed_session(data_dir: &std::path::Path, backend: &str, key: &str) {
    let backend = zeroclaw_infra::make_session_backend(data_dir, backend).expect("open backend");
    backend
        .append(key, &zeroclaw_providers::ChatMessage::user("seed message"))
        .expect("append seed message");
}

fn count_sessions(data_dir: &std::path::Path, backend: &str) -> usize {
    let backend = zeroclaw_infra::make_session_backend(data_dir, backend).expect("open backend");
    backend.list_sessions().len()
}

fn owner_of(data_dir: &std::path::Path, backend: &str, key: &str) -> Option<String> {
    let backend = zeroclaw_infra::make_session_backend(data_dir, backend).expect("open backend");
    backend.get_session_agent_alias(key).ok().flatten()
}

fn claim_ok_no_ghost_rows(backend: &str) {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let data_dir = write_config(config_dir.path(), backend);
    seed_session(&data_dir, backend, "gw_real");

    let before = count_sessions(&data_dir, backend);
    let out = run_zeroclaw(
        config_dir.path(),
        &[
            "migrate",
            "session-ownership",
            "--claim",
            "gw_real",
            "--agent-alias",
            "default",
            "--yes",
        ],
    );
    assert!(
        out.status.success(),
        "claim of an existing session for a configured agent should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        owner_of(&data_dir, backend, "gw_real").as_deref(),
        Some("default"),
        "session should now be owned by the agent"
    );
    assert_eq!(
        count_sessions(&data_dir, backend),
        before,
        "a successful claim must not create additional session rows"
    );
}

fn claim_unknown_session_fails_closed(backend: &str) {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let data_dir = write_config(config_dir.path(), backend);
    // No session seeded on purpose. Touch the backend so the sessions store
    // exists but is empty.
    let before = count_sessions(&data_dir, backend);

    let out = run_zeroclaw(
        config_dir.path(),
        &[
            "migrate",
            "session-ownership",
            "--claim",
            "gw_does_not_exist",
            "--agent-alias",
            "default",
            "--yes",
        ],
    );
    assert!(
        !out.status.success(),
        "claiming a nonexistent session must exit non-zero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not exist"),
        "error must come from the preflight (session-exists check), got: {stderr}"
    );
    assert_eq!(
        count_sessions(&data_dir, backend),
        before,
        "a rejected claim must not create a ghost session row/sidecar"
    );
    assert_eq!(
        owner_of(&data_dir, backend, "gw_does_not_exist"),
        None,
        "no ownership should have been written for a nonexistent session"
    );
}

fn claim_unknown_agent_fails_closed(backend: &str) {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let data_dir = write_config(config_dir.path(), backend);
    seed_session(&data_dir, backend, "gw_real");

    let out = run_zeroclaw(
        config_dir.path(),
        &[
            "migrate",
            "session-ownership",
            "--claim",
            "gw_real",
            "--agent-alias",
            "typo_agent",
            "--yes",
        ],
    );
    assert!(
        !out.status.success(),
        "claiming for an unconfigured agent alias must exit non-zero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown agent alias"),
        "error must come from the preflight (configured-agent check), got: {stderr}"
    );
    assert_eq!(
        owner_of(&data_dir, backend, "gw_real"),
        None,
        "a rejected claim must not write ownership for a typo alias"
    );
}

#[test]
fn migrate_claim_ok_no_ghost_rows_sqlite() {
    claim_ok_no_ghost_rows("sqlite");
}

#[test]
fn migrate_claim_ok_no_ghost_rows_jsonl() {
    claim_ok_no_ghost_rows("jsonl");
}

#[test]
fn migrate_claim_unknown_session_fails_closed_sqlite() {
    claim_unknown_session_fails_closed("sqlite");
}

#[test]
fn migrate_claim_unknown_session_fails_closed_jsonl() {
    claim_unknown_session_fails_closed("jsonl");
}

#[test]
fn migrate_claim_unknown_agent_fails_closed_sqlite() {
    claim_unknown_agent_fails_closed("sqlite");
}

#[test]
fn migrate_claim_unknown_agent_fails_closed_jsonl() {
    claim_unknown_agent_fails_closed("jsonl");
}

// ── helpers ──────────────────────────────────────────────────────────

/// Write a config that enables the given list of agents (in addition to
/// `default`, which is always enabled).  Returns the data dir.
fn write_config_with_agents(
    config_dir: &std::path::Path,
    backend: &str,
    extra_agents: &[&str],
) -> std::path::PathBuf {
    let mut config = format!(
        r#"schema_version = 3

[channels]
session_backend = "{}"

[agents.default]
enabled = true

"#,
        backend
    );
    for agent in extra_agents {
        config.push_str(&format!("[agents.{agent}]\nenabled = true\n\n"));
    }
    std::fs::write(config_dir.join("config.toml"), config).expect("write config");
    config_dir.join("data")
}

/// Run zeroclaw with stdin piped.  Callers pass the full text including
/// the trailing newline expected by `read_line`.
fn run_zeroclaw_with_stdin(config_dir: &std::path::Path, args: &[&str], stdin_str: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    let mut child = Command::new(bin)
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("LANG", "C")
        .env("RUST_LOG", "off")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run zeroclaw");
    if let Some(ref mut stdin_pipe) = child.stdin {
        stdin_pipe
            .write_all(stdin_str.as_bytes())
            .expect("write stdin");
    }
    // Dropping `child.stdin` above (via `if let` scope) closes the pipe
    // so the child sees EOF after the line.
    child.wait_with_output().expect("wait zeroclaw")
}

// ── Test 1: migration_claim_success ─────────────────────────────────
// A session with messages but no owner can be claimed successfully.

fn migration_claim_success(backend: &str) {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let data_dir = write_config(config_dir.path(), backend);
    seed_session(&data_dir, backend, "gw_success");

    let out = run_zeroclaw(
        config_dir.path(),
        &[
            "migrate",
            "session-ownership",
            "--claim",
            "gw_success",
            "--agent-alias",
            "default",
            "--yes",
        ],
    );
    assert!(
        out.status.success(),
        "claim of an unowned session should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        owner_of(&data_dir, backend, "gw_success").as_deref(),
        Some("default"),
        "session should now be owned by 'default'"
    );
}

#[test]
fn migration_claim_success_sqlite() {
    migration_claim_success("sqlite");
}

#[test]
fn migration_claim_success_jsonl() {
    migration_claim_success("jsonl");
}

// ── Test 2: migration_claim_conflict_bails ──────────────────────────
// Claiming a session already owned by a different alias must fail
// without overwriting.

fn migration_claim_conflict_bails(backend: &str) {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    // Config needs both "default" and "bob" — we claim AS "bob".
    let data_dir = write_config_with_agents(config_dir.path(), backend, &["bob"]);
    seed_session(&data_dir, backend, "gw_conflict");
    // Pre-set the owner to "alice" via the backend.
    {
        let be = zeroclaw_infra::make_session_backend(&data_dir, backend).expect("open backend");
        be.set_session_agent_alias("gw_conflict", "alice")
            .expect("set existing owner");
    }

    let out = run_zeroclaw(
        config_dir.path(),
        &[
            "migrate",
            "session-ownership",
            "--claim",
            "gw_conflict",
            "--agent-alias",
            "bob",
            "--yes",
        ],
    );
    assert!(
        !out.status.success(),
        "claim conflict must exit non-zero\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already owned"),
        "stderr must mention 'already owned', got: {stderr}"
    );
    assert!(
        stderr.contains("alice"),
        "stderr must mention existing owner 'alice', got: {stderr}"
    );
    // Owner must NOT have been overwritten.
    assert_eq!(
        owner_of(&data_dir, backend, "gw_conflict").as_deref(),
        Some("alice"),
        "session must still be owned by 'alice'"
    );
}

#[test]
fn migration_claim_conflict_bails_sqlite() {
    migration_claim_conflict_bails("sqlite");
}

#[test]
fn migration_claim_conflict_bails_jsonl() {
    migration_claim_conflict_bails("jsonl");
}

// ── Test 4: migration_bulk_mixed_outcomes ──────────────────────────
// Bulk migration with sessions in different ownership states.

fn migration_bulk_mixed_outcomes(backend: &str) {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let data_dir = write_config(config_dir.path(), backend);
    seed_session(&data_dir, backend, "gw_unowned_1");
    seed_session(&data_dir, backend, "gw_unowned_2");
    seed_session(&data_dir, backend, "gw_owned_diff");
    seed_session(&data_dir, backend, "gw_owned_target");
    // Pre-set owners on the already-owned sessions.
    {
        let be = zeroclaw_infra::make_session_backend(&data_dir, backend).expect("open backend");
        be.set_session_agent_alias("gw_owned_diff", "alice")
            .expect("set owner");
        be.set_session_agent_alias("gw_owned_target", "default")
            .expect("set owner");
    }

    let out = run_zeroclaw_with_stdin(
        config_dir.path(),
        &["migrate", "session-ownership"],
        "default\n",
    );
    assert!(
        out.status.success(),
        "bulk migration should exit 0\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("claimed 2"),
        "summary should show claimed 2, got: {stdout}"
    );
    assert!(
        stdout.contains("skipped 0"),
        "summary should show skipped 0, got: {stdout}"
    );
    assert!(
        stdout.contains("failed 0"),
        "summary should show failed 0, got: {stdout}"
    );

    // Unowned sessions got the new owner.
    assert_eq!(
        owner_of(&data_dir, backend, "gw_unowned_1").as_deref(),
        Some("default")
    );
    assert_eq!(
        owner_of(&data_dir, backend, "gw_unowned_2").as_deref(),
        Some("default")
    );
    // Already‑owned sessions were NOT overwritten.
    assert_eq!(
        owner_of(&data_dir, backend, "gw_owned_diff").as_deref(),
        Some("alice")
    );
    assert_eq!(
        owner_of(&data_dir, backend, "gw_owned_target").as_deref(),
        Some("default")
    );
}

#[test]
fn migration_bulk_mixed_outcomes_sqlite() {
    migration_bulk_mixed_outcomes("sqlite");
}

#[test]
fn migration_bulk_mixed_outcomes_jsonl() {
    migration_bulk_mixed_outcomes("jsonl");
}

// ── Test 5: migration_claim_atomically_prevents_overwrite ───────────
// A session atomically claimed by "bot-b" via the backend must not be
// overwritten when the migration CLI tries to claim it for "bot-a".

fn migration_claim_atomically_prevents_overwrite(backend: &str) {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    let data_dir = write_config_with_agents(config_dir.path(), backend, &["bot-a"]);
    seed_session(&data_dir, backend, "gw_atomic");
    // Atomically claim the session for "bot-b" directly via the backend.
    {
        let be = zeroclaw_infra::make_session_backend(&data_dir, backend).expect("open backend");
        let outcome = be
            .claim_session_agent_alias("gw_atomic", "bot-b")
            .expect("direct claim for bot-b should succeed");
        assert_eq!(outcome, ClaimOutcome::Claimed);
    }

    // Now the CLI tries to claim the same session for "bot-a".
    let out = run_zeroclaw(
        config_dir.path(),
        &[
            "migrate",
            "session-ownership",
            "--claim",
            "gw_atomic",
            "--agent-alias",
            "bot-a",
            "--yes",
        ],
    );
    assert!(
        !out.status.success(),
        "claim for bot-a must fail because bot-b owns the session\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("already owned"),
        "error should mention ownership conflict, got: {stderr}"
    );
    assert!(
        stderr.contains("bot-b"),
        "error should mention existing owner 'bot-b', got: {stderr}"
    );
    // Session must still be owned by "bot-b".
    assert_eq!(
        owner_of(&data_dir, backend, "gw_atomic").as_deref(),
        Some("bot-b"),
        "session must still be owned by 'bot-b'"
    );
}

#[test]
fn migration_claim_atomically_prevents_overwrite_sqlite() {
    migration_claim_atomically_prevents_overwrite("sqlite");
}

#[test]
fn migration_claim_atomically_prevents_overwrite_jsonl() {
    migration_claim_atomically_prevents_overwrite("jsonl");
}
