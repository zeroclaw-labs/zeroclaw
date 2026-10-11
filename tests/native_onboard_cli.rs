#![cfg(all(feature = "agent-runtime", unix))]

use std::path::Path;
use std::process::{Command, Output};

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command.args([
        "--config-dir",
        root.to_str().unwrap(),
        "native-onboard",
        "--client",
        "chatgpt-plan",
        "--provider-alias",
        "subscriber",
        "--agent-alias",
        "assistant",
        "--model",
        "model-fixture",
        "--risk-preset",
        "balanced",
        "--expected-billing",
        "subscription",
    ]);
    command.env("LC_ALL", "C").env("LANG", "C");
    command
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn native_onboard_cli_help_exposes_explicit_opt_ins_without_initializing_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let output = command(&root).arg("--help").output().unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        !help.contains("claude-code"),
        "unavailable native client must not be advertised"
    );
    for flag in [
        "--client",
        "--provider-alias",
        "--agent-alias",
        "--model",
        "--risk-preset",
        "--expected-billing",
        "--accept-yolo",
        "--accept-api-billing",
        "--native-config-dir",
        "--auth-profile",
    ] {
        assert!(help.contains(flag), "missing {flag}");
    }
    assert!(!root.exists());
}

#[test]
fn native_onboard_cli_refuses_arbitrary_precreated_root_before_auth_or_config_load() {
    let root = tempfile::tempdir().unwrap();
    let original = b"operator-owned data";
    std::fs::write(root.path().join("keep"), original).unwrap();
    let output = command(root.path()).output().unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("owned fresh instance"),
        "{}",
        stderr(&output)
    );
    assert_eq!(std::fs::read(root.path().join("keep")).unwrap(), original);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn native_onboard_cli_refuses_root_symlink_without_writing_its_target() {
    let parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = parent.path().join("alias");
    std::os::unix::fs::symlink(outside.path(), &root).unwrap();
    let output = command(&root).output().unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("owned fresh instance"),
        "{}",
        stderr(&output)
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn native_onboard_cli_yolo_requires_fresh_acceptance_before_creating_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let mut args = command(&root)
        .get_args()
        .map(|a| a.to_owned())
        .collect::<Vec<_>>();
    let risk = args.iter().position(|a| a == "--risk-preset").unwrap();
    args[risk + 1] = "yolo".into();
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--accept-yolo"),
        "{}",
        stderr(&output)
    );
    assert!(!root.exists());
}

#[test]
fn native_onboard_cli_rejects_plan_api_billing_before_creating_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let mut args = command(&root)
        .get_args()
        .map(|a| a.to_owned())
        .collect::<Vec<_>>();
    let billing = args.iter().position(|a| a == "--expected-billing").unwrap();
    args[billing + 1] = "api".into();
    args.push("--accept-api-billing".into());
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("subscription"),
        "{}",
        stderr(&output)
    );
    assert!(!root.exists());
}

#[test]
fn native_onboard_cli_hides_unavailable_claude_client_and_rejects_before_root_creation() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("fresh");
    let mut args = command(&root)
        .get_args()
        .map(|a| a.to_owned())
        .collect::<Vec<_>>();
    let client = args.iter().position(|a| a == "--client").unwrap();
    args[client + 1] = "claude-code".into();
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("invalid value"),
        "{}",
        stderr(&output)
    );
    assert!(!root.exists());
}
