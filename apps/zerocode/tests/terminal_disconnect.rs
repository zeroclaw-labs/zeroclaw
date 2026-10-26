#![cfg(unix)]

use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // A failed assertion or deadline must never leave the spin under test alive.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn terminal_poll_child() {
    let Some(ready) = std::env::var_os("ZEROCLAW_TEST_DEAD_TTY_READY") else {
        return;
    };
    crossterm::terminal::enable_raw_mode().expect("enable raw mode on owned PTY");
    // Bind the input reader to the live slave before the parent can close it.
    assert!(!crossterm::event::poll(Duration::ZERO).expect("initialize owned input reader"));
    std::fs::write(ready, b"ready").expect("announce input-poll readiness");
    let error = crossterm::event::poll(Duration::from_secs(10))
        .expect_err("closed terminal must return an error");
    assert!(
        error.kind() == std::io::ErrorKind::UnexpectedEof
            || matches!(
                error.raw_os_error(),
                Some(libc::EIO | libc::ENXIO | libc::EBADF)
            ),
        "unexpected disconnect error: {error}"
    );
}

fn open_pty() -> (File, File) {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: openpty initializes both output descriptors. Null optional pointers
    // request default settings; each successful descriptor is owned once below.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: successful openpty returned two distinct descriptors owned once.
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: both descriptors are live. Prevent the child inheriting a
        // spare master, which would keep its terminal open after our close.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    (master, slave)
}

#[test]
fn terminal_disconnect_returns_without_sighup_or_leaked_child() {
    let (mut master, slave) = open_pty();
    let fixture = tempfile::tempdir().expect("private readiness directory");
    let ready = fixture.path().join("ready");
    // No setsid/TIOCSCTTY: this PTY is not the child's controlling terminal,
    // so SIGHUP cannot mask an input-reader failure when its master disappears.
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "terminal_poll_child", "--nocapture"])
            .env("ZEROCLAW_TEST_DEAD_TTY_READY", &ready)
            .stdin(Stdio::from(slave))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn owned input-poll child"),
    );
    let start = Instant::now();
    while !ready.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "child readiness deadline"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child exited before polling"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    master
        .write_all(b"\x1b[")
        .expect("write partial ANSI sequence");
    std::thread::sleep(Duration::from_millis(50));
    drop(master);
    let disconnected = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait().expect("read owned child status") {
            assert!(status.success(), "input-poll child failed: {status}");
            break;
        }
        assert!(
            disconnected.elapsed() < Duration::from_secs(2),
            "terminal poll did not exit within two seconds of PTY disconnection"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn sigterm_exits_idle_zerocode_and_restores_connected_terminal() {
    use std::io::{BufRead, BufReader, Read};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};

    let fixture = tempfile::Builder::new()
        .prefix("zc-signal-")
        .tempdir_in("/tmp")
        .expect("private short socket directory");
    let socket = fixture.path().join("rpc.sock");
    let listener = UnixListener::bind(&socket).expect("bind private RPC fixture");
    listener.set_nonblocking(true).unwrap();
    let loop_started = AtomicBool::new(false);
    let (mut master, slave) = open_pty();
    let original = terminal_attributes(&slave);
    let size = libc::winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the owned slave descriptor and window-size pointer are live.
    assert_eq!(
        unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ, &size) },
        0
    );
    // SAFETY: the owned master is live; retain its current flags.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let stderr = File::create(fixture.path().join("stderr")).unwrap();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let start = Instant::now();
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(start.elapsed() < Duration::from_secs(5), "RPC connection deadline");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept private RPC connection: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            let mut writer = stream.try_clone().unwrap();
            for line in BufReader::new(stream).lines() {
                let request: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
                let Some(id) = request.get("id") else { continue };
                let method = request["method"].as_str().unwrap();
                let mut response = serde_json::json!({"jsonrpc": "2.0", "id": id});
                let result = match method {
                    "initialize" => Some(serde_json::json!({"server_version": env!("CARGO_PKG_VERSION")})),
                    "config/sections" => Some(serde_json::json!({"sections": []})),
                    "config/templates" => Some(serde_json::json!({"templates": []})),
                    "logs/subscribe" => Some(serde_json::Value::Null),
                    // Health is queried after pane initialization, inside the app loop.
                    "health" => {
                        loop_started.store(true, Ordering::Release);
                        None
                    }
                    _ => None,
                };
                if let Some(result) = result {
                    response["result"] = result;
                } else {
                    response["error"] = serde_json::json!({"code": -32601, "message": "terminal fixture: unavailable"});
                }
                writeln!(writer, "{response}").unwrap();
            }
        });
        let mut child = OwnedChild(
            Command::new(env!("CARGO_BIN_EXE_zerocode"))
                .args(["--config-dir", fixture.path().to_str().unwrap()])
                .env("ZEROCLAW_SOCKET", &socket)
                .env("TERM", "xterm-256color")
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stdout(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(stderr))
                .spawn()
                .expect("spawn owned production client"),
        );
        let mut output = Vec::new();
        let drain = |master: &mut File, output: &mut Vec<u8>| {
            let mut bytes = [0; 8192];
            loop {
                match master.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => output.extend_from_slice(&bytes[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("drain connected PTY: {error}"),
                }
            }
        };
        let contains = |output: &[u8], sequence: &[u8]| {
            output.windows(sequence.len()).any(|part| part == sequence)
        };
        let start = Instant::now();
        while !loop_started.load(Ordering::Acquire)
            || !contains(&output, b"\x1b[?1049h")
            || output.len() < 1000
        {
            drain(&mut master, &mut output);
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "client exited before idle loop"
            );
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "idle-loop readiness deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Drain throughout settling and shutdown; a full output buffer must not
        // disguise signal-selector starvation as a blocked terminal write.
        let settled = Instant::now();
        while settled.elapsed() < Duration::from_millis(300) {
            drain(&mut master, &mut output);
            std::thread::sleep(Duration::from_millis(10));
        }
        // SAFETY: this is the exact live child owned by this test, never a user process.
        assert_eq!(
            unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        let signalled = Instant::now();
        loop {
            drain(&mut master, &mut output);
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success(), "client failed after SIGTERM: {status}");
                break;
            }
            assert!(
                signalled.elapsed() < Duration::from_secs(2),
                "SIGTERM did not stop the idle client within two seconds"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drain(&mut master, &mut output);
        assert!(
            contains(&output, b"\x1b[?1049l"),
            "alternate screen not restored"
        );
        assert_eq!(
            terminal_attributes(&slave).c_lflag,
            original.c_lflag,
            "terminal local flags not restored"
        );
    });
}

fn terminal_attributes(terminal: &File) -> libc::termios {
    let mut attributes = std::mem::MaybeUninit::uninit();
    // SAFETY: the owned descriptor and output buffer are live; initialize before reading.
    assert_eq!(
        unsafe { libc::tcgetattr(terminal.as_raw_fd(), attributes.as_mut_ptr()) },
        0
    );
    unsafe { attributes.assume_init() }
}
