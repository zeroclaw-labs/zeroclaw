//! `zeroclaw config set`, `config patch` and `config init` against a running
//! daemon.
//!
//! An edit to the authorization inputs (`[users]`, `[permission_profiles]`,
//! `[oidc]`, `security.trust_daemon_uid`) is committed through the daemon's
//! RPC config methods, even when it re-asserts the value already in the
//! file: the daemon validates it, saves `config.toml` itself, and publishes
//! the new policy, which established connections pick up at their next
//! operation. When the daemon refuses the caller at the handshake, binding no
//! principal, is not the one serving the CLI's config dir, or cannot take the
//! write, or when no heartbeat confirms a daemon but another process holds
//! the config dir's ownership lock, the CLI saves the file directly and
//! reports the edit as pending a reload, unless the policy it would leave
//! there stops compiling. When the daemon binds a principal and refuses it
//! the edit, or rejects the edit as invalid, nothing is saved. Edits outside
//! those sections, and every edit made while no daemon serves the CLI's
//! config dir, keep the direct save and today's output, and contact no
//! daemon.
//!
//! Every test drives real processes: a `zeroclaw daemon` on a private config
//! dir and port, the CLI in a separate process, and a raw JSON-RPC client on
//! the daemon's local socket. The fixture maps this process's uid to
//! `[users.me]` (or, to have the daemon refuse this process at the
//! handshake, another uid) and sets `security.trust_daemon_uid = false`. With
//! the default `true`, the daemon's own uid is the shared operator whatever
//! the roster says, so a narrowed profile could never be observed.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use zeroclaw_api::jsonrpc::error_codes::{AUTH_REQUIRED, FORBIDDEN};
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_runtime::live_config_authority::ConfigOwnershipGuard;
use zeroclaw_runtime::rpc::dispatch::RPC_PROTOCOL_VERSION;

/// Bound on daemon startup, until both `/health` and the local socket serve.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on one CLI run, so a CLI that blocks on the daemon fails the test
/// instead of hanging it.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on each read and write on a probe connection.
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// A private config dir whose roster maps a uid, usually this process's, to
/// `users.me`.
struct Fixture {
    dir: tempfile::TempDir,
    /// The uid `users.me` maps.
    uid: u32,
    port: u16,
}

impl Fixture {
    /// `users.me` maps this process's uid and holds the `me_profile`
    /// permission profile: `admin` may do anything, `reader` may read config
    /// but not write it, and a profile the file does not define leaves a
    /// policy that does not compile.
    fn new(me_profile: &str) -> Self {
        Self::with_roster_uid(me_profile, peer_uid())
    }

    /// `users.me` maps a uid other than this process's, so the daemon binds
    /// no principal to this process and refuses it at `initialize`.
    fn unmapped(me_profile: &str) -> Self {
        let uid = if peer_uid() == 4242 { 4243 } else { 4242 };
        Self::with_roster_uid(me_profile, uid)
    }

    fn with_roster_uid(me_profile: &str, uid: u32) -> Self {
        // A short root keeps `<dir>/data/daemon.sock` inside the 104-byte
        // `sun_path` limit on macOS whatever `TMPDIR` is.
        let dir = tempfile::Builder::new()
            .prefix("zc")
            .tempdir_in("/tmp")
            .expect("create a temp config dir under /tmp");
        let port = free_port();
        let version = zeroclaw_config::migration::CURRENT_SCHEMA_VERSION;
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                r#"schema_version = {version}

[gateway]
host = "127.0.0.1"
port = {port}
require_pairing = false

[security]
trust_daemon_uid = false

[permission_profiles.admin]
admin = true

[permission_profiles.reader]
grants = {{ sessions = ["read"], config = ["read"] }}

[users.me]
uid = {uid}
permission_profiles = ["{me_profile}"]
"#
            ),
        )
        .expect("write the fixture config.toml");
        Self { dir, uid, port }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `data_dir`, which defaults to `<config_dir>/data`: the daemon's local
    /// RPC socket and the config dir's ownership lock live there.
    fn data_dir(&self) -> PathBuf {
        self.path().join("data")
    }

    /// The daemon's local RPC socket.
    fn socket(&self) -> PathBuf {
        self.data_dir().join("daemon.sock")
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.path().join("config.toml")).expect("read config.toml")
    }

    /// Replace `config.toml` by hand, as an operator editing the file while
    /// the daemon runs would.
    fn write_config_text(&self, text: &str) {
        std::fs::write(self.path().join("config.toml"), text).expect("write config.toml");
    }

    /// Whether the daemon heartbeat beside `config.toml` names `pid`: the CLI
    /// hands an edit only to the daemon its config dir records.
    fn heartbeat_names(&self, pid: u32) -> bool {
        std::fs::read_to_string(self.path().join("state").join("daemon_state.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|state| state.get("pid").and_then(Value::as_u64))
            == Some(u64::from(pid))
    }

    /// `config.toml` parsed as plain TOML, so assertions do not depend on the
    /// `Config` schema.
    fn config_toml(&self) -> toml::Value {
        let text = self.config_text();
        toml::from_str(&text)
            .unwrap_or_else(|error| panic!("config.toml does not parse: {error}\n{text}"))
    }

    /// A uid for a second roster entry that cannot collide with `users.me`.
    fn other_uid(&self) -> u32 {
        if self.uid == 4242 { 4243 } else { 4242 }
    }
}

/// The uid the kernel reports for this process as a Unix-socket peer (the
/// effective uid), which `[users.me].uid` has to map. The CLI runs as the
/// same uid, so the one entry covers the probe and the CLI.
fn peer_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .expect("reserve a free loopback port")
}

/// A `zeroclaw` command bound to the fixture's config dir that inherits no
/// `ZEROCLAW_*` variable from the invoking shell. An inherited
/// `ZEROCLAW_SOCKET` would point the CLI at another daemon's socket (the test
/// about that sets it itself); env overrides would rewrite fixture values.
fn zeroclaw_command(config_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ZEROCLAW_") {
            command.env_remove(key);
        }
    }
    command
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .env("LC_ALL", "C")
        .env("TERM", "dumb");
    command
}

/// A `zeroclaw daemon` serving one fixture. Dropping it kills and reaps the
/// process, so a failed assertion never leaks a daemon.
struct Daemon {
    child: Child,
    socket: PathBuf,
    log: PathBuf,
}

/// What the daemon's `initialize` answers this process once it serves.
#[derive(Clone, Copy)]
enum Handshake {
    /// It binds the roster principal `users.me`.
    BindsMe,
    /// It binds no principal and refuses this process with this code.
    Refuses(i32),
}

impl Daemon {
    /// A daemon whose `initialize` binds this process to `users.me`.
    fn start(fixture: &Fixture) -> Self {
        Self::start_expecting(fixture, Handshake::BindsMe)
    }

    fn start_expecting(fixture: &Fixture, handshake: Handshake) -> Self {
        // A file rather than a pipe: nothing drains a pipe while the test
        // runs, and a full one would stall the daemon mid-test.
        let log = fixture.path().join("daemon.log");
        let stdout = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .expect("create the daemon log");
        let stderr = stdout.try_clone().expect("share the daemon log");
        let child = zeroclaw_command(fixture.path())
            .arg("--config-dir")
            .arg(fixture.path())
            .args(["daemon", "--host", "127.0.0.1", "--port"])
            .arg(fixture.port.to_string())
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("spawn zeroclaw daemon");
        let mut daemon = Self {
            child,
            socket: fixture.socket(),
            log,
        };
        daemon.wait_until_serving(fixture, handshake);
        daemon
    }

    /// Wait until the gateway answers `/health`, the heartbeat names this
    /// daemon, and the local socket answers `initialize` as `expected` says.
    fn wait_until_serving(&mut self, fixture: &Fixture, expected: Handshake) {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll the daemon") {
                panic!(
                    "the daemon exited during startup ({status})\n{}",
                    self.output()
                );
            }
            if health_ok(fixture.port)
                && fixture.heartbeat_names(self.child.id())
                && let Ok(mut probe) = RpcProbe::connect(&self.socket)
            {
                let handshake = probe.initialize();
                match expected {
                    Handshake::BindsMe => self.assert_bound_to_me(handshake),
                    Handshake::Refuses(code) => self.assert_refused(handshake, code),
                }
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon did not serve /health, its heartbeat and {} within {STARTUP_TIMEOUT:?}\n{}",
                self.socket.display(),
                self.output()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A new connection, bound through `initialize` as `users.me`.
    fn probe(&self) -> RpcProbe {
        let mut probe = RpcProbe::connect(&self.socket)
            .unwrap_or_else(|error| panic!("connect {}: {error}", self.socket.display()));
        let handshake = probe.initialize();
        self.assert_bound_to_me(handshake);
        probe
    }

    /// Every connection here must resolve through the roster entry, never the
    /// shared operator: the roster is what decides this uid's grants.
    fn assert_bound_to_me(&self, handshake: Result<Value, Value>) {
        let result = handshake.unwrap_or_else(|error| {
            panic!(
                "initialize was refused for users.me: {error}\n{}",
                self.output()
            )
        });
        assert_eq!(
            result.get("principal_id").and_then(Value::as_str),
            Some(PrincipalId::for_roster("me").as_str()),
            "initialize must bind the roster principal users.me: {result}\n{}",
            self.output()
        );
    }

    /// A daemon that maps no principal to this process, or enforces a
    /// deny-all policy, refuses it at `initialize` with `code`.
    fn assert_refused(&self, handshake: Result<Value, Value>, code: i32) {
        match handshake {
            Err(error) => assert_eq!(
                error.get("code").and_then(Value::as_i64),
                Some(i64::from(code)),
                "initialize must refuse this process with {code}: {error}\n{}",
                self.output()
            ),
            Ok(result) => panic!(
                "initialize must refuse this process with {code}, but it bound {result}\n{}",
                self.output()
            ),
        }
    }

    /// Everything the daemon printed so far, for failure messages.
    fn output(&self) -> String {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        format!("daemon output:\n{text}")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Whether the gateway answers `GET /health` with 200.
fn health_ok(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let timeout = Some(Duration::from_secs(2));
    if stream.set_read_timeout(timeout).is_err()
        || stream.set_write_timeout(timeout).is_err()
        || stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .is_err()
    {
        return false;
    }
    let mut head = [0_u8; 64];
    let Ok(read) = stream.read(&mut head) else {
        return false;
    };
    std::str::from_utf8(&head[..read])
        .ok()
        .and_then(|text| text.split_whitespace().nth(1))
        == Some("200")
}

/// A raw JSON-RPC 2.0 client on the daemon's local socket, one
/// newline-delimited frame per message. The connection stays open for the
/// probe's lifetime, so a test can observe how an established binding behaves
/// after the policy changes underneath it.
struct RpcProbe {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl RpcProbe {
    fn connect(socket: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(RPC_TIMEOUT))?;
        stream.set_write_timeout(Some(RPC_TIMEOUT))?;
        Ok(Self {
            writer: stream.try_clone()?,
            reader: BufReader::new(stream),
            next_id: 1,
        })
    }

    /// Send one request and return its `result` (`Ok`) or its `error` object
    /// (`Err`). Frames with any other id, such as notifications, are skipped.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut frame =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        frame.push('\n');
        self.writer
            .write_all(frame.as_bytes())
            .unwrap_or_else(|error| panic!("send {method}: {error}"));
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .unwrap_or_else(|error| panic!("read the {method} response: {error}"));
            assert!(
                read > 0,
                "the daemon closed the connection instead of answering {method}"
            );
            if line.trim().is_empty() {
                continue;
            }
            let reply: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
                panic!("undecodable frame while awaiting {method}: {error}: {line}")
            });
            if reply.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            return match reply.get("error") {
                Some(error) => Err(error.clone()),
                None => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
            };
        }
    }

    fn initialize(&mut self) -> Result<Value, Value> {
        self.call(
            "initialize",
            json!({"protocol_version": RPC_PROTOCOL_VERSION}),
        )
    }

    fn config_get(&mut self, prop: &str) -> Result<Value, Value> {
        self.call("config/get", json!({"prop": prop}))
    }

    fn config_set(&mut self, prop: &str, value: &str) -> Result<Value, Value> {
        self.call("config/set", json!({"prop": prop, "value": value}))
    }
}

fn assert_forbidden(outcome: Result<Value, Value>, what: &str) {
    match outcome {
        Err(error) => assert_eq!(
            error.get("code").and_then(Value::as_i64),
            Some(i64::from(FORBIDDEN)),
            "{what}: expected FORBIDDEN, got {error}"
        ),
        Ok(result) => panic!("{what}: expected FORBIDDEN, but the call succeeded with {result}"),
    }
}

/// Run the CLI against the fixture's config dir, selected through
/// `ZEROCLAW_CONFIG_DIR` as in the other CLI component tests.
fn run_cli(fixture: &Fixture, args: &[&str], input: Option<&[u8]>) -> Output {
    run(zeroclaw_command(fixture.path()), args, input)
}

/// `run_cli` with `ZEROCLAW_SOCKET` naming `socket`, as in a shell that
/// exported it for another daemon.
fn run_cli_with_socket(fixture: &Fixture, socket: &Path, args: &[&str]) -> Output {
    let mut command = zeroclaw_command(fixture.path());
    command.env("ZEROCLAW_SOCKET", socket);
    run(command, args, None)
}

fn run(mut command: Command, args: &[&str], input: Option<&[u8]>) -> Output {
    let command_line = args.join(" ");
    let mut child = command
        .env("RUST_LOG", "off")
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn `zeroclaw {command_line}`: {error}"));
    if let Some(input) = input {
        // The handle drops at the end of this statement, which closes the
        // pipe so the CLI sees end of input.
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(input)
            .unwrap_or_else(|error| panic!("write stdin of `zeroclaw {command_line}`: {error}"));
    }
    let deadline = Instant::now() + CLI_TIMEOUT;
    while child.try_wait().expect("poll the CLI").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().expect("reap the timed-out CLI");
            panic!(
                "`zeroclaw {command_line}` did not exit within {CLI_TIMEOUT:?}\n{}",
                describe(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("collect `zeroclaw {command_line}` output: {error}"))
}

fn config_set_json(fixture: &Fixture, path: &str, value: &str) -> Output {
    run_cli(
        fixture,
        &["config", "set", "--no-interactive", path, value, "--json"],
        None,
    )
}

fn config_patch_json(fixture: &Fixture, patch: &Value) -> Output {
    run_cli(
        fixture,
        &["config", "patch", "--json", "-"],
        Some(patch.to_string().as_bytes()),
    )
}

/// In JSON mode the envelope's `daemon` member carries the outcome, so
/// nothing else may reach stderr.
fn assert_clean_stderr(output: &Output) {
    assert!(
        output.stderr.is_empty(),
        "JSON output leaves stderr to machine consumers\n{}",
        describe(output)
    );
}

fn describe(output: &Output) -> String {
    format!(
        "{}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The JSON envelope the CLI printed on stdout.
fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not a JSON envelope: {error}\n{}",
            describe(output)
        )
    })
}

/// The value at a dotted path in a parsed TOML document.
fn toml_at<'a>(doc: &'a toml::Value, path: &str) -> Option<&'a toml::Value> {
    path.split('.').try_fold(doc, |value, key| value.get(key))
}

fn string_array(items: &[&str]) -> toml::Value {
    toml::Value::Array(items.iter().map(|item| toml::Value::from(*item)).collect())
}

#[test]
fn config_set_narrowing_binds_an_established_connection() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);
    let mut established = daemon.probe();
    established
        .config_set("gateway.host", "127.0.0.9")
        .unwrap_or_else(|error| panic!("an admin principal must be able to write config: {error}"));

    let output = config_set_json(&fixture, "users.me.permission_profiles", "reader");
    assert!(
        output.status.success(),
        "a narrowing the daemon accepts must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(
        envelope["path"], "users.me.permission_profiles",
        "{envelope}"
    );
    assert_eq!(
        envelope["daemon"]["applied"], true,
        "the daemon must have committed the edit: {envelope}"
    );
    assert_clean_stderr(&output);

    // The established connection re-resolves at its next operation and now
    // holds only the reader's grants: config writes are refused, reads work.
    assert_forbidden(
        established.config_set("gateway.host", "127.0.0.9"),
        "the established connection after the narrowing",
    );
    let live = established
        .config_get("users.me.permission_profiles")
        .unwrap_or_else(|error| panic!("the reader profile keeps config read: {error}"));
    assert!(
        live["value"]
            .as_str()
            .is_some_and(|value| value.contains("reader")),
        "the daemon's live roster must carry the narrowed profile: {live}"
    );

    // A new connection binds the same roster principal under the new policy.
    let mut fresh = daemon.probe();
    assert_forbidden(
        fresh.config_set("gateway.host", "127.0.0.9"),
        "a new connection after the narrowing",
    );

    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the daemon must have saved the narrowed roster:\n{}",
        fixture.config_text()
    );
}

/// A `--comment` on an edit the daemon applies travels with the edit: the
/// daemon writes it under the lock it saved under, and the CLI leaves
/// config.toml alone afterwards.
#[test]
fn config_set_comment_on_an_applied_edit_is_written_by_the_daemon() {
    let fixture = Fixture::new("admin");
    let _daemon = Daemon::start(&fixture);

    let output = run_cli(
        &fixture,
        &[
            "config",
            "set",
            "--no-interactive",
            "users.me.permission_profiles",
            "reader",
            "--comment",
            "narrowed pending audit",
            "--json",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "a commented narrowing the daemon accepts must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(
        envelope["daemon"]["applied"], true,
        "the daemon must have committed the edit: {envelope}"
    );
    assert_clean_stderr(&output);

    let text = fixture.config_text();
    assert_eq!(
        text.matches("narrowed pending audit").count(),
        1,
        "config.toml must carry the comment exactly once:\n{text}"
    );
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the comment must not disturb the saved value:\n{text}"
    );
}

/// The security regression: a principal the daemon identified and refused an
/// edit must move neither the running policy nor the file the daemon's next
/// reload would install.
#[test]
fn config_set_refused_to_the_bound_principal_saves_nothing() {
    let fixture = Fixture::new("reader");
    let daemon = Daemon::start(&fixture);
    let mut established = daemon.probe();
    assert_forbidden(
        established.config_set("gateway.host", "127.0.0.9"),
        "a reader before the edit",
    );
    let before = fixture.config_text();

    // The reader tries to grant itself admin. The daemon binds users.me at
    // initialize and then refuses it the edit, so the command fails.
    let output = config_set_json(&fixture, "permission_profiles.reader.admin", "true");
    assert!(
        !output.status.success(),
        "an edit the daemon refused the bound principal must fail\n{}",
        describe(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "a refused edit prints no envelope\n{}",
        describe(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("identified you and refused this edit") && stderr.contains("not granted"),
        "stderr must carry the refusal and the daemon's reason\n{}",
        describe(&output)
    );
    assert_eq!(
        fixture.config_text(),
        before,
        "a refused edit must leave config.toml byte-identical"
    );

    // Without an envelope, the outcome is the same.
    let human = run_cli(
        &fixture,
        &[
            "config",
            "set",
            "--no-interactive",
            "permission_profiles.reader.admin",
            "true",
        ],
        None,
    );
    assert!(
        !human.status.success(),
        "an edit the daemon refused the bound principal must fail\n{}",
        describe(&human)
    );
    assert!(
        String::from_utf8_lossy(&human.stderr).contains("identified you and refused this edit"),
        "stderr must carry the refusal in human mode\n{}",
        describe(&human)
    );
    assert_eq!(
        fixture.config_text(),
        before,
        "a refused edit must leave config.toml byte-identical"
    );

    // The daemon's policy did not move: the refused caller gained nothing on
    // its established connection or on a new one.
    assert_forbidden(
        established.config_set("gateway.host", "127.0.0.9"),
        "the established connection after the refused edit",
    );
    let live = established
        .config_get("permission_profiles.reader.admin")
        .unwrap_or_else(|error| panic!("the reader profile keeps config read: {error}"));
    assert_eq!(
        live["value"], "false",
        "the daemon's live profile must be unchanged: {live}"
    );
    let mut fresh = daemon.probe();
    assert_forbidden(
        fresh.config_set("gateway.host", "127.0.0.9"),
        "a new connection after the refused edit",
    );
}

/// A daemon whose roster does not map this process binds it no principal and
/// refuses it at the handshake. That refusal is no verdict on the edit, so
/// the CLI saves the file directly and reports the edit as pending a reload.
/// This process is no principal of the daemon, so no probe can read its live
/// policy.
#[test]
fn config_set_refused_at_the_handshake_is_saved_and_pending() {
    let fixture = Fixture::unmapped("reader");
    let _daemon = Daemon::start_expecting(&fixture, Handshake::Refuses(AUTH_REQUIRED));

    let output = config_set_json(&fixture, "users.me.permission_profiles", "admin");
    assert!(
        output.status.success(),
        "an edit refused at the handshake falls back to the direct save\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(
        envelope["path"], "users.me.permission_profiles",
        "{envelope}"
    );
    assert_eq!(envelope["daemon"]["applied"], false, "{envelope}");
    assert_eq!(envelope["daemon"]["pending_reload"], true, "{envelope}");
    assert_eq!(envelope["daemon"]["reason"], "refused", "{envelope}");
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["admin"])),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );

    // Without an envelope, the same outcome is a notice on stderr.
    let human = run_cli(
        &fixture,
        &[
            "config",
            "set",
            "--no-interactive",
            "users.me.permission_profiles",
            "reader",
        ],
        None,
    );
    assert!(
        human.status.success(),
        "an edit refused at the handshake falls back to the direct save\n{}",
        describe(&human)
    );
    let notice = String::from_utf8_lossy(&human.stderr);
    assert!(
        notice.contains("still enforces the previous authorization policy")
            && notice.contains("refused this caller"),
        "a pending edit prints its notice on stderr in human mode\n{}",
        describe(&human)
    );
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );
}

/// A daemon whose authorization sections do not compile enforces deny-all
/// and refuses every caller at the handshake, so the file is the only way to
/// repair it: the CLI saves the edit and reports it as pending. The policy
/// did not compile before the edit, so the compile guard lets it through.
#[test]
fn config_set_repairs_a_deny_all_daemon_through_the_file() {
    // `users.me` names a permission profile the file does not define.
    let fixture = Fixture::new("missing");
    let _daemon = Daemon::start_expecting(&fixture, Handshake::Refuses(FORBIDDEN));

    let output = config_set_json(&fixture, "users.me.permission_profiles", "admin");
    assert!(
        output.status.success(),
        "a repair of a deny-all daemon falls back to the direct save\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["daemon"]["applied"], false, "{envelope}");
    assert_eq!(envelope["daemon"]["pending_reload"], true, "{envelope}");
    assert_eq!(envelope["daemon"]["reason"], "refused", "{envelope}");
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["admin"])),
        "the CLI must have saved the repair:\n{}",
        fixture.config_text()
    );
}

#[test]
fn config_set_rejected_by_the_daemon_saves_nothing() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);
    let before = fixture.config_text();

    // A roster entry with a uid but no permission profile can never
    // authenticate, so the daemon rejects the policy it would compile.
    let uid = fixture.other_uid().to_string();
    let output = config_set_json(&fixture, "users.bob.uid", &uid);
    assert!(
        !output.status.success(),
        "an edit the daemon rejects must fail\n{}",
        describe(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Authorization config rejected"),
        "stderr must carry the daemon's reason\n{}",
        describe(&output)
    );
    assert!(
        toml_at(&fixture.config_toml(), "users.bob").is_none(),
        "a rejected edit must not reach config.toml:\n{}",
        fixture.config_text()
    );
    assert_eq!(
        fixture.config_text(),
        before,
        "a rejected edit must leave config.toml untouched"
    );

    let mut probe = daemon.probe();
    if let Ok(live) = probe.config_get("users.bob.uid") {
        panic!("the daemon's live config must not gain users.bob: {live}");
    }
}

#[test]
fn config_patch_creates_a_roster_entry_live() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);
    let uid = fixture.other_uid();

    // Neither field alone is a valid roster entry, so both must commit as one
    // batch.
    let patch = json!([
        {"op": "add", "path": "/users/bob/uid", "value": uid},
        {"op": "add", "path": "/users/bob/permission_profiles", "value": ["reader"]}
    ]);
    let output = config_patch_json(&fixture, &patch);
    assert!(
        output.status.success(),
        "a batch the daemon accepts must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["saved"], true, "{envelope}");
    assert_eq!(
        envelope["daemon"]["applied"], true,
        "the daemon must have committed the batch: {envelope}"
    );
    assert_eq!(
        envelope["results"].as_array().map(Vec::len),
        Some(2),
        "{envelope}"
    );

    let mut probe = daemon.probe();
    let live = probe
        .config_get("users.bob.uid")
        .unwrap_or_else(|error| panic!("the daemon's live config must carry users.bob: {error}"));
    assert!(
        live["value"]
            .as_str()
            .is_some_and(|value| value.contains(&uid.to_string())),
        "the daemon's live users.bob.uid must be {uid}: {live}"
    );

    let saved = fixture.config_toml();
    assert_eq!(
        toml_at(&saved, "users.bob.uid").and_then(toml::Value::as_integer),
        Some(i64::from(uid)),
        "the daemon must have saved users.bob:\n{}",
        fixture.config_text()
    );
    assert_eq!(
        toml_at(&saved, "users.bob.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the daemon must have saved users.bob:\n{}",
        fixture.config_text()
    );
}

#[test]
fn config_set_outside_authorization_never_contacts_the_daemon() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);

    let output = config_set_json(&fixture, "gateway.host", "127.0.0.2");
    assert!(
        output.status.success(),
        "a non-authorization edit must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["path"], "gateway.host", "{envelope}");
    assert!(
        envelope.get("daemon").is_none(),
        "an edit outside the authorization inputs must not involve the daemon: {envelope}"
    );

    // The file moved; the daemon's live copy did not.
    assert_eq!(
        toml_at(&fixture.config_toml(), "gateway.host").and_then(toml::Value::as_str),
        Some("127.0.0.2"),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );
    let mut probe = daemon.probe();
    let live = probe
        .config_get("gateway.host")
        .unwrap_or_else(|error| panic!("an admin principal must be able to read config: {error}"));
    assert_eq!(
        live["value"], "127.0.0.1",
        "the daemon's live config must be untouched: {live}"
    );
}

#[test]
fn config_set_without_a_daemon_is_unchanged() {
    let fixture = Fixture::new("admin");
    assert!(
        !fixture.socket().exists(),
        "no daemon may serve this fixture"
    );

    let output = config_set_json(&fixture, "users.me.permission_profiles", "reader");
    assert!(
        output.status.success(),
        "with no daemon the direct save must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    let keys: BTreeSet<&str> = envelope
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        keys,
        BTreeSet::from(["path", "value"]),
        "with no daemon the envelope must be exactly today's, with no daemon member: {envelope}"
    );
    assert_eq!(
        envelope["path"], "users.me.permission_profiles",
        "{envelope}"
    );
    assert!(
        output.stderr.is_empty(),
        "with no daemon nothing is added to the output\n{}",
        describe(&output)
    );
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );
}

/// The live `users.me.permission_profiles` as a probe reads it.
fn live_profiles_of_me(probe: &mut RpcProbe) -> String {
    let live = probe
        .config_get("users.me.permission_profiles")
        .unwrap_or_else(|error| panic!("config read is granted to users.me: {error}"));
    live["value"]
        .as_str()
        .unwrap_or_else(|| panic!("config/get answers a string value: {live}"))
        .to_owned()
}

/// A `ZEROCLAW_SOCKET` exported for one daemon must not carry another config
/// dir's edit to it: the daemon that answers is not the one the config dir
/// records, so the CLI sends it nothing, saves its own file and reports the
/// edit as pending.
#[test]
fn config_set_refuses_to_publish_through_another_daemon() {
    let fixture_a = Fixture::new("admin");
    let daemon_a = Daemon::start(&fixture_a);
    let fixture_b = Fixture::new("admin");
    let _daemon_b = Daemon::start(&fixture_b);
    let before_a = fixture_a.config_text();

    let output = run_cli_with_socket(
        &fixture_b,
        &fixture_a.socket(),
        &[
            "config",
            "set",
            "--no-interactive",
            "users.me.permission_profiles",
            "reader",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "an edit no daemon of this config dir could take falls back to the direct save\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["daemon"]["applied"], false, "{envelope}");
    assert_eq!(envelope["daemon"]["pending_reload"], true, "{envelope}");
    assert_eq!(envelope["daemon"]["reason"], "other_daemon", "{envelope}");
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture_b.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved its own config.toml:\n{}",
        fixture_b.config_text()
    );

    // The other daemon received nothing: its live roster, its grants and its
    // file are as they were.
    let mut probe = daemon_a.probe();
    let live = live_profiles_of_me(&mut probe);
    assert!(
        live.contains("admin") && !live.contains("reader"),
        "the other daemon's live roster must be unchanged: {live}"
    );
    probe
        .config_set("gateway.host", "127.0.0.9")
        .unwrap_or_else(|error| panic!("the other daemon must still grant admin: {error}"));
    assert_eq!(
        toml_at(
            &toml::from_str(&before_a).expect("the fixture parses"),
            "users.me.permission_profiles"
        ),
        toml_at(&fixture_a.config_toml(), "users.me.permission_profiles"),
        "the other daemon's config.toml must keep its roster:\n{}",
        fixture_a.config_text()
    );
}

/// With no daemon serving the CLI's config dir, the edit keeps the direct
/// save and today's output. A `ZEROCLAW_SOCKET` exported for another
/// config dir's daemon does not make the CLI contact that daemon.
#[test]
fn config_set_ignores_another_daemon_when_this_configuration_has_none() {
    let fixture_a = Fixture::new("admin");
    let daemon_a = Daemon::start(&fixture_a);
    let fixture_b = Fixture::new("admin");
    assert!(
        !fixture_b.socket().exists(),
        "no daemon may serve the CLI's config dir"
    );

    let output = run_cli_with_socket(
        &fixture_b,
        &fixture_a.socket(),
        &[
            "config",
            "set",
            "--no-interactive",
            "users.me.permission_profiles",
            "reader",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "with no daemon for this config dir the direct save must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert!(
        envelope.get("daemon").is_none(),
        "with no daemon for this config dir the envelope is today's: {envelope}"
    );
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture_b.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved its own config.toml:\n{}",
        fixture_b.config_text()
    );

    let mut probe = daemon_a.probe();
    let live = live_profiles_of_me(&mut probe);
    assert!(
        live.contains("admin") && !live.contains("reader"),
        "the other daemon's live roster must be unchanged: {live}"
    );
}

/// With no heartbeat naming a daemon for the CLI's config dir, the CLI
/// connects to nothing: a listener at the endpoint `ZEROCLAW_SOCKET` names
/// is never connected to, let alone sent the edit.
#[test]
fn config_set_contacts_no_endpoint_without_a_running_daemon() {
    let fixture = Fixture::new("admin");
    let heartbeat = fixture.path().join("state").join("daemon_state.json");
    assert!(
        !heartbeat.exists(),
        "no heartbeat may name a daemon for this fixture"
    );
    // Inside the fixture's short /tmp root, so the path fits `sun_path`.
    let endpoint = fixture.path().join("listener.sock");
    let listener = UnixListener::bind(&endpoint)
        .unwrap_or_else(|error| panic!("bind {}: {error}", endpoint.display()));

    let output = run_cli_with_socket(
        &fixture,
        &endpoint,
        &[
            "config",
            "set",
            "--no-interactive",
            "users.me.permission_profiles",
            "reader",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "with no daemon the direct save must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert!(
        envelope.get("daemon").is_none(),
        "with no daemon the envelope is today's: {envelope}"
    );
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );

    // A connection the CLI made would wait in the accept queue even after
    // the CLI exited.
    listener
        .set_nonblocking(true)
        .expect("make the listener nonblocking");
    match listener.accept() {
        Err(error) => assert_eq!(
            error.kind(),
            std::io::ErrorKind::WouldBlock,
            "accept must find the queue empty, not fail: {error}"
        ),
        Ok((_, peer)) => panic!("the CLI connected to {}: {peer:?}", endpoint.display()),
    }
}

/// A write that re-asserts the value already in the file still reaches the
/// daemon: the file is not what the daemon enforces.
#[test]
fn config_set_reasserting_a_hand_edit_reaches_the_daemon() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);
    let text = fixture.config_text();
    let edited = text.replace(
        r#"permission_profiles = ["admin"]"#,
        r#"permission_profiles = ["reader"]"#,
    );
    assert_ne!(
        edited, text,
        "the fixture grants users.me the admin profile"
    );
    fixture.write_config_text(&edited);

    let mut established = daemon.probe();
    let live = live_profiles_of_me(&mut established);
    assert!(
        live.contains("admin"),
        "the daemon still enforces the loaded roster: {live}"
    );

    let output = config_set_json(&fixture, "users.me.permission_profiles", "reader");
    assert!(
        output.status.success(),
        "an edit the daemon accepts must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(
        envelope["daemon"]["applied"], true,
        "the edit must reach the daemon although the file already had it: {envelope}"
    );
    assert_forbidden(
        established.config_set("gateway.host", "127.0.0.9"),
        "the established connection after the re-asserted narrowing",
    );
    let mut fresh = daemon.probe();
    assert_forbidden(
        fresh.config_set("gateway.host", "127.0.0.9"),
        "a new connection after the re-asserted narrowing",
    );
}

/// While a daemon runs, an edit the CLI would save itself is refused when it
/// turns the policy in the file, which compiles, into one that does not: the
/// daemon could never take it, and would install a deny-all policy at its
/// next reload.
#[test]
fn config_set_leaves_no_uncompilable_policy_pending() {
    let fixture = Fixture::unmapped("admin");
    let _daemon = Daemon::start_expecting(&fixture, Handshake::Refuses(AUTH_REQUIRED));
    let before = fixture.config_text();

    // The daemon refuses this process at the handshake, which would fall
    // back to the direct save; a roster entry with no permission profile
    // does not compile.
    let uid = fixture.other_uid().to_string();
    let output = config_set_json(&fixture, "users.bob.uid", &uid);
    assert!(
        !output.status.success(),
        "an edit whose policy does not compile must fail\n{}",
        describe(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("would not compile"),
        "stderr must say the policy would not compile\n{}",
        describe(&output)
    );
    assert!(
        toml_at(&fixture.config_toml(), "users.bob").is_none(),
        "a refused edit must not reach config.toml:\n{}",
        fixture.config_text()
    );
    assert_eq!(
        fixture.config_text(),
        before,
        "a refused edit must leave config.toml untouched"
    );
}

/// A patch that touches the authorization inputs goes to the daemon whole,
/// including its other edits, and is saved and swapped in as one unit.
#[test]
fn config_patch_mixed_batch_is_committed_as_one_unit() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);
    let uid = fixture.other_uid();

    let patch = json!([
        {"op": "replace", "path": "/gateway/host", "value": "127.0.0.5"},
        {"op": "add", "path": "/users/bob/uid", "value": uid},
        {"op": "add", "path": "/users/bob/permission_profiles", "value": ["reader"]}
    ]);
    let output = config_patch_json(&fixture, &patch);
    assert!(
        output.status.success(),
        "a batch the daemon accepts must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(
        envelope["daemon"]["applied"], true,
        "the daemon must have committed the batch: {envelope}"
    );
    assert_clean_stderr(&output);

    let mut probe = daemon.probe();
    let host = probe
        .config_get("gateway.host")
        .unwrap_or_else(|error| panic!("an admin principal must be able to read config: {error}"));
    assert_eq!(
        host["value"], "127.0.0.5",
        "the daemon's live config must carry the batch's other edit: {host}"
    );
    let bob = probe
        .config_get("users.bob.uid")
        .unwrap_or_else(|error| panic!("the daemon's live config must carry users.bob: {error}"));
    assert!(
        bob["value"]
            .as_str()
            .is_some_and(|value| value.contains(&uid.to_string())),
        "the daemon's live users.bob.uid must be {uid}: {bob}"
    );
    let saved = fixture.config_toml();
    assert_eq!(
        toml_at(&saved, "gateway.host").and_then(toml::Value::as_str),
        Some("127.0.0.5"),
        "the daemon must have saved the whole batch:\n{}",
        fixture.config_text()
    );
    assert_eq!(
        toml_at(&saved, "users.bob.uid").and_then(toml::Value::as_integer),
        Some(i64::from(uid)),
        "the daemon must have saved the whole batch:\n{}",
        fixture.config_text()
    );
}

/// `config init` has no daemon path: a permission profile it creates is
/// saved and reported as pending while a daemon runs.
#[test]
fn config_init_of_an_authorization_entry_is_pending_while_a_daemon_runs() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);

    let output = run_cli(
        &fixture,
        &["config", "init", "permission_profiles.probe", "--json"],
        None,
    );
    assert!(
        output.status.success(),
        "config init must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(
        envelope["initialized"],
        json!(["permission_profiles.probe"]),
        "{envelope}"
    );
    assert_eq!(envelope["daemon"]["applied"], false, "{envelope}");
    assert_eq!(envelope["daemon"]["pending_reload"], true, "{envelope}");
    assert_eq!(
        envelope["daemon"]["reason"], "offline_command",
        "{envelope}"
    );
    assert_clean_stderr(&output);
    assert!(
        toml_at(&fixture.config_toml(), "permission_profiles.probe").is_some(),
        "the CLI must have saved the new profile:\n{}",
        fixture.config_text()
    );

    let mut probe = daemon.probe();
    assert!(
        probe.config_get("permission_profiles.probe.admin").is_err(),
        "the daemon's live config must not have the new profile yet"
    );
}

/// A roster entry `config init` creates has no uid and no permission profile
/// until it is filled in, so the policy it leaves would not compile. While a
/// daemon runs, which would install deny-all in its place at its next
/// reload, the command fails and saves nothing.
#[test]
fn config_init_of_an_uncompilable_authorization_entry_fails_while_a_daemon_runs() {
    let fixture = Fixture::new("admin");
    let _daemon = Daemon::start(&fixture);
    let before = fixture.config_text();

    let output = run_cli(&fixture, &["config", "init", "users.alice", "--json"], None);
    assert!(
        !output.status.success(),
        "an entry whose policy would not compile must fail\n{}",
        describe(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("would not compile"),
        "stderr must say the policy would not compile\n{}",
        describe(&output)
    );
    assert!(
        toml_at(&fixture.config_toml(), "users.alice").is_none(),
        "a refused entry must not reach config.toml:\n{}",
        fixture.config_text()
    );
    assert_eq!(
        fixture.config_text(),
        before,
        "a refused entry must leave config.toml untouched"
    );
}

/// A patch with a write the daemon's config methods refuse (clearing a
/// secret) cannot be replayed there, so the CLI saves the whole patch itself
/// and reports its authorization edit as pending.
#[test]
fn config_patch_the_daemon_cannot_replay_is_saved_and_pending() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);

    let patch = json!([
        {"op": "replace", "path": "/users/me/permission_profiles", "value": ["reader"]},
        {"op": "remove", "path": "/relay/token"}
    ]);
    let output = config_patch_json(&fixture, &patch);
    assert!(
        output.status.success(),
        "a patch the daemon cannot replay falls back to the direct save\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["daemon"]["applied"], false, "{envelope}");
    assert_eq!(envelope["daemon"]["pending_reload"], true, "{envelope}");
    assert_eq!(envelope["daemon"]["reason"], "not_replayable", "{envelope}");
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );

    let mut probe = daemon.probe();
    let live = live_profiles_of_me(&mut probe);
    assert!(
        live.contains("admin") && !live.contains("reader"),
        "the daemon's live roster must be unchanged: {live}"
    );
    probe
        .config_set("gateway.host", "127.0.0.9")
        .unwrap_or_else(|error| panic!("the daemon must still grant admin: {error}"));
}

/// A patch the daemon rejects fails with the JSON error envelope a patch the
/// CLI rejects itself uses, and saves nothing. The file names a permission
/// profile the running daemon never loaded, so the CLI's own validation
/// accepts the roster entry that uses it and only the daemon refuses it.
#[test]
fn config_patch_rejected_by_the_daemon_fails_with_a_json_error() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);
    let edited = format!(
        "{}\n[permission_profiles.ops]\nadmin = true\n",
        fixture.config_text()
    );
    fixture.write_config_text(&edited);

    let patch = json!([
        {"op": "add", "path": "/users/bob/uid", "value": fixture.other_uid()},
        {"op": "add", "path": "/users/bob/permission_profiles", "value": ["ops"]}
    ]);
    let output = config_patch_json(&fixture, &patch);
    assert!(
        !output.status.success(),
        "a patch the daemon rejects must fail\n{}",
        describe(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "a failed patch prints no envelope\n{}",
        describe(&output)
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!("stderr is not a JSON error: {error}\n{}", describe(&output))
    });
    assert_eq!(error["code"], "validation_failed", "{error}");
    assert!(
        error["message"].as_str().is_some_and(|message| {
            message.contains("running daemon rejected") && message.contains("ops")
        }),
        "the error must carry the daemon's verdict and reason: {error}"
    );
    assert_eq!(
        fixture.config_text(),
        edited,
        "a rejected patch must leave config.toml untouched"
    );

    let mut probe = daemon.probe();
    if let Ok(live) = probe.config_get("users.bob.uid") {
        panic!("the daemon's live config must not gain users.bob: {live}");
    }
}

/// With no heartbeat naming a daemon, another process holding the config
/// dir's ownership lock may be a daemon that is still starting, which takes
/// that lock before it loads its configuration. The CLI then saves the edit
/// and reports it as pending rather than as if no daemon ran.
#[test]
fn config_set_reports_pending_while_another_process_owns_the_configuration() {
    let fixture = Fixture::new("admin");
    assert!(
        !fixture
            .path()
            .join("state")
            .join("daemon_state.json")
            .exists(),
        "no heartbeat may name a daemon for this fixture"
    );
    let owner = ConfigOwnershipGuard::acquire(&fixture.data_dir())
        .unwrap_or_else(|error| panic!("take the config dir's ownership lock: {error}"));

    let output = config_set_json(&fixture, "users.me.permission_profiles", "reader");
    assert!(
        output.status.success(),
        "an edit while another process owns the config dir falls back to the direct save\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["daemon"]["applied"], false, "{envelope}");
    assert_eq!(envelope["daemon"]["pending_reload"], true, "{envelope}");
    assert_eq!(
        envelope["daemon"]["reason"], "owner_unconfirmed",
        "{envelope}"
    );
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );
    drop(owner);
}

/// A patch that tests an authorization input and replaces another, which a
/// daemon running as the admin `users.me` would otherwise take.
fn patch_testing_an_authorization_input() -> Value {
    json!([
        {"op": "test", "path": "/permission_profiles/admin/admin", "value": true},
        {"op": "replace", "path": "/users/me/permission_profiles", "value": ["reader"]}
    ])
}

/// A patch the running daemon would commit applies its writes to the
/// daemon's live configuration, while the CLI checked its `test` ops against
/// its own copy of config.toml. A `test` op on an authorization input then
/// fails the whole patch before the daemon is contacted, and nothing is
/// saved.
#[test]
fn config_patch_testing_an_authorization_input_fails_while_a_daemon_runs() {
    let fixture = Fixture::new("admin");
    let daemon = Daemon::start(&fixture);
    let before = fixture.config_text();

    let output = config_patch_json(&fixture, &patch_testing_an_authorization_input());
    assert!(
        !output.status.success(),
        "a patch whose `test` op cannot be checked where it applies must fail\n{}",
        describe(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "a failed patch prints no envelope\n{}",
        describe(&output)
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!("stderr is not a JSON error: {error}\n{}", describe(&output))
    });
    assert_eq!(error["code"], "op_not_supported", "{error}");
    assert_eq!(error["path"], "permission_profiles.admin.admin", "{error}");
    assert!(
        error["message"].as_str().is_some_and(|message| {
            message.contains("cannot be checked against the running daemon's live configuration")
        }),
        "the error must say why the `test` op was refused: {error}"
    );
    assert_eq!(
        fixture.config_text(),
        before,
        "a refused patch must leave config.toml untouched"
    );

    let mut probe = daemon.probe();
    let live = live_profiles_of_me(&mut probe);
    assert!(
        live.contains("admin") && !live.contains("reader"),
        "the daemon's live roster must be unchanged: {live}"
    );
}

/// With no daemon running, the patch is saved to the copy of config.toml its
/// `test` ops were checked against, so they keep their meaning: one that
/// holds lets the patch through, one that fails stops it.
#[test]
fn config_patch_testing_an_authorization_input_is_unchanged_without_a_daemon() {
    let fixture = Fixture::new("admin");
    assert!(
        !fixture.socket().exists(),
        "no daemon may serve this fixture"
    );

    let output = config_patch_json(&fixture, &patch_testing_an_authorization_input());
    assert!(
        output.status.success(),
        "with no daemon a `test` op that holds lets the patch through\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["saved"], true, "{envelope}");
    assert_eq!(envelope["results"][0]["op"], "test", "{envelope}");
    assert!(
        envelope.get("daemon").is_none(),
        "with no daemon the envelope is today's: {envelope}"
    );
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "users.me.permission_profiles"),
        Some(&string_array(&["reader"])),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );

    let before = fixture.config_text();
    let failing = json!([
        {"op": "test", "path": "/permission_profiles/admin/admin", "value": false},
        {"op": "replace", "path": "/users/me/permission_profiles", "value": ["admin"]}
    ]);
    let output = config_patch_json(&fixture, &failing);
    assert!(
        !output.status.success(),
        "a `test` op that fails stops the patch\n{}",
        describe(&output)
    );
    let error: Value = serde_json::from_slice(&output.stderr).unwrap_or_else(|error| {
        panic!("stderr is not a JSON error: {error}\n{}", describe(&output))
    });
    assert_eq!(error["code"], "validation_failed", "{error}");
    assert_eq!(
        fixture.config_text(),
        before,
        "a patch whose `test` op fails must leave config.toml untouched"
    );
}
