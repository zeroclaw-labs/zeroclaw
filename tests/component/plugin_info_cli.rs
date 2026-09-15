//! Exit-code contract of `zeroclaw plugin info`.
//!
//! A script that asks about a plugin must be able to branch on the exit code:
//! zero means the plugin is installed and loads here, non-zero means it does
//! not (missing, or refused by this host). This pins the missing half.

use std::process::{Command, Output};

fn run_zeroclaw(config_dir: &std::path::Path, args: &[&str]) -> Output {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    Command::new(bin)
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("RUST_LOG", "off")
        .args(args)
        .output()
        .expect("run zeroclaw")
}

fn throwaway_config_dir() -> tempfile::TempDir {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    std::fs::write(
        config_dir.path().join("config.toml"),
        "schema_version = 3\n",
    )
    .expect("write config");
    config_dir
}

#[test]
fn plugin_info_on_an_unknown_name_exits_non_zero_and_names_it() {
    let config_dir = throwaway_config_dir();
    let out = run_zeroclaw(config_dir.path(), &["plugin", "info", "no-such-plugin"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "an unknown plugin must not exit 0: {combined}"
    );
    assert!(
        combined.contains("no-such-plugin"),
        "the failure must name the plugin asked about: {combined}"
    );
}

#[test]
fn plugin_list_with_nothing_installed_still_exits_zero() {
    // The control: listing is a report, not a check, and stays zero.
    let config_dir = throwaway_config_dir();
    let out = run_zeroclaw(config_dir.path(), &["plugin", "list"]);
    assert!(
        out.status.success(),
        "plugin list must exit 0 with nothing installed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
