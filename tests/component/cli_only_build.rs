//! A build without the gateway (the CLI-only build) points users to the
//! standard build for the dashboard instead of failing with a build hint.

use std::path::Path;
use std::process::{Command, Output};

const NO_DASHBOARD: &str = "This ZeroClaw build has no dashboard or web gateway";

fn zeroclaw(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .arg("--config-dir")
        .arg(config_dir)
        .args(args)
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .unwrap()
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn gateway_and_desktop_commands_point_to_the_standard_build() {
    let config_dir = tempfile::tempdir().unwrap();
    for args in [
        &["gateway"][..],
        &["gateway", "start"],
        &["gateway", "restart"],
        &["gateway", "get-paircode"],
        &["desktop"],
    ] {
        let output = zeroclaw(config_dir.path(), args);
        let text = combined(&output);
        assert!(
            !output.status.success(),
            "{args:?} must fail in a build without the gateway: {text}"
        );
        assert!(
            text.contains(NO_DASHBOARD) && text.contains("install.sh"),
            "{args:?} must point to the standard build: {text}"
        );
    }
}

#[test]
fn status_says_the_dashboard_is_not_in_this_build() {
    let config_dir = tempfile::tempdir().unwrap();
    let output = zeroclaw(config_dir.path(), &["status"]);
    let text = combined(&output);
    assert!(output.status.success(), "status failed: {text}");
    assert!(text.contains("not included in this build"), "{text}");
    assert!(!text.contains("MISSING"), "{text}");
}
