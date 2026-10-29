//! `zeroclaw config set` and `config patch` against a running daemon, over
//! its Windows named pipe.
//!
//! A pipe carries no peer uid, so the CLI identifies the daemon the other
//! way round: it asks the kernel which process created the pipe's server end
//! and requires that process to be the one the daemon heartbeat names,
//! running as this account or SYSTEM. Only then does it hand an edit to the
//! authorization inputs (`[users]`, `[permission_profiles]`, `[oidc]`,
//! `security.trust_daemon_uid`) to the daemon, which validates it, saves
//! `config.toml` itself and publishes the new policy. A pipe served by any
//! other process is sent nothing, and the CLI saves the file directly and
//! reports the edit as pending a reload. Once a `[users]` roster exists, a
//! pipe client that presents no token is refused at the handshake, so the CLI
//! takes the same pending path. With no daemon, nothing is contacted.
//!
//! Every test drives real processes: a `zeroclaw daemon` on a private config
//! dir, port and pipe, the CLI in a separate process, and a raw JSON-RPC
//! client on the pipe. The pipe name is pinned with `ZEROCLAW_SOCKET` for the
//! daemon and the CLI alike, so no test depends on how the default name is
//! derived.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use zeroclaw_api::jsonrpc::error_codes::AUTH_REQUIRED;
use zeroclaw_runtime::rpc::dispatch::RPC_PROTOCOL_VERSION;

/// Bound on daemon startup, until `/health`, the heartbeat and the pipe serve.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on one CLI run, so a CLI that blocks on the daemon fails the test
/// instead of hanging it.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on each request a probe makes, so a daemon that stops answering
/// fails the test instead of hanging it.
const RPC_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on opening a pipe, and on waiting for a pipe the test serves to be
/// opened.
const OPEN_TIMEOUT: Duration = Duration::from_secs(2);
/// `ERROR_PIPE_BUSY`: every instance is connected and the server has not yet
/// created the next one.
const ERROR_PIPE_BUSY: i32 = 231;
/// `ERROR_NO_DATA`: the client opened the pipe and closed it again before the
/// server looked.
const ERROR_NO_DATA: i32 = 232;

/// A private config dir, port and pipe.
struct Fixture {
    dir: tempfile::TempDir,
    port: u16,
    pipe: PathBuf,
}

impl Fixture {
    /// No roster: a local pipe client is the shared operator, so the daemon
    /// takes the CLI's edits.
    fn no_roster() -> Self {
        Self::write("")
    }

    /// A roster that maps a uid: a pipe client that presents no token
    /// identifies nobody and is refused at `initialize`.
    fn with_roster() -> Self {
        Self::write(
            r#"
[users.me]
uid = 4242
permission_profiles = ["reader"]
"#,
        )
    }

    fn write(roster: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("zc")
            .tempdir()
            .expect("create a temp config dir");
        let port = free_port();
        let pipe = PathBuf::from(format!(
            r"\\.\pipe\zeroclaw-component-{}-{port}",
            std::process::id()
        ));
        let version = zeroclaw_config::migration::CURRENT_SCHEMA_VERSION;
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                r#"schema_version = {version}

[gateway]
host = "127.0.0.1"
port = {port}
require_pairing = false

[permission_profiles.reader]
grants = {{ sessions = ["read"], config = ["read"] }}
{roster}"#
            ),
        )
        .expect("write the fixture config.toml");
        Self { dir, port, pipe }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.path().join("config.toml")).expect("read config.toml")
    }

    /// `config.toml` parsed as plain TOML, so assertions do not depend on the
    /// `Config` schema.
    fn config_toml(&self) -> toml::Value {
        let text = self.config_text();
        toml::from_str(&text)
            .unwrap_or_else(|error| panic!("config.toml does not parse: {error}\n{text}"))
    }

    fn heartbeat_path(&self) -> PathBuf {
        self.path().join("state").join("daemon_state.json")
    }

    /// Whether the daemon heartbeat beside `config.toml` names `pid`: the CLI
    /// hands an edit only to the daemon its config dir records.
    fn heartbeat_names(&self, pid: u32) -> bool {
        std::fs::read_to_string(self.heartbeat_path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|state| state.get("pid").and_then(Value::as_u64))
            == Some(u64::from(pid))
    }

    /// A heartbeat as a daemon would have left it a moment ago, naming `pid`.
    fn forge_heartbeat(&self, pid: u32) {
        let path = self.heartbeat_path();
        std::fs::create_dir_all(path.parent().expect("the state file has a directory"))
            .expect("create the state directory");
        let state = json!({ "pid": pid, "written_at": chrono::Utc::now().to_rfc3339() });
        std::fs::write(path, state.to_string()).expect("write the heartbeat");
    }

    /// Serve the fixture's pipe from this test with one pending instance, in
    /// place of a daemon.
    fn serve_pipe(&self, runtime: &tokio::runtime::Runtime) -> NamedPipeServer {
        let name = self.pipe.to_string_lossy().into_owned();
        runtime.block_on(async {
            ServerOptions::new()
                .first_pipe_instance(true)
                .create(&name)
                .unwrap_or_else(|error| panic!("create {name}: {error}"))
        })
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .expect("reserve a free loopback port")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a runtime for the test's pipe server")
}

/// A `zeroclaw` command bound to the fixture's config dir and pipe that
/// inherits no `ZEROCLAW_*` variable from the invoking shell: env overrides
/// would rewrite fixture values.
fn zeroclaw_command(fixture: &Fixture) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ZEROCLAW_") {
            command.env_remove(key);
        }
    }
    command
        .env("ZEROCLAW_CONFIG_DIR", fixture.path())
        .env("ZEROCLAW_SOCKET", &fixture.pipe)
        .env("LC_ALL", "C")
        .env("TERM", "dumb");
    command
}

/// A `zeroclaw daemon` serving one fixture. Dropping it kills and reaps the
/// process, so a failed assertion never leaks a daemon.
struct Daemon {
    child: Child,
    pipe: PathBuf,
    log: PathBuf,
}

/// What the daemon's `initialize` answers a pipe client once it serves.
#[derive(Clone, Copy)]
enum Handshake {
    /// No roster: it binds the shared operator.
    BindsSharedOperator,
    /// A roster: it binds no principal and refuses the client with this code.
    Refuses(i32),
}

impl Daemon {
    fn start(fixture: &Fixture) -> Self {
        Self::start_expecting(fixture, Handshake::BindsSharedOperator)
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
        let child = zeroclaw_command(fixture)
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
            pipe: fixture.pipe.clone(),
            log,
        };
        daemon.wait_until_serving(fixture, handshake);
        daemon
    }

    /// Wait until the gateway answers `/health`, the heartbeat names this
    /// daemon, and the pipe answers `initialize` as `expected` says.
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
                && let Ok(mut probe) = RpcProbe::connect(&self.pipe)
            {
                let handshake = probe.initialize();
                match expected {
                    Handshake::BindsSharedOperator => self.assert_bound(handshake),
                    Handshake::Refuses(code) => self.assert_refused(handshake, code),
                }
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon did not serve /health, its heartbeat and {} within {STARTUP_TIMEOUT:?}\n{}",
                self.pipe.display(),
                self.output()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A new connection, bound through `initialize`.
    fn probe(&self) -> RpcProbe {
        let mut probe = RpcProbe::connect(&self.pipe)
            .unwrap_or_else(|error| panic!("open {}: {error}", self.pipe.display()));
        let handshake = probe.initialize();
        self.assert_bound(handshake);
        probe
    }

    fn assert_bound(&self, handshake: Result<Value, Value>) {
        let result = handshake
            .unwrap_or_else(|error| panic!("initialize was refused: {error}\n{}", self.output()));
        assert!(
            result
                .get("principal_id")
                .and_then(Value::as_str)
                .is_some_and(|principal| !principal.is_empty()),
            "initialize must bind a principal: {result}\n{}",
            self.output()
        );
    }

    /// A daemon with a roster refuses a pipe client that presents no token
    /// at `initialize` with `code`.
    fn assert_refused(&self, handshake: Result<Value, Value>, code: i32) {
        match handshake {
            Err(error) => assert_eq!(
                error.get("code").and_then(Value::as_i64),
                Some(i64::from(code)),
                "initialize must refuse this client with {code}: {error}\n{}",
                self.output()
            ),
            Ok(result) => panic!(
                "initialize must refuse this client with {code}, but it bound {result}\n{}",
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

/// A raw JSON-RPC 2.0 client on the daemon's pipe, one newline-delimited
/// frame per message, every exchange bounded by `RPC_TIMEOUT`. The connection
/// stays open for the probe's lifetime, so a test can observe how an
/// established binding behaves after the policy changes underneath it.
struct RpcProbe {
    runtime: tokio::runtime::Runtime,
    reader: BufReader<ReadHalf<NamedPipeClient>>,
    writer: WriteHalf<NamedPipeClient>,
    next_id: u64,
}

impl RpcProbe {
    /// Open the pipe, waiting out a server that is between instances.
    fn connect(pipe: &Path) -> std::io::Result<Self> {
        let runtime = runtime();
        let name = pipe.to_string_lossy().into_owned();
        let client = runtime.block_on(async {
            let deadline = Instant::now() + OPEN_TIMEOUT;
            loop {
                match ClientOptions::new().open(&name) {
                    Ok(client) => return Ok(client),
                    Err(error)
                        if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                            && Instant::now() < deadline =>
                    {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(error) => return Err(error),
                }
            }
        })?;
        let (reader, writer) = tokio::io::split(client);
        Ok(Self {
            runtime,
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        })
    }

    /// Send one request and return its `result` (`Ok`) or its `error` object
    /// (`Err`). Frames with any other id, such as notifications, are skipped.
    /// A daemon that does not answer within `RPC_TIMEOUT` fails the test.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut frame =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        frame.push('\n');
        let Self {
            runtime,
            reader,
            writer,
            ..
        } = self;
        let exchange = async {
            writer
                .write_all(frame.as_bytes())
                .await
                .unwrap_or_else(|error| panic!("send {method}: {error}"));
            loop {
                let mut line = String::new();
                let read = reader
                    .read_line(&mut line)
                    .await
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
        };
        // The deadline is created inside the runtime, which owns the timer.
        runtime
            .block_on(async { tokio::time::timeout(RPC_TIMEOUT, exchange).await })
            .unwrap_or_else(|elapsed| {
                panic!("the daemon did not answer {method} within {RPC_TIMEOUT:?}: {elapsed}")
            })
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
}

/// The daemon's live value of `prop`, as `config/get` reports it.
fn live_value(probe: &mut RpcProbe, prop: &str) -> String {
    let live = probe
        .config_get(prop)
        .unwrap_or_else(|error| panic!("the daemon must answer config/get {prop}: {error}"));
    live["value"]
        .as_str()
        .unwrap_or_else(|| panic!("config/get {prop} must answer a string: {live}"))
        .to_owned()
}

/// Run the CLI against the fixture's config dir and pipe.
fn run_cli(fixture: &Fixture, args: &[&str], input: Option<&[u8]>) -> Output {
    run(zeroclaw_command(fixture), args, input)
}

/// `run_cli` with `ZEROCLAW_SOCKET` naming `pipe`, as in a shell that
/// exported it for another daemon.
fn run_cli_with_pipe(fixture: &Fixture, pipe: &Path, args: &[&str]) -> Output {
    let mut command = zeroclaw_command(fixture);
    command.env("ZEROCLAW_SOCKET", pipe);
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

/// A pending outcome reported in the envelope's `daemon` member.
fn assert_pending(envelope: &Value, reason: &str) {
    assert_eq!(envelope["daemon"]["applied"], false, "{envelope}");
    assert_eq!(envelope["daemon"]["pending_reload"], true, "{envelope}");
    assert_eq!(envelope["daemon"]["reason"], reason, "{envelope}");
}

/// What a pipe the test serves saw of the CLI: whether anything opened it,
/// and how many bytes arrived.
fn observe_pipe(runtime: &tokio::runtime::Runtime, server: &mut NamedPipeServer) -> Option<usize> {
    use tokio::io::AsyncReadExt;

    runtime.block_on(async {
        match tokio::time::timeout(OPEN_TIMEOUT, server.connect()).await {
            Err(_) => return None,
            Ok(Ok(())) => {}
            // The CLI hung up before the instance noticed it.
            Ok(Err(error)) if error.raw_os_error() == Some(ERROR_NO_DATA) => return Some(0),
            Ok(Err(error)) => panic!("the pending instance did not connect: {error}"),
        }
        let mut bytes = Vec::new();
        match tokio::time::timeout(OPEN_TIMEOUT, server.read_to_end(&mut bytes)).await {
            Ok(_) => Some(bytes.len()),
            Err(elapsed) => panic!("the CLI kept the pipe open: {elapsed}"),
        }
    })
}

/// With no roster the CLI is the shared operator, so a batch that creates
/// the first roster entry is applied live: the established connection loses
/// its grants at its next operation, and a new pipe client is refused at the
/// handshake, without any reload.
#[test]
fn config_patch_creating_a_roster_is_applied_live_and_closes_the_shared_operator_path() {
    let fixture = Fixture::no_roster();
    let daemon = Daemon::start(&fixture);
    let mut established = daemon.probe();
    established
        .config_get("gateway.host")
        .unwrap_or_else(|error| panic!("the shared operator must be able to read config: {error}"));

    let patch = json!([
        {"op": "add", "path": "/users/bob/uid", "value": 4242},
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
        "the daemon must have committed the batch over its pipe: {envelope}"
    );
    assert_clean_stderr(&output);

    // The roster is enforced at once: the established shared-operator
    // connection identifies nobody now, and so does a fresh one.
    assert!(
        established.config_get("gateway.host").is_err(),
        "the established connection must lose the shared operator's grants once a roster exists"
    );
    let mut fresh = RpcProbe::connect(&fixture.pipe)
        .unwrap_or_else(|error| panic!("open {}: {error}", fixture.pipe.display()));
    daemon.assert_refused(fresh.initialize(), AUTH_REQUIRED);

    let saved = fixture.config_toml();
    assert_eq!(
        toml_at(&saved, "users.bob.uid").and_then(toml::Value::as_integer),
        Some(4242),
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

/// A `--comment` on an edit the daemon applies travels with the edit: the
/// daemon writes it under the lock it saved under, and the CLI leaves
/// config.toml alone afterwards.
#[test]
fn config_set_with_no_roster_is_applied_and_the_daemon_writes_the_comment() {
    let fixture = Fixture::no_roster();
    let daemon = Daemon::start(&fixture);

    let output = run_cli(
        &fixture,
        &[
            "config",
            "set",
            "--no-interactive",
            "security.trust_daemon_uid",
            "false",
            "--comment",
            "daemon uid no longer trusted",
            "--json",
        ],
        None,
    );
    assert!(
        output.status.success(),
        "an edit the daemon accepts must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(envelope["path"], "security.trust_daemon_uid", "{envelope}");
    assert_eq!(
        envelope["daemon"]["applied"], true,
        "the daemon must have committed the edit over its pipe: {envelope}"
    );
    assert_clean_stderr(&output);

    let text = fixture.config_text();
    assert_eq!(
        text.matches("daemon uid no longer trusted").count(),
        1,
        "config.toml must carry the comment exactly once:\n{text}"
    );
    assert_eq!(
        toml_at(&fixture.config_toml(), "security.trust_daemon_uid"),
        Some(&toml::Value::Boolean(false)),
        "the daemon must have saved the edit:\n{text}"
    );
    let mut probe = daemon.probe();
    assert_eq!(
        live_value(&mut probe, "security.trust_daemon_uid"),
        "false",
        "the daemon must enforce the edit without a reload"
    );
}

/// A pipe client presents no token, so once a roster exists the daemon binds
/// no principal to the CLI and refuses it at the handshake: the CLI saves
/// the file directly and reports the edit as pending.
#[test]
fn config_set_with_a_roster_is_refused_at_the_handshake_and_pending() {
    let fixture = Fixture::with_roster();
    let _daemon = Daemon::start_expecting(&fixture, Handshake::Refuses(AUTH_REQUIRED));

    let output = config_set_json(&fixture, "users.me.permission_profiles", "reader");
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
    assert_pending(&envelope, "refused");
    assert_clean_stderr(&output);

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
}

/// `ZEROCLAW_SOCKET` exported for another config dir's daemon sends this
/// dir's edit to it: the kernel names that daemon as the pipe's server, which
/// is not the process this dir's heartbeat records, so the CLI sends it
/// nothing, saves its own file and reports the edit as pending.
#[test]
fn config_set_refuses_to_publish_through_another_daemon() {
    let fixture_a = Fixture::no_roster();
    let daemon_a = Daemon::start(&fixture_a);
    let fixture_b = Fixture::no_roster();
    let _daemon_b = Daemon::start(&fixture_b);
    let before_a = fixture_a.config_text();

    let output = run_cli_with_pipe(
        &fixture_b,
        &fixture_a.pipe,
        &[
            "config",
            "set",
            "--no-interactive",
            "security.trust_daemon_uid",
            "false",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "an edit no daemon of this config dir could take falls back to the direct save\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_pending(&envelope, "other_daemon");
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture_b.config_toml(), "security.trust_daemon_uid"),
        Some(&toml::Value::Boolean(false)),
        "the CLI must have saved its own config.toml:\n{}",
        fixture_b.config_text()
    );

    // The other daemon received nothing: its live policy and its file are as
    // they were.
    let mut probe = daemon_a.probe();
    assert_eq!(
        live_value(&mut probe, "security.trust_daemon_uid"),
        "true",
        "the other daemon's live policy must be unchanged"
    );
    assert_eq!(
        fixture_a.config_text(),
        before_a,
        "the other daemon's config.toml must be unchanged"
    );
}

/// The heartbeat names a live process, but the pipe is served by another
/// one: the kernel names the server, the CLI sends it nothing, not even the
/// handshake, and reports the edit as another daemon's.
#[test]
fn config_set_is_sent_nothing_when_another_process_serves_the_pipe() {
    let fixture = Fixture::no_roster();
    let mut bystander = Command::new("cmd")
        .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
        .spawn()
        .expect("spawn a long-lived process");
    fixture.forge_heartbeat(bystander.id());
    let runtime = runtime();
    let mut server = fixture.serve_pipe(&runtime);

    let output = config_set_json(&fixture, "security.trust_daemon_uid", "false");
    bystander.kill().expect("terminate the process");
    bystander.wait().expect("reap the process");
    assert!(
        output.status.success(),
        "an edit no daemon could take falls back to the direct save\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_pending(&envelope, "other_daemon");
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "security.trust_daemon_uid"),
        Some(&toml::Value::Boolean(false)),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );
    assert_eq!(
        observe_pipe(&runtime, &mut server),
        Some(0),
        "the CLI opens the pipe to ask the kernel who serves it, and writes nothing"
    );
}

/// With no heartbeat naming a daemon for the CLI's config dir, the CLI
/// contacts nothing: a pipe served at the name `ZEROCLAW_SOCKET` gives is
/// never opened, let alone sent the edit.
#[test]
fn config_set_contacts_no_endpoint_without_a_running_daemon() {
    let fixture = Fixture::no_roster();
    assert!(
        !fixture.heartbeat_path().exists(),
        "no heartbeat may name a daemon for this fixture"
    );
    let runtime = runtime();
    let mut server = fixture.serve_pipe(&runtime);

    let output = config_set_json(&fixture, "security.trust_daemon_uid", "false");
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
        toml_at(&fixture.config_toml(), "security.trust_daemon_uid"),
        Some(&toml::Value::Boolean(false)),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );
    assert_eq!(
        observe_pipe(&runtime, &mut server),
        None,
        "nothing may open the pipe while no daemon runs"
    );
}

/// A daemon that stopped leaves its last heartbeat behind. The CLI probes the
/// process it names and finds none, so the edit keeps the direct save and
/// today's output instead of waiting on a pipe nobody serves.
#[test]
fn config_set_after_the_daemon_stopped_finds_no_daemon() {
    let fixture = Fixture::no_roster();
    let daemon = Daemon::start(&fixture);
    let pid = daemon.child.id();
    drop(daemon);
    assert!(
        fixture.heartbeat_names(pid),
        "the stopped daemon's heartbeat must still name it"
    );

    let output = config_set_json(&fixture, "security.trust_daemon_uid", "false");
    assert!(
        output.status.success(),
        "with the daemon stopped the direct save must succeed\n{}",
        describe(&output)
    );
    let envelope = stdout_json(&output);
    assert_eq!(
        envelope
            .as_object()
            .map(|members| members.keys().cloned().collect::<Vec<_>>()),
        Some(vec!["path".to_owned(), "value".to_owned()]),
        "a stale heartbeat is no daemon, so the envelope is today's: {envelope}"
    );
    assert_clean_stderr(&output);
    assert_eq!(
        toml_at(&fixture.config_toml(), "security.trust_daemon_uid"),
        Some(&toml::Value::Boolean(false)),
        "the CLI must have saved config.toml directly:\n{}",
        fixture.config_text()
    );
}
