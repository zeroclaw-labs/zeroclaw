//! Regression: `zeroclaw cron` delivery flags at the shipped CLI boundary.
//!
//! The unit tests around `handle_command` construct `CronCommands` values
//! directly, so they never exercise Clap parsing, the process exit status, or
//! the terminal output the delivery flags exist to produce. This spawns the real
//! binary against an isolated config directory and asserts what a user actually
//! observes: stdout, exit status, and the saved job.
//!
//! It covers the `update` patch contract, which the command documents as "only
//! the fields you specify are changed; others remain unchanged". Before the fix
//! `build_delivery` produced a whole new `DeliveryConfig` and `update_job`
//! assigned it wholesale, so changing a channel silently dropped an existing
//! thread id and reset `best_effort` back to true.

use std::path::Path;
use std::process::{Command, Output};

use zeroclaw_config::schema::Config;
use zeroclaw_runtime::cron;

/// `locale = "en"` is pinned so stdout assertions do not depend on the
/// environment; `config_dir_locale_regression.rs` shows config drives locale.
/// The explicit `risk_profile` is required — without it every cron command
/// fails with "no resolvable risk_profile".
const CONFIG_TOML: &str = r#"schema_version = 3
locale = "en"

[risk_profiles.default]

[agents.default]
enabled = true
risk_profile = "default"
"#;

fn run(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run zeroclaw")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} should succeed (status {:?})\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        stdout_of(out),
        stderr_of(out)
    );
}

/// The job id printed by `cron add`. The surrounding label is a Fluent message,
/// so the id is located by shape (a UUID) rather than by matching text, which
/// keeps this independent of the active locale.
fn job_id_from(out: &Output) -> String {
    let stdout = stdout_of(out);
    stdout
        .split_whitespace()
        .find(|token| {
            token.len() == 36
                && token.chars().enumerate().all(|(i, c)| {
                    if matches!(i, 8 | 13 | 18 | 23) {
                        c == '-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
        })
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no job id in output:\n{stdout}"))
}

/// Read the persisted job through the runtime's own store API. `cron_db_path`
/// resolves to `<data_dir>/cron/jobs.db`, and the binary puts `data_dir` at
/// `<config dir>/data`.
fn stored_job(config_dir: &Path, id: &str) -> cron::CronJob {
    let config = Config {
        data_dir: config_dir.join("data"),
        config_path: config_dir.join("config.toml"),
        ..Config::default()
    };
    cron::get_job(&config, id).expect("stored cron job")
}

#[test]
fn cron_update_patches_delivery_without_dropping_unspecified_fields() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let config_dir = dir.path();
    std::fs::write(config_dir.join("config.toml"), CONFIG_TOML).expect("write config");

    // 1. Create with a full delivery config: channel, recipient, thread, and a
    //    non-default failure policy.
    let add = run(
        config_dir,
        &[
            "cron",
            "add",
            "*/5 * * * *",
            "--agent",
            "default",
            "--channel",
            "telegram",
            "--to",
            "111",
            "--thread",
            "t-1",
            "--no-best-effort",
            "echo hi",
        ],
    );
    assert_ok(&add, "cron add with delivery");
    let add_stdout = stdout_of(&add);
    assert!(
        add_stdout.contains("telegram → 111"),
        "create must print where output will go:\n{add_stdout}"
    );

    let id = job_id_from(&add);
    let created = stored_job(config_dir, &id);
    assert_eq!(created.delivery.mode, "announce");
    assert_eq!(created.delivery.thread_id.as_deref(), Some("t-1"));
    assert!(!created.delivery.best_effort);

    // 2. Repoint the destination. Thread and best-effort were not named, so the
    //    patch contract says they survive.
    let update = run(
        config_dir,
        &[
            "cron",
            "update",
            &id,
            "--agent",
            "default",
            "--channel",
            "discord",
            "--to",
            "222",
        ],
    );
    assert_ok(&update, "cron update with delivery");
    let update_stdout = stdout_of(&update);
    assert!(
        update_stdout.contains("discord → 222"),
        "update must print the resolved destination:\n{update_stdout}"
    );

    let updated = stored_job(config_dir, &id);
    assert_eq!(updated.delivery.channel.as_deref(), Some("discord"));
    assert_eq!(updated.delivery.to.as_deref(), Some("222"));
    assert_eq!(
        updated.delivery.thread_id.as_deref(),
        Some("t-1"),
        "thread id must survive a channel/recipient change"
    );
    assert!(
        !updated.delivery.best_effort,
        "best_effort=false must survive a channel/recipient change"
    );

    // 3. `--channel` alone repoints an announcing job without restating the
    //    recipient. This is the patch contract doing its job: the recipient,
    //    thread and policy all carry over.
    let channel_only = run(
        config_dir,
        &[
            "cron",
            "update",
            &id,
            "--agent",
            "default",
            "--channel",
            "slack",
        ],
    );
    assert_ok(&channel_only, "cron update with --channel alone");
    let channel_only_stdout = stdout_of(&channel_only);
    assert!(
        channel_only_stdout.contains("slack → 222"),
        "--channel alone must keep the existing recipient:\n{channel_only_stdout}"
    );
    let repointed = stored_job(config_dir, &id);
    assert_eq!(repointed.delivery.to.as_deref(), Some("222"));
    assert_eq!(repointed.delivery.thread_id.as_deref(), Some("t-1"));
    assert!(!repointed.delivery.best_effort);

    // 4. An incomplete update to this same job is rejected and changes nothing.
    //    An empty value is not inherited from the stored config the way an
    //    omitted flag is, so it leaves the merged config without a channel.
    let blanked = run(
        config_dir,
        &["cron", "update", &id, "--agent", "default", "--channel", ""],
    );
    assert!(
        !blanked.status.success(),
        "an empty channel must fail rather than clear the field\nstdout:\n{}",
        stdout_of(&blanked)
    );
    let blanked_stderr = stderr_of(&blanked);
    assert!(
        blanked_stderr.contains("delivery.channel is required"),
        "rejection must explain the missing channel:\n{blanked_stderr}"
    );
    let after_blank = stored_job(config_dir, &id);
    assert_eq!(
        after_blank.delivery.channel.as_deref(),
        Some("slack"),
        "a rejected update must not change the stored job"
    );
    assert_eq!(after_blank.delivery.to.as_deref(), Some("222"));
    assert_eq!(after_blank.delivery.thread_id.as_deref(), Some("t-1"));
    assert!(!after_blank.delivery.best_effort);

    // 5. A job with delivery off has nothing to inherit, so partial flags are
    //    an incomplete announce config and must be rejected, leaving the stored
    //    job untouched.
    let add_bare = run(
        config_dir,
        &[
            "cron",
            "add",
            "*/9 * * * *",
            "--agent",
            "default",
            "echo bare",
        ],
    );
    assert_ok(&add_bare, "cron add without delivery");
    assert!(
        stdout_of(&add_bare).contains("disabled"),
        "a job with no delivery flags must say so:\n{}",
        stdout_of(&add_bare)
    );
    let bare_id = job_id_from(&add_bare);
    assert_eq!(stored_job(config_dir, &bare_id).delivery.mode, "none");

    let rejected = run(
        config_dir,
        &[
            "cron",
            "update",
            &bare_id,
            "--agent",
            "default",
            "--channel",
            "slack",
        ],
    );
    assert!(
        !rejected.status.success(),
        "an incomplete announce config must fail\nstdout:\n{}",
        stdout_of(&rejected)
    );
    let rejected_stderr = stderr_of(&rejected);
    assert!(
        rejected_stderr.contains("delivery.to is required"),
        "rejection must explain the missing recipient:\n{rejected_stderr}"
    );
    assert_eq!(
        stored_job(config_dir, &bare_id).delivery.mode,
        "none",
        "a rejected update must not change the stored job"
    );

    // The first job is also untouched by the failed update to the second.
    let untouched = stored_job(config_dir, &id);
    assert_eq!(untouched.delivery.channel.as_deref(), Some("slack"));
    assert_eq!(untouched.delivery.to.as_deref(), Some("222"));
}

/// Regression: Telegram group and channel ids are negative (`-100…`), and clap
/// treats a hyphen-prefixed token as a flag unless the argument opts out. Before
/// `allow_negative_numbers`, `--to -100123456` exited 2 with
/// `unexpected argument '-1' found` before any cron validation ran, so the
/// advertised `--to <DELIVERY_TO>` form was unusable for the most common
/// Telegram target and only the undocumented `--to=` form worked.
///
/// The flag is shared by `add`, `add-at`, `add-every`, `once` and `update`, so
/// this covers create and update; the parse happens in the same flattened struct
/// for all five.
#[test]
fn cron_delivery_accepts_negative_telegram_chat_id() {
    let dir = tempfile::tempdir().expect("temp config dir");
    let config_dir = dir.path();
    std::fs::write(config_dir.join("config.toml"), CONFIG_TOML).expect("write config");

    // Create with a negative id in the normal separate-token form.
    let add = run(
        config_dir,
        &[
            "cron",
            "add",
            "*/5 * * * *",
            "--agent",
            "default",
            "--channel",
            "telegram",
            "--to",
            "-100123456",
            "echo hi",
        ],
    );
    assert_ok(&add, "cron add with a negative Telegram chat id");
    assert!(
        stdout_of(&add).contains("telegram → -100123456"),
        "the negative id must survive to the confirmation line:\n{}",
        stdout_of(&add)
    );

    let id = job_id_from(&add);
    assert_eq!(
        stored_job(config_dir, &id).delivery.to.as_deref(),
        Some("-100123456"),
        "the negative id must be persisted verbatim"
    );

    // And on the update path, which shares the flattened argument struct.
    let update = run(
        config_dir,
        &[
            "cron",
            "update",
            &id,
            "--agent",
            "default",
            "--to",
            "-100999888",
        ],
    );
    assert_ok(&update, "cron update with a negative Telegram chat id");
    assert_eq!(
        stored_job(config_dir, &id).delivery.to.as_deref(),
        Some("-100999888")
    );

    // A forum topic target is `chat:thread`, hyphen-led but not a number, which
    // is why `allow_negative_numbers` alone is insufficient.
    let composite = run(
        config_dir,
        &[
            "cron",
            "add",
            "*/13 * * * *",
            "--agent",
            "default",
            "--channel",
            "telegram",
            "--to",
            "-100123456:42",
            "echo topic",
        ],
    );
    assert_ok(&composite, "cron add with a chat:thread target");
    assert_eq!(
        stored_job(config_dir, &job_id_from(&composite))
            .delivery
            .to
            .as_deref(),
        Some("-100123456:42")
    );

    // `allow_hyphen_values` alone would consume `--thread` as the recipient here
    // and then fail on the positional argument. The value parser keeps the
    // mistake legible by naming the offending token.
    let flag_shaped = run(
        config_dir,
        &[
            "cron",
            "add",
            "*/5 * * * *",
            "--agent",
            "default",
            "--channel",
            "telegram",
            "--to",
            "--thread",
            "t-1",
            "echo hi",
        ],
    );
    assert!(
        !flag_shaped.status.success(),
        "--to with a following flag must not be accepted\nstdout:\n{}",
        stdout_of(&flag_shaped)
    );
    let flag_shaped_stderr = stderr_of(&flag_shaped);
    assert!(
        flag_shaped_stderr.contains("looks like a flag, not a recipient"),
        "the error must name the offending token rather than the positional:\n{flag_shaped_stderr}"
    );
    assert!(
        flag_shaped_stderr.contains("--thread"),
        "the error must quote the token that was mistaken for a value:\n{flag_shaped_stderr}"
    );
}
