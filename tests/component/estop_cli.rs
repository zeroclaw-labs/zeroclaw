use std::path::Path;
use std::process::{Command, Output};

const STATE_BEFORE: &str = "{\n  \"kill_all\": false,\n  \"network_kill\": false,\n  \"blocked_domains\": [],\n  \"frozen_tools\": [],\n  \"updated_at\": null\n}\n";

fn write_config(config_dir: &Path, state_file: &Path, enabled: bool) {
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "schema_version = 4\nlocale = \"en\"\n\n[security.estop]\nenabled = {enabled}\nrequire_otp_to_resume = false\nstate_file = '{}'\n",
            state_file.display()
        ),
    )
    .unwrap();
}

fn run_estop(config_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("RUST_LOG", "off")
        .env_remove("ZEROCLAW_CONFIG_DIR")
        .env_remove("ZEROCLAW_DATA_DIR")
        .env_remove("ZEROCLAW_WORKSPACE")
        .arg("--config-dir")
        .arg(config_dir)
        .arg("estop")
        .args(args)
        .output()
        .expect("failed to run zeroclaw estop")
}

fn assert_refused(output: &Output, args: &[&str]) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "`zeroclaw estop {args:?}` must not succeed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("`zeroclaw estop` is not enforced"),
        "`zeroclaw estop {args:?}` must say it is not enforced\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("Estop engaged") && !stdout.contains("Estop resume completed"),
        "`zeroclaw estop {args:?}` must not report a stop\nstdout:\n{stdout}"
    );
}

const FORMS: &[&[&str]] = &[
    &[],
    &["--level", "kill-all"],
    &["--level", "network-kill"],
    &["--level", "domain-block", "--domain", "*.example.com"],
    &["--level", "tool-freeze", "--tool", "shell"],
    &["status"],
    &["resume"],
    &["resume", "--network"],
    &["resume", "--domain", "*.example.com"],
    &["resume", "--tool", "shell"],
    &["resume", "--otp", "123456"],
    &["--level", "domain-block"],
    &["resume", "--network", "--tool", "shell"],
];

#[test]
fn estop_refuses_every_form_and_leaves_the_state_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    write_config(dir.path(), &state_file, true);
    std::fs::write(&state_file, STATE_BEFORE).unwrap();

    for args in FORMS {
        let output = run_estop(dir.path(), args);
        assert_refused(&output, args);
        assert_eq!(
            std::fs::read_to_string(&state_file).unwrap(),
            STATE_BEFORE,
            "`zeroclaw estop {args:?}` changed the state file"
        );
    }
}

#[test]
fn estop_does_not_create_a_state_file_and_refuses_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("estop-state.json");
    write_config(dir.path(), &state_file, false);

    for args in FORMS {
        let output = run_estop(dir.path(), args);
        assert_refused(&output, args);
        assert!(
            !state_file.exists(),
            "`zeroclaw estop {args:?}` created the state file"
        );
    }
}
