//! `zeroclaw user` through the shipped binary: clap dispatch, the global
//! config load, the policy check before a prompt and before a write, the
//! incremental save, and the secret encryption of `password_hash`, against
//! an isolated config dir.
//!
//! Assertions on refusals look for text every CLI catalog carries (flag
//! names, numbers, entry paths), so they hold whatever locale the host uses.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use zeroclaw_config::password_hash::{Decoy, verify_password};
use zeroclaw_config::secrets::SecretStore;

const PASSWORD: &str = "zeroclaw-test-passphrase";
const ROTATED: &str = "zeroclaw-rotated-passphrase";
const PROFILES: &str =
    "schema_version = 3\n\n[permission_profiles.operator]\ngrants = { sessions = [\"read\"] }\n";

fn write_config(dir: &Path, contents: &str) {
    std::fs::write(dir.join("config.toml"), contents).expect("write config");
}

fn read_config(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("config.toml")).expect("read config")
}

fn zeroclaw_with(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command
        .env("RUST_LOG", "off")
        .env("LC_ALL", "C")
        .arg("--config-dir")
        .arg(dir)
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn zeroclaw");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("wait for zeroclaw")
}

fn zeroclaw(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    zeroclaw_with(dir, args, stdin, &[])
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The stored hash of `users.<name>`, decrypted with the config dir's key.
fn stored_hash(dir: &Path, name: &str) -> Option<String> {
    let config: toml::Value = toml::from_str(&read_config(dir)).expect("config parses");
    let stored = config
        .get("users")?
        .get(name)?
        .get("password_hash")?
        .as_str()?
        .to_owned();
    assert!(
        stored.starts_with("enc2:"),
        "the hash must be encrypted at rest: {stored}"
    );
    Some(
        SecretStore::new(dir, true)
            .decrypt(&stored)
            .expect("decrypt the stored hash"),
    )
}

fn matches(dir: &Path, name: &str, password: &str) -> bool {
    let hash = stored_hash(dir, name).expect("a stored hash");
    verify_password(password, Some(&hash), &Decoy::default())
}

fn add_password_user(dir: &Path, extra: &[&str]) {
    let mut args = vec![
        "user",
        "add",
        "zeroclaw_user",
        "--profile",
        "operator",
        "--password-stdin",
    ];
    args.extend_from_slice(extra);
    let added = zeroclaw(dir, &args, Some(&format!("{PASSWORD}\n")));
    assert!(added.status.success(), "add failed: {}", stderr(&added));
}

#[test]
fn add_list_rotate_and_remove_a_password_user() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);

    add_password_user(dir.path(), &[]);
    let config = read_config(dir.path());
    assert!(config.contains("[users.zeroclaw_user]"), "{config}");
    assert!(!config.contains(PASSWORD), "plaintext password on disk");
    assert!(matches(dir.path(), "zeroclaw_user", PASSWORD));

    let listed = zeroclaw(dir.path(), &["user", "list"], None);
    assert!(listed.status.success(), "list failed: {}", stderr(&listed));
    let listing = stdout(&listed);
    assert!(listing.contains("zeroclaw_user"), "{listing}");
    assert!(
        !listing.contains("$scrypt$"),
        "list must never print a hash"
    );

    let rotated = zeroclaw(
        dir.path(),
        &["user", "passwd", "zeroclaw_user", "--password-stdin"],
        Some(&format!("{ROTATED}\n")),
    );
    assert!(
        rotated.status.success(),
        "passwd failed: {}",
        stderr(&rotated)
    );
    assert!(matches(dir.path(), "zeroclaw_user", ROTATED));
    assert!(!matches(dir.path(), "zeroclaw_user", PASSWORD));

    let removed = zeroclaw(dir.path(), &["user", "remove", "zeroclaw_user"], None);
    assert!(
        removed.status.success(),
        "remove failed: {}",
        stderr(&removed)
    );
    let config = read_config(dir.path());
    assert!(!config.contains("zeroclaw_user"), "{config}");
    assert!(
        config.contains("[permission_profiles.operator]"),
        "unrelated sections survive the edit: {config}"
    );
}

#[test]
fn a_crlf_line_is_one_password() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);
    let added = zeroclaw(
        dir.path(),
        &[
            "user",
            "add",
            "zeroclaw_user",
            "--profile",
            "operator",
            "--password-stdin",
        ],
        Some(&format!("{PASSWORD}\r\n")),
    );
    assert!(added.status.success(), "add failed: {}", stderr(&added));
    assert!(matches(dir.path(), "zeroclaw_user", PASSWORD));
}

#[test]
fn disable_password_keeps_the_uid() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);
    add_password_user(dir.path(), &["--uid", "4101"]);

    let disabled = zeroclaw(
        dir.path(),
        &["user", "disable-password", "zeroclaw_user"],
        None,
    );
    assert!(
        disabled.status.success(),
        "disable-password failed: {}",
        stderr(&disabled)
    );
    assert_eq!(stored_hash(dir.path(), "zeroclaw_user"), None);
    let config: toml::Value = toml::from_str(&read_config(dir.path())).expect("parse");
    assert_eq!(
        config["users"]["zeroclaw_user"]["uid"].as_integer(),
        Some(4101)
    );
}

#[test]
fn an_entry_without_a_credential_is_refused_by_flag() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);
    let output = zeroclaw(
        dir.path(),
        &["user", "add", "zeroclaw_user", "--profile", "operator"],
        None,
    );
    assert!(!output.status.success(), "an entry needs uid or a password");
    let reason = stderr(&output);
    assert!(
        reason.contains("--uid") && reason.contains("--password-stdin"),
        "the refusal names the flags that supply a credential: {reason}"
    );
    assert_eq!(
        read_config(dir.path()),
        PROFILES,
        "a refused edit writes nothing"
    );
}

#[test]
fn a_short_password_is_refused() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);
    let output = zeroclaw(
        dir.path(),
        &[
            "user",
            "add",
            "zeroclaw_user",
            "--profile",
            "operator",
            "--password-stdin",
        ],
        Some("too-short\n"),
    );
    assert!(!output.status.success(), "a short password must be refused");
    assert!(stderr(&output).contains("15"), "{}", stderr(&output));
    assert_eq!(read_config(dir.path()), PROFILES);
}

#[test]
fn the_entry_is_checked_before_the_password_is_read() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);
    // The profile does not exist. Reading the too-short password would
    // report its length; the refusal must be about the profile instead.
    let output = zeroclaw(
        dir.path(),
        &[
            "user",
            "add",
            "zeroclaw_user",
            "--profile",
            "zeroclaw_no_such_profile",
            "--password-stdin",
        ],
        Some("too-short\n"),
    );
    assert!(!output.status.success());
    let reason = stderr(&output);
    assert!(reason.contains("zeroclaw_no_such_profile"), "{reason}");
    assert!(
        !reason.contains("15"),
        "the password was read first: {reason}"
    );
    assert_eq!(read_config(dir.path()), PROFILES);
}

#[test]
fn the_only_credential_cannot_be_disabled() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);
    add_password_user(dir.path(), &[]);
    let before = read_config(dir.path());

    let output = zeroclaw(
        dir.path(),
        &["user", "disable-password", "zeroclaw_user"],
        None,
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("zeroclaw user remove zeroclaw_user"),
        "{}",
        stderr(&output)
    );
    assert_eq!(read_config(dir.path()), before);
}

#[test]
fn a_roster_that_failed_to_load_is_not_edited() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    let broken = "schema_version = 3\nusers = \"not a table\"\n";
    write_config(dir.path(), broken);
    for args in [
        &["user", "list"][..],
        &["user", "remove", "zeroclaw_user"][..],
    ] {
        let output = zeroclaw(dir.path(), args, None);
        assert!(!output.status.success(), "{args:?} must refuse");
        // Not the unknown-entry refusal, which every catalog words around
        // the `[users.<name>]` header.
        assert!(
            !stderr(&output).contains("[users.zeroclaw_user]"),
            "{args:?} must refuse for the failed load, not a missing entry: {}",
            stderr(&output)
        );
    }
    assert_eq!(read_config(dir.path()), broken);
}

#[test]
fn passwd_repairs_an_entry_the_policy_refuses() {
    let out_of_bounds = "$scrypt$ln=18,r=8,p=1$BwcHBwcHBwcHBwcHBwcHBw$\
                         CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk";
    for broken_entry in [
        String::new(),
        format!("password_hash = \"{out_of_bounds}\"\n"),
    ] {
        let dir = tempfile::tempdir().expect("temporary config directory");
        write_config(
            dir.path(),
            &format!(
                "{PROFILES}\n[users.zeroclaw_user]\npermission_profiles = [\"operator\"]\n{broken_entry}"
            ),
        );
        let repaired = zeroclaw(
            dir.path(),
            &["user", "passwd", "zeroclaw_user", "--password-stdin"],
            Some(&format!("{PASSWORD}\n")),
        );
        assert!(
            repaired.status.success(),
            "passwd must repair {broken_entry:?}: {}",
            stderr(&repaired)
        );
        assert!(matches(dir.path(), "zeroclaw_user", PASSWORD));
    }
}

#[test]
fn overrides_the_check_does_not_read_do_not_block_an_edit() {
    let oidc = format!(
        "{PROFILES}\n[oidc.corp]\nissuer = \"https://sso.example.com/realms/main\"\n\
         audience = \"zeroclaw\"\nvalidation = \"introspection\"\nclaim_path = \"roles\"\n\
         profile_map = {{ \"zeroclaw-operators\" = \"operator\" }}\n"
    );
    let cases = [
        (
            PROFILES.to_owned(),
            ("ZEROCLAW_security__otp__enabled", "false"),
        ),
        // Introspection needs a client secret, supplied here only through the
        // environment, the documented way to keep it out of the file.
        (
            oidc,
            ("ZEROCLAW_oidc__corp__client_secret", "zeroclaw-test-secret"),
        ),
    ];
    for (config, env) in cases {
        let dir = tempfile::tempdir().expect("temporary config directory");
        write_config(dir.path(), &config);
        let added = zeroclaw_with(
            dir.path(),
            &[
                "user",
                "add",
                "zeroclaw_user",
                "--profile",
                "operator",
                "--uid",
                "4103",
            ],
            None,
            &[env],
        );
        assert!(
            added.status.success(),
            "{} must not block the edit: {}",
            env.0,
            stderr(&added)
        );
        assert!(
            !read_config(dir.path()).contains(env.1),
            "an override is never written to the file"
        );
    }
}

#[test]
fn an_environment_override_on_an_auth_section_is_refused() {
    let dir = tempfile::tempdir().expect("temporary config directory");
    write_config(dir.path(), PROFILES);
    let output = zeroclaw_with(
        dir.path(),
        &[
            "user",
            "add",
            "zeroclaw_user",
            "--profile",
            "zeroclaw_env_admin",
            "--uid",
            "4102",
        ],
        None,
        &[(
            "ZEROCLAW_permission_profiles__zeroclaw_env_admin__admin",
            "true",
        )],
    );
    assert!(
        !output.status.success(),
        "a profile that exists only in the environment must not pass the check"
    );
    assert!(
        stderr(&output).contains("permission_profiles.zeroclaw_env_admin.admin"),
        "the refusal names the overridden path: {}",
        stderr(&output)
    );
    assert_eq!(read_config(dir.path()), PROFILES);
}

#[test]
fn hash_password_prints_only_the_hash_and_loads_no_config() {
    let empty = tempfile::tempdir().expect("temporary config directory");
    let otp = tempfile::tempdir().expect("temporary config directory");
    // With the OTP prelude on and no seed yet, a config-loading run would
    // print an enrollment URI on stdout.
    write_config(
        otp.path(),
        "schema_version = 3\n\n[security.otp]\nenabled = true\n",
    );

    for dir in [empty.path(), otp.path()] {
        let output = zeroclaw(
            dir,
            &["user", "hash-password", "--password-stdin"],
            Some(&format!("{PASSWORD}\n")),
        );
        assert!(
            output.status.success(),
            "hash-password failed: {}",
            stderr(&output)
        );
        let out = stdout(&output);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1, "stdout carries only the hash: {out:?}");
        assert!(lines[0].starts_with("$scrypt$ln=15,r=8,p=3$"), "{out}");
        assert!(verify_password(PASSWORD, Some(lines[0]), &Decoy::default()));
    }
    assert!(
        !empty.path().join("config.toml").exists(),
        "hash-password must not create a config"
    );
}
