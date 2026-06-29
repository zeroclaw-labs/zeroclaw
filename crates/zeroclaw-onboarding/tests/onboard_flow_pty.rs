use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tempfile::TempDir;
use zeroclaw_config::schema::{Config, MatrixConfig};
use zeroclaw_onboarding::{Outcome, build_spec};
use zeroclaw_runtime::response_type::ResponseType;

const SECTION: &str = "channels.matrix.home";

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

struct DriveResult {
    completed: bool,
    transcript: String,
}

fn zeroclaw_binary() -> std::path::PathBuf {
    static BINARY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BINARY
        .get_or_init(|| {
            escargot::CargoBuild::new()
                .package("zeroclawlabs")
                .bin("zeroclaw")
                .run()
                .expect("build the zeroclaw binary for the onboarding pty test")
                .path()
                .to_path_buf()
        })
        .clone()
}

fn drive_flow_over_pty(
    config_dir: &std::path::Path,
    extra_args: &[&str],
    instance: &str,
) -> DriveResult {
    let binary = zeroclaw_binary();
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 200,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(binary);
    cmd.arg("--config-dir");
    cmd.arg(config_dir);
    cmd.arg("onboard-flow");
    cmd.arg("--section");
    cmd.arg(SECTION);
    cmd.arg("--layer");
    cmd.arg("channel");
    cmd.arg("--instance");
    cmd.arg(instance);
    for arg in extra_args {
        cmd.arg(arg);
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn child");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut writer = pair.master.take_writer().expect("take writer");
    let mut answers = scripted_answers().into_iter();
    assert!(
        answers.len() > 3,
        "matrix section should ask several fields"
    );

    let mut transcript = String::new();
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut idle_since = Instant::now();
    let mut completed = false;

    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                transcript.push_str(&String::from_utf8_lossy(&chunk));
                idle_since = Instant::now();
                if transcript.contains("[completed") {
                    completed = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if idle_since.elapsed() >= Duration::from_millis(200) {
                    if let Some(answer) = answers.next() {
                        writer
                            .write_all(format!("{answer}\n").as_bytes())
                            .expect("write answer");
                        writer.flush().expect("flush");
                        idle_since = Instant::now();
                    } else if transcript.contains("[completed") {
                        completed = true;
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.wait();
    DriveResult {
        completed,
        transcript,
    }
}

fn scripted_answers() -> Vec<String> {
    let mut config = Config::default();
    config
        .channels
        .matrix
        .insert("home".to_string(), MatrixConfig::default());
    let spec = build_spec(
        config.prop_fields(),
        SECTION,
        "channel",
        "home",
        Outcome::Cancelled,
    )
    .expect("matrix section yields a spec");

    let mut ordered: Vec<_> = spec.nodes.values().collect();
    ordered.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    ordered
        .into_iter()
        .map(|node| match &node.prompt.response_type {
            ResponseType::Secret => "sk-token".to_string(),
            ResponseType::YesNo => "y".to_string(),
            ResponseType::Number => "100".to_string(),
            ResponseType::Choice { options } => options[0].value.clone(),
            ResponseType::FreeformText => "https://walked.test".to_string(),
        })
        .collect()
}

#[test]
fn onboard_flow_walks_the_matrix_section_over_a_real_pty_and_writes_config() {
    let tmp = seeded_config_dir();
    let config_path = tmp.path().join("config.toml");

    let result = drive_flow_over_pty(tmp.path(), &[], "home");

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

    let result = drive_flow_over_pty(tmp.path(), &["--create"], "home");

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

    let result = drive_flow_over_pty(tmp.path(), &["--create", "--required-only"], "home");

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
