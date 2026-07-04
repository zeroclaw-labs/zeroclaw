#[path = "common/pty.rs"]
mod pty;
#[path = "common/spec.rs"]
mod spec;

use std::io::Write;
use std::time::Duration;

use pty::{PtyDrive, drive_flow, scripted_answers as base_answers};
use spec::{SECTION, completed_outcome, matrix_config, matrix_spec};
use tempfile::TempDir;
use zeroclaw_onboarding::append_peer_group_branch;
use zeroclaw_runtime::response_type::ResponseType;

fn seeded_config_dir() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let mut file = std::fs::File::create(tmp.path().join("config.toml")).unwrap();
    writeln!(
        file,
        "schema_version = 3\n\n[channels.matrix.home]\nenabled = false\nhomeserver = \"https://matrix.org\"\n"
    )
    .unwrap();
    tmp
}

fn bare_config_dir() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let mut file = std::fs::File::create(tmp.path().join("config.toml")).unwrap();
    writeln!(file, "schema_version = 3\n").unwrap();
    tmp
}

fn scripted_answers() -> Vec<String> {
    let mut answers = base_answers();
    assert!(
        answers.len() > 3,
        "matrix section should ask several fields"
    );
    answers.push(peer_group_skip_value());
    answers
}

fn peer_group_skip_value() -> String {
    let config = matrix_config();
    let branched =
        append_peer_group_branch(matrix_spec(), SECTION, "home", &config, completed_outcome());
    branched
        .nodes
        .values()
        .find_map(|node| match &node.prompt.response_type {
            ResponseType::Choice { options } => options
                .iter()
                .find(|option| option.value == "skip")
                .map(|option| option.value.clone()),
            _ => None,
        })
        .expect("peer-group decision exposes a skip option")
}

fn drive(config_dir: &std::path::Path, extra_args: &[&str]) -> PtyDrive {
    drive_flow(
        config_dir,
        extra_args,
        scripted_answers(),
        Duration::from_secs(90),
    )
}

#[test]
fn onboard_flow_walks_the_matrix_section_over_a_real_pty_and_writes_config() {
    let tmp = seeded_config_dir();
    let config_path = tmp.path().join("config.toml");

    let result = drive(tmp.path(), &[]);

    assert!(
        result.completed,
        "flow did not reach a completed outcome.\ntranscript:\n{}",
        result.transcript
    );

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("https://walked.test"),
        "freeform answer should have been written, got:\n{written}"
    );
}

#[test]
fn onboard_flow_create_inserts_a_new_alias_over_a_real_pty_and_writes_config() {
    let tmp = bare_config_dir();
    let config_path = tmp.path().join("config.toml");

    let before = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !before.contains("[channels.matrix.home]"),
        "precondition: the alias must be absent before the flow"
    );

    let result = drive(tmp.path(), &["--create"]);

    assert!(
        result.completed,
        "create flow did not reach a completed outcome.\ntranscript:\n{}",
        result.transcript
    );

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("[channels.matrix.home]"),
        "the brand-new alias block should have been written, got:\n{written}"
    );
    assert!(
        written.contains("https://walked.test"),
        "the walked answer should have been written into the new alias, got:\n{written}"
    );
}

#[test]
fn onboard_flow_required_only_create_omits_optional_fields_on_disk() {
    let tmp = bare_config_dir();
    let config_path = tmp.path().join("config.toml");

    let result = drive(tmp.path(), &["--create", "--required-only"]);

    assert!(
        result.completed,
        "required-only create flow did not complete.\ntranscript:\n{}",
        result.transcript
    );

    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("[channels.matrix.home]"),
        "the new alias block should have been written, got:\n{written}"
    );
    assert!(
        written.contains("https://walked.test"),
        "a required field should have been walked, got:\n{written}"
    );
    assert!(
        !written.contains("access_token"),
        "an Option field must not be asked or written in required-only mode, got:\n{written}"
    );
}
