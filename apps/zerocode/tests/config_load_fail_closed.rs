#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const PLACEHOLDER_TOKEN: &str = "redacted-test-token-12330";
const REMOTE_URI: &str = "wss://127.0.0.1:9/rpc";

struct SocketProbe {
    hits: Arc<AtomicUsize>,
    join: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicUsize>,
}

impl Drop for SocketProbe {
    fn drop(&mut self) {
        self.stop.store(1, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn valid_remote_config() -> String {
    format!("[connection.wss]\nuri = \"{REMOTE_URI}\"\nauth_token = \"{PLACEHOLDER_TOKEN}\"\n")
}

fn listen_for_local_socket(socket: &Path) -> SocketProbe {
    let _ = std::fs::remove_file(socket);
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).expect("create data dir for socket probe");
    }
    let listener = UnixListener::bind(socket).expect("bind local daemon socket probe");
    listener
        .set_nonblocking(true)
        .expect("nonblocking socket probe");
    let hits = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicUsize::new(0));
    let thread_hits = Arc::clone(&hits);
    let thread_stop = Arc::clone(&stop);
    let join = std::thread::spawn(move || {
        while thread_stop.load(Ordering::Acquire) == 0 {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    thread_hits.fetch_add(1, Ordering::Release);
                    let mut buf = [0_u8; 256];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(b"{}\n");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    SocketProbe {
        hits,
        join: Some(join),
        stop,
    }
}

fn run_zerocode(config_dir: &Path) -> Output {
    let child = Command::new(env!("CARGO_BIN_EXE_zerocode"))
        .args([
            "--config-dir",
            config_dir.to_str().expect("utf-8 config dir"),
        ])
        .env_remove("ZEROCLAW_SOCKET")
        .env_remove("ZEROCODE_CONFIG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn zerocode");
    let pid = child.id();
    let handle = std::thread::spawn(move || child.wait_with_output());
    let start = Instant::now();
    let timeout = Duration::from_secs(15);
    while !handle.is_finished() {
        if start.elapsed() > timeout {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            let _ = handle.join();
            panic!("zerocode did not finish within {timeout:?} (killed pid {pid})");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    handle
        .join()
        .expect("join zerocode waiter")
        .expect("wait for zerocode")
}

fn assert_fail_closed(output: &Output, probe: &SocketProbe, config_path: &Path) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "zerocode must exit non-zero on a broken config\nstatus={:?}\nstdout={stdout}\nstderr={stderr}",
        output.status
    );
    assert!(
        stderr.contains("config load failed"),
        "stderr must name the load failure\nstderr={stderr}"
    );
    assert!(
        stderr.contains(&config_path.display().to_string())
            || stderr.contains("zerocode-config.toml"),
        "stderr must name the config file\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("starting with defaults"),
        "must not fall back to defaults\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("failed to spawn daemon"),
        "must not attempt to spawn a local daemon\nstderr={stderr}"
    );
    assert_eq!(
        probe.hits.load(Ordering::Acquire),
        0,
        "must not connect to the local daemon socket\nstderr={stderr}"
    );
    assert!(
        !stdout.contains(PLACEHOLDER_TOKEN) && !stderr.contains(PLACEHOLDER_TOKEN),
        "must not print the saved credential\nstdout={stdout}\nstderr={stderr}"
    );
}

#[test]
fn unparseable_config_exits_before_local_socket_or_daemon_spawn() {
    let dir = tempfile::tempdir().expect("temp config dir");
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config_path = dir.path().join("zerocode-config.toml");
    let mut body = valid_remote_config();
    body.push_str(&format!("\n[connection.logins.\"{REMOTE_URI}\".tls\n"));
    std::fs::write(&config_path, body).unwrap();

    let socket = dir.path().join("data").join("daemon.sock");
    let probe = listen_for_local_socket(&socket);
    let output = run_zerocode(dir.path());
    assert_fail_closed(&output, &probe, &config_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("parse") || stderr.contains("parsing"),
        "unparseable file should report a parse error\nstderr={stderr}"
    );
}

#[test]
fn unreadable_config_directory_exits_before_local_socket_or_daemon_spawn() {
    let dir = tempfile::tempdir().expect("temp config dir");
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let config_path = dir.path().join("zerocode-config.toml");
    // A directory at the config path is a durable read error. chmod 000 on a
    // regular file is not: load_persisted restores owner mode to 0600 first.
    std::fs::create_dir(&config_path).unwrap();

    let socket = dir.path().join("data").join("daemon.sock");
    let probe = listen_for_local_socket(&socket);
    let output = run_zerocode(dir.path());
    assert_fail_closed(&output, &probe, &config_path);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reading") || stderr.contains("Is a directory"),
        "unreadable path should report a read error\nstderr={stderr}"
    );

    // Restore traverse bits so TempDir cleanup can remove the directory
    // `restrict_to_owner` may have rewritten to 0600 while trying to read it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o700));
    }
}
