//! CLI-boundary regressions for pairing-channel operator binds.

use std::path::Path;
use std::process::{Command, Output};
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

fn seed_config(config_dir: &Path) {
    std::fs::write(
        config_dir.join("config.toml"),
        r#"schema_version = 3
locale = "en"

[channels.wechat.default]
enabled = false

[channels.wechat.support]
enabled = false

[channels.line.default]
enabled = false

[channels.line.support]
enabled = false
"#,
    )
    .expect("write bind CLI config");
}

fn run_bind(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        // Keep managed-service discovery and locale resolution deterministic.
        .env("HOME", config_dir)
        .env("USERPROFILE", config_dir)
        .env("PATH", config_dir)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("RUST_LOG", "off")
        .args(["channel"])
        .args(args)
        .output()
        .expect("run channel bind command")
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_stdout_contains(output: &Output, expected: &str, context: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(expected),
        "{context} should use the localized output contract\nexpected:\n{expected}\nstdout:\n{stdout}"
    );
}

fn load_saved(config_dir: &Path) -> Config {
    let saved = std::fs::read_to_string(config_dir.join("config.toml"))
        .expect("read persisted bind config");
    toml::from_str(&saved).expect("persisted bind config should parse")
}

fn assert_bound(config: &Config, channel_type: &str, alias: &str, identity: &str) {
    let group_name = format!("{channel_type}_{alias}");
    let group = config
        .peer_groups
        .get(&group_name)
        .unwrap_or_else(|| panic!("missing persisted peer group {group_name}"));
    assert_eq!(group.channel.as_str(), format!("{channel_type}.{alias}"));
    assert_eq!(
        group
            .external_peers
            .iter()
            .map(|peer| peer.as_str())
            .collect::<Vec<_>>(),
        [identity],
    );
}

fn exercise_bind_cli(channel_type: &str, default_identity: &str, support_identity: &str) {
    zeroclaw_runtime::i18n::init("en");
    let config_dir = tempfile::tempdir().expect("temp bind CLI config dir");
    seed_config(config_dir.path());
    let command = format!("bind-{channel_type}");

    let help = run_bind(config_dir.path(), &[&command, "--help"]);
    assert_success(&help, "bind help");
    assert_stdout_contains(
        &help,
        &get_required_cli_string(&format!("cli-channel-bind-{channel_type}-long-about")),
        "bind help",
    );

    let default_bind = run_bind(config_dir.path(), &[&command, default_identity]);
    assert_success(&default_bind, "default-alias bind");
    let default_ref = format!("{channel_type}.default");
    assert_stdout_contains(
        &default_bind,
        &get_required_cli_string_with_args(
            "cli-channel-bind-success",
            &[
                ("channel", channel_type),
                ("channel_ref", &default_ref),
                ("identity", default_identity),
            ],
        ),
        "default-alias bind",
    );
    assert_stdout_contains(
        &default_bind,
        &get_required_cli_string_with_args(
            "cli-channel-bind-saved",
            &[(
                "path",
                &config_dir.path().join("config.toml").display().to_string(),
            )],
        ),
        "default-alias bind",
    );
    assert_stdout_contains(
        &default_bind,
        &get_required_cli_string("cli-channel-bind-daemon-not-running"),
        "default-alias bind",
    );
    assert_bound(
        &load_saved(config_dir.path()),
        channel_type,
        "default",
        default_identity,
    );

    let support_bind = run_bind(
        config_dir.path(),
        &[&command, support_identity, "--alias", "support"],
    );
    assert_success(&support_bind, "explicit-alias bind");
    assert_bound(
        &load_saved(config_dir.path()),
        channel_type,
        "support",
        support_identity,
    );

    let duplicate = run_bind(
        config_dir.path(),
        &[&command, support_identity, "--alias", "support"],
    );
    assert_success(&duplicate, "idempotent re-bind");
    let support_ref = format!("{channel_type}.support");
    assert_stdout_contains(
        &duplicate,
        &get_required_cli_string_with_args(
            "cli-channel-bind-already-bound",
            &[
                ("channel", channel_type),
                ("channel_ref", &support_ref),
                ("identity", support_identity),
            ],
        ),
        "idempotent re-bind",
    );
    assert_bound(
        &load_saved(config_dir.path()),
        channel_type,
        "support",
        support_identity,
    );

    let missing_alias = run_bind(
        config_dir.path(),
        &[&command, "missing-user", "--alias", "missing"],
    );
    assert!(
        !missing_alias.status.success(),
        "unconfigured alias should fail"
    );
    let section = format!("channels.{channel_type}.missing");
    let expected = get_required_cli_string_with_args(
        "cli-channel-bind-alias-not-configured",
        &[
            ("channel", channel_type),
            ("alias", "missing"),
            ("section", &section),
        ],
    );
    let stderr = String::from_utf8_lossy(&missing_alias.stderr);
    assert!(
        stderr.contains(&expected),
        "failure should use the localized missing-alias contract: {stderr}",
    );
    assert!(
        !load_saved(config_dir.path())
            .peer_groups
            .contains_key(&format!("{channel_type}_missing")),
        "failed bind must not persist a phantom peer group",
    );
}

#[test]
fn bind_wechat_cli_persists_aliases_and_rejects_phantoms() {
    exercise_bind_cli("wechat", "wx_default", "wx_support");
}

#[test]
fn bind_line_cli_persists_aliases_and_rejects_phantoms() {
    exercise_bind_cli("line", "Udefault", "Usupport");
}
