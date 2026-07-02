//! Single source of truth for driving the real binary's `onboard-flow` walk
//! over a pty: build the binary once, feed scripted type-valid answers when
//! the terminal goes idle, capture the transcript, stop on `[completed`.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use zeroclaw_runtime::response_type::ResponseType;

use super::spec::{INSTANCE, LAYER, SECTION, matrix_spec};

/// The type-valid stub answer for a prompt kind, as typed at a terminal.
pub fn stub_answer(response_type: &ResponseType) -> String {
    match response_type {
        ResponseType::Secret => "sk-token".to_string(),
        ResponseType::YesNo => "y".to_string(),
        ResponseType::Number => "100".to_string(),
        ResponseType::Choice { options } => options[0].value.clone(),
        ResponseType::FreeformText => "https://walked.test".to_string(),
    }
}

/// Scripted terminal answers for a manual walk of the matrix section: the
/// locale choice first (it is the first prompt), then a stub per node in id
/// order.
pub fn scripted_answers() -> Vec<String> {
    let spec = matrix_spec();
    let mut ordered: Vec<_> = spec.nodes.values().collect();
    ordered.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    let mut answers = vec![
        zeroclaw_runtime::i18n::available_locales()
            .first()
            .expect("registry lists at least one locale")
            .code
            .clone(),
    ];
    answers.extend(
        ordered
            .into_iter()
            .map(|node| stub_answer(&node.prompt.response_type)),
    );
    answers
}

fn zeroclaw_binary() -> PathBuf {
    static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BINARY
        .get_or_init(|| {
            escargot::CargoBuild::new()
                .package("zeroclawlabs")
                .bin("zeroclaw")
                .run()
                .expect("build the zeroclaw binary from source for the test")
                .path()
                .to_path_buf()
        })
        .clone()
}

pub struct PtyDrive {
    pub completed: bool,
    pub transcript: String,
}

pub fn drive_flow(
    config_dir: &Path,
    extra_args: &[&str],
    answers: Vec<String>,
    deadline: Duration,
) -> PtyDrive {
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
    cmd.arg(LAYER);
    cmd.arg("--instance");
    cmd.arg(INSTANCE);
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
    let mut answers = answers.into_iter();

    let mut transcript = String::new();
    let deadline = Instant::now() + deadline;
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
    PtyDrive {
        completed,
        transcript,
    }
}
