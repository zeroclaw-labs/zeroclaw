// `apps/zerocode` is a standalone TUI client, not daemon-path code.
// It speaks JSON-RPC to whatever ZeroClaw daemon is at the configured
// address; the daemon owns attribution, the TUI owns its session id.
// Bare `tokio::spawn` is the right primitive here — the workspace-wide
// `zeroclaw_spawn::spawn!` rule is daemon-path only (see `clippy.toml`'s
// commentary, which records this crate as the sole exemption).
#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::{ExitCode, ExitStatus};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use clap::Parser;

mod acp;
mod app;
mod attachment;
mod chat;
mod client;
mod clipboard;
mod color_depth;
mod config;
mod config_manager;
mod dashboard;
mod diff;
mod display_width;
mod doctor;
mod editor;
mod file_explorer;
mod help;
mod i18n;
mod input_bar;
mod jsonrpc;
mod keymap;
mod logs;
mod mouse;
mod quickstart_pane;
mod sop_pane;
mod terminal_backend;
mod text_navigation;
mod theme;
mod todo_tracker;
mod turn_status;
mod widgets;
mod wire;
mod zerocode_pane;

const DAEMON_CONNECT_INTERVAL: Duration = Duration::from_millis(50);
const SPAWNED_DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DAEMON_STDERR_LIMIT: usize = 8 * 1024;

/// Set to `true` once the alternate screen is active so signal/panic
/// handlers know they need to restore the terminal before exiting.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(
    name = "zerocode",
    about = "Interactive TUI config manager for ZeroClaw",
    version,
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\n\nThis version must exactly match the running zeroclaw daemon. ",
        "The TUI and daemon share a wire protocol with no cross-version ",
        "compatibility guarantee; mismatched versions may fail to connect ",
        "or behave unpredictably."
    )
)]
struct Cli {
    /// Path to the ZeroClaw config directory
    #[arg(long)]
    config_dir: Option<PathBuf>,

    /// Start in chat mode with this agent alias.
    /// If omitted, opens the config manager.
    #[arg(long, short = 'a')]
    agent: Option<String>,

    /// Connect to a remote daemon via WSS instead of the local Unix socket.
    /// Example: `--connect wss://host:9781`
    #[arg(long)]
    connect: Option<String>,

    /// Skip TLS certificate verification for WSS connections.
    /// Required for self-signed certificates. Only used with --connect.
    #[arg(long)]
    tls_skip_verify: bool,
}

/// Where zerocode should connect.
pub(crate) enum ConnectTarget {
    LocalSocket(PathBuf),
    Wss { url: String, skip_verify: bool },
}

impl ConnectTarget {
    /// Human-readable label for the dashboard Status box.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::LocalSocket(p) => format!("local:{}", p.display()),
            Self::Wss { url, .. } => url.clone(),
        }
    }

    pub(crate) fn insecure_tls(&self) -> bool {
        matches!(
            self,
            Self::Wss {
                skip_verify: true,
                ..
            }
        )
    }

    /// Connect to this target, reclaiming a prior TUI identity when
    /// `prev_id`/`prev_sig` are supplied. Single source of truth for the
    /// per-transport connect call — used by initial startup and in-loop
    /// reconnection alike.
    pub(crate) async fn connect(
        &self,
        prev_id: Option<&str>,
        prev_sig: Option<&str>,
    ) -> anyhow::Result<client::RpcClient> {
        match self {
            Self::LocalSocket(socket) => {
                client::RpcClient::connect(socket, prev_id, prev_sig).await
            }
            Self::Wss { url, skip_verify } => {
                client::RpcClient::connect_wss(url, prev_id, prev_sig, *skip_verify).await
            }
        }
    }
}

fn resolve_wss_target(
    cli_connect: Option<String>,
    cli_skip_verify: bool,
    cfg_wss: &config::WssSection,
) -> Option<(String, bool)> {
    let uri = cli_connect.or_else(|| cfg_wss.uri.clone())?;
    let skip_verify = cli_skip_verify || cfg_wss.tls.skip_verify;
    Some((uri, skip_verify))
}

#[tokio::main]
async fn main() -> ExitCode {
    install_panic_hook();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zerocode: {}", format_startup_error(&e));
            ExitCode::FAILURE
        }
    }
}

fn format_startup_error(err: &anyhow::Error) -> String {
    if let Some(mismatch) = err.downcast_ref::<client::DaemonVersionMismatch>() {
        return i18n::t_args(
            "zc-error-daemon-version-mismatch",
            &[
                ("client_version", mismatch.client_version()),
                ("server_version", mismatch.server_version()),
            ],
        );
    }
    if let Some(timeout) = err.downcast_ref::<client::DaemonInitializeTimeout>() {
        return i18n::t_args(
            "zc-error-daemon-initialize-timeout",
            &[("seconds", &timeout.timeout_seconds().to_string())],
        );
    }
    if let Some(startup) = err.downcast_ref::<SpawnedDaemonStartupFailure>() {
        return i18n::t_args(
            "zc-error-spawned-daemon-startup",
            &[("details", startup.details())],
        );
    }
    format!("{err:#}")
}

/// Install a panic hook that restores the terminal before printing the
/// panic message.  Without this, a panic inside the event loop leaves the
/// terminal in raw mode / alternate screen, making the error unreadable.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        force_restore_terminal();
        default_hook(info);
    }));
}

/// Best-effort terminal restoration used by the panic hook and SIGTERM
/// handler.  Errors are intentionally ignored — we're already crashing.
fn force_restore_terminal() {
    if TERMINAL_ACTIVE.load(Ordering::Relaxed) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::DisableBracketedPaste,
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

enum InsecureTlsChoice {
    Once,
    Always,
    Abort,
}

/// Prompt the operator to accept an insecure-TLS connection to `url`.
///
/// Returns the operator's [`InsecureTlsChoice`]:
/// - [`InsecureTlsChoice::Once`] for `y` / `yes` (connect once, do not persist)
/// - [`InsecureTlsChoice::Always`] for `a` / `always` (connect and remember this route)
/// - [`InsecureTlsChoice::Abort`] for everything else (default, empty, `n`, junk)
///
/// Reads the operator's answer from `reader` and writes the prompt to
/// `writer` so tests can inject deterministic input without touching
/// `stdin` / `stderr`.
fn confirm_insecure_tls_with<R: std::io::BufRead, W: std::io::Write>(
    mut reader: R,
    writer: &mut W,
    url: &str,
) -> anyhow::Result<InsecureTlsChoice> {
    writeln!(
        writer,
        "\nWARNING: --tls-skip-verify DISABLES TLS certificate verification for\n\
         {url}\nThis connection is UNSAFE on untrusted networks (susceptible to\n\
         man-in-the-middle). Only continue on a trusted network against a\n\
         self-signed cert you control.\n\n\
         You are accepting an UNVERIFIED route, not a trusted peer.\n\
         [y] yes, connect once   [a] always (remember this route)   [N] no, abort"
    )?;
    write!(writer, "Continue with verification disabled? [y/a/N] ")?;
    writer.flush().ok();
    let mut answer = String::new();
    reader.read_line(&mut answer)?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(InsecureTlsChoice::Once),
        "a" | "always" => Ok(InsecureTlsChoice::Always),
        _ => Ok(InsecureTlsChoice::Abort),
    }
}

/// Production entry point: locks `stdin` and writes the prompt to `stderr`,
/// delegating to [`confirm_insecure_tls_with`]. Behaviour is identical to
/// the previous inline implementation — the refactor only adds the
/// `BufRead` / `Write` seam so the prompt logic can be unit-tested.
fn confirm_insecure_tls(url: &str) -> anyhow::Result<InsecureTlsChoice> {
    let stdin = std::io::stdin();
    let mut stderr = std::io::stderr();
    confirm_insecure_tls_with(stdin.lock(), &mut stderr, url)
}

/// Apply the operator's [`InsecureTlsChoice`] for `url`: a no-op for
/// `Once`, persisting the route acknowledgement for `Always`, or
/// bailing out for `Abort`. Extracted from the inline match in `run()`
/// so the choice -> side-effect mapping can be exercised directly in
/// tests without a running daemon.
fn apply_insecure_tls_choice(
    choice: InsecureTlsChoice,
    config_dir: &std::path::Path,
    url: &str,
) -> anyhow::Result<()> {
    match choice {
        InsecureTlsChoice::Once => {}
        InsecureTlsChoice::Always => {
            config::persist_wss_route_ack(config_dir, url)?;
        }
        InsecureTlsChoice::Abort => {
            anyhow::bail!("aborted: insecure TLS connection not confirmed");
        }
    }
    Ok(())
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let _ = rustls::crypto::ring::default_provider().install_default();

    let local_config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
    let loaded_config = match config::ensure_and_load(&local_config_dir) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("zerocode: config load failed ({e:#}); starting with defaults");
            config::ZerocodeConfig::default()
        }
    };
    let active_theme = loaded_config.resolve_theme().unwrap_or_else(|e| {
        let path = config::config_path(&local_config_dir);
        eprintln!("zerocode: {e:#}");
        eprintln!(
            "  fix: remove the entire [theme] section from {} to restore the default theme",
            path.display()
        );
        std::process::exit(1);
    });
    theme::set_active(active_theme);

    let resolved_locale = loaded_config
        .resolve_locale()
        .unwrap_or_else(i18n::detect_locale);
    i18n::init(&resolved_locale, &local_config_dir);

    // Apply persisted keybinding overrides into the keymap. A bad table
    // fails loud (same posture as an unknown theme) rather than silently
    // running stale bindings.
    match loaded_config.resolve_keybindings() {
        Ok(table) if !table.is_empty() => keymap::overrides::set_active(table),
        Ok(_) => {}
        Err(e) => {
            let path = config::config_path(&local_config_dir);
            eprintln!("zerocode: invalid keybindings: {e:#}");
            eprintln!(
                "  fix: remove the entire [keybindings] section from {} to restore default keybindings",
                path.display()
            );
            std::process::exit(1);
        }
    }

    let target = {
        let cfg_wss = &loaded_config.connection.wss;
        if let Some((uri, skip_verify)) =
            resolve_wss_target(cli.connect.clone(), cli.tls_skip_verify, cfg_wss)
        {
            ConnectTarget::Wss {
                url: uri,
                skip_verify,
            }
        } else {
            let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
            let socket = client::resolve_socket_path(&config_dir)?;
            ConnectTarget::LocalSocket(socket)
        }
    };

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    // Initial connection (before the terminal is initialized).
    // `owns_ephemeral` records whether THIS process spawned the daemon
    // (initial connect failed → we started one). Only an owned ephemeral
    // daemon may be respawned on disconnect, and then exactly once.
    let mut owns_ephemeral = false;
    let rpc = match &target {
        ConnectTarget::LocalSocket(socket) => {
            #[cfg(unix)]
            let initial_connection = tokio::select! {
                result = client::RpcClient::connect(socket, None, None) => result,
                _ = sigterm.recv() => return Ok(()),
            };
            #[cfg(not(unix))]
            let initial_connection = client::RpcClient::connect(socket, None, None).await;

            match initial_connection {
                Ok(c) => c,
                Err(e) if is_terminal_connection_error(&e) => return Err(e),
                Err(_) => {
                    let config_dir = client::resolve_config_dir(cli.config_dir.as_deref())?;
                    let mut daemon = spawn_owned_ephemeral_daemon(&config_dir, socket)?;
                    #[cfg(unix)]
                    let readiness = tokio::select! {
                        result = await_spawned_daemon_ready(socket, &mut daemon) => result,
                        _ = sigterm.recv() => {
                            return cleanup_spawned_daemon_after_signal(&mut daemon);
                        }
                    };
                    #[cfg(not(unix))]
                    let readiness = await_spawned_daemon_ready(socket, &mut daemon).await;

                    match readiness {
                        Ok(client) => {
                            owns_ephemeral =
                                reconcile_spawned_daemon_identity(client.server_pid, &mut daemon)?;
                            if owns_ephemeral {
                                daemon.detach();
                            }
                            client
                        }
                        Err(startup_error) => {
                            return Err(spawned_daemon_startup_failure(startup_error, &mut daemon));
                        }
                    }
                }
            }
        }
        ConnectTarget::Wss { url, skip_verify } => {
            if *skip_verify && !loaded_config.connection.wss.tls.route_acked(url) {
                apply_insecure_tls_choice(confirm_insecure_tls(url)?, &local_config_dir, url)?;
            }
            #[cfg(unix)]
            {
                tokio::select! {
                    result = client::RpcClient::connect_wss(url, None, None, *skip_verify) => result?,
                    _ = sigterm.recv() => return Ok(()),
                }
            }
            #[cfg(not(unix))]
            {
                client::RpcClient::connect_wss(url, None, None, *skip_verify).await?
            }
        }
    };

    let mut term = config_manager::init_terminal()?;
    TERMINAL_ACTIVE.store(true, Ordering::Relaxed);

    let result = run_until_exit(
        Arc::new(rpc),
        &mut term,
        &target,
        &local_config_dir,
        owns_ephemeral,
        #[cfg(unix)]
        &mut sigterm,
    )
    .await;

    TERMINAL_ACTIVE.store(false, Ordering::Relaxed);
    config_manager::restore_terminal(&mut term)?;
    result
}

/// Runs the TUI under a SIGTERM handler so the terminal is restored on
/// signal instead of dying mid-draw. `app::run` owns the full session
/// lifecycle — including in-loop reconnection and recovery — and returns
/// only when the user quits.
async fn run_until_exit(
    rpc: Arc<client::RpcClient>,
    term: &mut config_manager::Term,
    target: &ConnectTarget,
    config_dir: &std::path::Path,
    owns_ephemeral: bool,
    #[cfg(unix)] sigterm: &mut tokio::signal::unix::Signal,
) -> anyhow::Result<()> {
    // Shared state that survives a reconnect. Quickstart's Stage 2 writes
    // the new agent's alias here so the recovering `app::run` loop drops
    // the user into Chat once the daemon is back up.
    let reconnect_state: app::SharedReconnectState =
        Arc::new(std::sync::Mutex::new(app::CrossReconnectState::default()));

    let label = target.label();
    let insecure_tls = target.insecure_tls();

    #[cfg(unix)]
    {
        tokio::select! {
            r = app::run(rpc, term, &label, insecure_tls, reconnect_state, config_dir, target, owns_ephemeral) => r.map(|_| ()),
            _ = sigterm.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        app::run(
            rpc,
            term,
            &label,
            insecure_tls,
            reconnect_state,
            config_dir,
            target,
            owns_ephemeral,
        )
        .await
        .map(|_| ())
    }
}

pub(crate) fn spawn_ephemeral_daemon(
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) -> anyhow::Result<()> {
    let mut cmd = ephemeral_daemon_command(config_dir, socket);
    cmd.stderr(std::process::Stdio::null());
    cmd.spawn()
        .map_err(|e| anyhow::Error::msg(format!("failed to spawn daemon: {e}")))?;
    Ok(())
}

fn spawn_owned_ephemeral_daemon(
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) -> anyhow::Result<SpawnedDaemon> {
    SpawnedDaemon::spawn(ephemeral_daemon_command(config_dir, socket))
}

fn ephemeral_daemon_command(
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) -> std::process::Command {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("zeroclaw")))
        .unwrap_or_else(|| PathBuf::from("zeroclaw"));

    let mut cmd = std::process::Command::new(&exe);
    configure_ephemeral_daemon_command(&mut cmd, config_dir, socket);

    // Lower the daemon's log level to DEBUG when spawned ephemerally by
    // zerocode so that the Logs pane can show debug events without any
    // manual RUST_LOG override. Third-party crates stay at WARN to avoid
    // noise. Honour an existing RUST_LOG if the user set one themselves.
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env(
            "RUST_LOG",
            "debug,matrix_sdk=warn,matrix_sdk_base=warn,matrix_sdk_crypto=warn,\
             hyper=warn,reqwest=warn,tokio=warn,h2=warn",
        );
    }

    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    cmd
}

fn configure_ephemeral_daemon_command(
    cmd: &mut std::process::Command,
    config_dir: &std::path::Path,
    socket: &std::path::Path,
) {
    cmd.arg("daemon")
        .arg("--ephemeral")
        .arg("--config-dir")
        .arg(config_dir)
        // The TUI waits on this exact endpoint, so the child must bind it
        // instead of independently deriving a potentially different path.
        .env("ZEROCLAW_SOCKET", socket);
}

struct SpawnedDaemon {
    child: std::process::Child,
    stderr: Arc<Mutex<std::collections::VecDeque<u8>>>,
    capture_stderr: Arc<AtomicBool>,
    stderr_done: Option<std::sync::mpsc::Receiver<()>>,
    stderr_collector: Option<std::thread::JoinHandle<()>>,
    cleanup_on_drop: bool,
}

impl SpawnedDaemon {
    fn spawn(mut cmd: std::process::Command) -> anyhow::Result<Self> {
        use std::io::Read;

        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::Error::msg(format!("failed to spawn daemon: {e}")))?;
        let mut stderr_pipe = match child.stderr.take() {
            Some(pipe) => pipe,
            None => {
                let cleanup_error = terminate_child(&mut child).err();
                let mut message = "spawned daemon stderr pipe was unavailable".to_owned();
                if let Some(error) = cleanup_error {
                    message.push_str("; cleanup also failed: ");
                    message.push_str(&format!("{error:#}"));
                }
                return Err(anyhow::Error::msg(message));
            }
        };
        let stderr = Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(
            DAEMON_STDERR_LIMIT,
        )));
        let capture_stderr = Arc::new(AtomicBool::new(true));
        let collector_buffer = Arc::clone(&stderr);
        let collector_capture = Arc::clone(&capture_stderr);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let stderr_collector = match std::thread::Builder::new()
            .name("zerocode-daemon-stderr".to_owned())
            .spawn(move || {
                let mut chunk = [0_u8; 1024];
                while let Ok(read) = stderr_pipe.read(&mut chunk) {
                    if read == 0 {
                        break;
                    }
                    if !collector_capture.load(Ordering::Acquire) {
                        continue;
                    }
                    let mut buffer = collector_buffer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !collector_capture.load(Ordering::Acquire) {
                        continue;
                    }
                    buffer.extend(&chunk[..read]);
                    while buffer.len() > DAEMON_STDERR_LIMIT {
                        buffer.pop_front();
                    }
                }
                let _ = done_tx.send(());
            }) {
            Ok(collector) => collector,
            Err(spawn_error) => {
                let cleanup_error = terminate_child(&mut child).err();
                let mut message = format!("failed to start daemon stderr collector: {spawn_error}");
                if let Some(error) = cleanup_error {
                    message.push_str("; cleanup also failed: ");
                    message.push_str(&format!("{error:#}"));
                }
                return Err(anyhow::Error::msg(message));
            }
        };

        Ok(Self {
            child,
            stderr,
            capture_stderr,
            stderr_done: Some(done_rx),
            stderr_collector: Some(stderr_collector),
            cleanup_on_drop: true,
        })
    }

    #[cfg(test)]
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn poll_exit(&mut self) -> anyhow::Result<Option<SpawnedDaemonExit>> {
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        let stderr = self.finish_stderr_collection();
        Ok(Some(SpawnedDaemonExit { status, stderr }))
    }

    fn terminate_and_wait(&mut self) -> anyhow::Result<SpawnedDaemonExit> {
        let status = terminate_child(&mut self.child);
        let stderr = self.finish_stderr_collection();
        status.map(|status| SpawnedDaemonExit { status, stderr })
    }

    fn finish_stderr_collection(&mut self) -> String {
        if let Some(done) = self.stderr_done.take() {
            let _ = done.recv_timeout(Duration::from_millis(100));
        }
        self.capture_stderr.store(false, Ordering::Release);

        let mut buffer = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = buffer.drain(..).collect::<Vec<_>>();
        drop(buffer);

        self.stderr_collector.take();
        sanitize_daemon_stderr(&bytes)
    }

    fn detach(mut self) {
        self.cleanup_on_drop = false;
        self.capture_stderr.store(false, Ordering::Release);
        self.stderr_done.take();
        self.stderr_collector.take();
    }
}

impl Drop for SpawnedDaemon {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = self.terminate_and_wait();
        }
    }
}

fn terminate_child(child: &mut std::process::Child) -> anyhow::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }

    if let Err(kill_error) = child.kill() {
        return match child.try_wait() {
            Ok(Some(status)) => Ok(status),
            Ok(None) => Err(anyhow::Error::msg(format!(
                "failed to terminate daemon: {kill_error}"
            ))),
            Err(poll_error) => Err(anyhow::Error::msg(format!(
                "failed to terminate daemon: {kill_error}; failed to re-check daemon: {poll_error}"
            ))),
        };
    }

    child
        .wait()
        .map_err(|error| anyhow::Error::msg(format!("failed to reap daemon: {error}")))
}

fn sanitize_daemon_stderr(bytes: &[u8]) -> String {
    let mut rendered = String::new();
    let decoded = String::from_utf8_lossy(bytes);
    let mut characters = decoded.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' if characters.peek() == Some(&'\n') => {
                characters.next();
                rendered.push('\n');
            }
            '\r' => rendered.push('\u{fffd}'),
            '\n' | '\t' => rendered.push(character),
            character if character.is_control() => rendered.push('\u{fffd}'),
            character => rendered.push(character),
        }
    }

    if rendered.len() <= DAEMON_STDERR_LIMIT {
        return rendered;
    }
    let mut start = rendered.len() - DAEMON_STDERR_LIMIT;
    while !rendered.is_char_boundary(start) {
        start += 1;
    }
    rendered[start..].to_owned()
}

fn reconcile_spawned_daemon_identity(
    server_pid: Option<u32>,
    daemon: &mut SpawnedDaemon,
) -> anyhow::Result<bool> {
    if server_pid == Some(daemon.id()) {
        return Ok(true);
    }

    let spawned_pid = daemon.id();
    daemon.terminate_and_wait().map_err(|cleanup_error| {
        anyhow::Error::msg(format!(
            "connected to daemon pid {}, but failed to clean up spawned daemon pid {}: {cleanup_error:#}",
            server_pid.map_or_else(|| "unknown".to_owned(), |pid| pid.to_string()),
            spawned_pid,
        ))
    })?;

    if server_pid.is_none() {
        anyhow::bail!(
            "spawned daemon did not report its process id during initialization; cleaned up pid {spawned_pid}"
        );
    }
    Ok(false)
}

#[cfg(unix)]
fn cleanup_spawned_daemon_after_signal(daemon: &mut SpawnedDaemon) -> anyhow::Result<()> {
    daemon
        .terminate_and_wait()
        .map(|_| ())
        .map_err(|cleanup_error| {
            anyhow::Error::msg(format!(
                "received SIGTERM while starting daemon; cleanup failed: {cleanup_error:#}"
            ))
        })
}

#[derive(Debug)]
struct SpawnedDaemonExit {
    status: ExitStatus,
    stderr: String,
}

impl SpawnedDaemonExit {
    #[cfg(test)]
    fn stderr(&self) -> &str {
        &self.stderr
    }
}

impl std::fmt::Display for SpawnedDaemonExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "daemon exited before ready (status: {})",
            self.status
        )?;
        if !self.stderr.trim().is_empty() {
            write!(formatter, "; stderr: {}", self.stderr.trim())?;
        }
        Ok(())
    }
}

impl std::error::Error for SpawnedDaemonExit {}

#[derive(Debug)]
struct SpawnedDaemonStartupFailure {
    details: String,
}

impl SpawnedDaemonStartupFailure {
    fn details(&self) -> &str {
        &self.details
    }
}

impl std::fmt::Display for SpawnedDaemonStartupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.details)
    }
}

impl std::error::Error for SpawnedDaemonStartupFailure {}

fn spawned_daemon_startup_failure(
    startup_error: anyhow::Error,
    daemon: &mut SpawnedDaemon,
) -> anyhow::Error {
    let startup_exit = startup_error.downcast_ref::<SpawnedDaemonExit>();
    let mut details = if let Some(exit) = startup_exit {
        format!("daemon exited before ready (status: {})", exit.status)
    } else {
        format_startup_error(&startup_error)
    };

    let cleanup = daemon.terminate_and_wait();
    let stderr = startup_exit
        .map(|exit| exit.stderr.as_str())
        .filter(|stderr| !stderr.trim().is_empty())
        .or_else(|| {
            cleanup
                .as_ref()
                .ok()
                .map(|exit| exit.stderr.as_str())
                .filter(|stderr| !stderr.trim().is_empty())
        })
        .unwrap_or_default();
    if !stderr.trim().is_empty() {
        details.push_str("; stderr: ");
        details.push_str(stderr.trim());
    }
    if let Err(error) = cleanup {
        details.push_str("; cleanup also failed: ");
        details.push_str(&format!("{error:#}"));
    }

    anyhow::Error::new(SpawnedDaemonStartupFailure { details })
}

async fn await_spawned_daemon_ready(
    socket: &std::path::Path,
    daemon: &mut SpawnedDaemon,
) -> anyhow::Result<client::RpcClient> {
    let deadline = tokio::time::Instant::now() + SPAWNED_DAEMON_CONNECT_TIMEOUT;
    loop {
        if let Some(exit) = daemon.poll_exit()? {
            return Err(anyhow::Error::new(exit));
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "daemon did not become ready within {}s (socket: {})",
                SPAWNED_DAEMON_CONNECT_TIMEOUT.as_secs(),
                socket.display(),
            );
        }
        match client::RpcClient::connect(socket, None, None).await {
            Ok(c) => return Ok(c),
            Err(e) if is_terminal_connection_error(&e) => return Err(e),
            Err(_) => tokio::time::sleep(DAEMON_CONNECT_INTERVAL).await,
        }
    }
}

fn is_daemon_version_mismatch(err: &anyhow::Error) -> bool {
    err.downcast_ref::<client::DaemonVersionMismatch>()
        .is_some()
}

fn is_terminal_connection_error(err: &anyhow::Error) -> bool {
    is_daemon_version_mismatch(err)
        || err
            .downcast_ref::<client::DaemonInitializeTimeout>()
            .is_some()
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::config::WssSection;
    use std::ffi::OsStr;

    fn spawned_daemon_helper_command(mode: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new(
            std::env::current_exe().expect("current zerocode test binary path"),
        );
        cmd.args([
            "connection_tests::spawned_daemon_subprocess_helper",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("ZEROCODE_SPAWNED_DAEMON_HELPER", mode);
        cmd
    }

    #[test]
    fn spawned_daemon_cleanup_terminates_and_reaps_running_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");
        assert!(daemon.try_wait().expect("poll helper").is_none());

        let exit = daemon.terminate_and_wait().expect("terminate helper");

        assert!(!exit.status.success());
        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[test]
    fn spawned_daemon_early_exit_reports_bounded_stderr() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("stderr")).expect("spawn helper");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let exit = loop {
            if let Some(exit) = daemon.poll_exit().expect("poll helper") {
                break exit;
            }
            assert!(std::time::Instant::now() < deadline, "helper did not exit");
            std::thread::sleep(Duration::from_millis(10));
        };
        let rendered = exit.to_string();

        assert!(rendered.contains("status"));
        assert!(rendered.contains("spawned-daemon-stderr-tail"));
        assert!(exit.stderr().len() <= DAEMON_STDERR_LIMIT);
    }

    #[test]
    fn spawned_daemon_exit_does_not_wait_for_inherited_stderr() {
        let mut exercise = spawned_daemon_helper_command("exercise-inherited-stderr")
            .spawn()
            .expect("spawn inherited-stderr exercise");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);

        loop {
            if let Some(status) = exercise.try_wait().expect("poll exercise") {
                assert!(status.success(), "exercise failed with {status}");
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = exercise.kill();
                let _ = exercise.wait();
                panic!("poll_exit blocked while a descendant held stderr open");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn spawned_daemon_stderr_is_rendered_safely_within_limit() {
        let mut daemon = SpawnedDaemon::spawn(spawned_daemon_helper_command("unsafe-stderr"))
            .expect("spawn helper");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let exit = loop {
            if let Some(exit) = daemon.poll_exit().expect("poll helper") {
                break exit;
            }
            assert!(std::time::Instant::now() < deadline, "helper did not exit");
            std::thread::sleep(Duration::from_millis(10));
        };

        assert!(exit.stderr().len() <= DAEMON_STDERR_LIMIT);
        assert!(!exit.stderr().contains('\u{1b}'));
        assert!(!exit.stderr().contains('\0'));
        assert!(!exit.stderr().contains('\r'));
        assert!(exit.stderr().contains("unsafe-stderr-tail"));
    }

    #[test]
    fn spawned_daemon_stderr_normalizes_crlf_and_replaces_bare_carriage_returns() {
        assert_eq!(
            sanitize_daemon_stderr(b"first\r\nsecond\roverwrite"),
            "first\nsecond\u{fffd}overwrite"
        );
    }

    #[test]
    fn spawned_daemon_mismatched_identity_terminates_and_reaps_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");
        let spawned_pid = daemon.id();

        let owns_ephemeral =
            reconcile_spawned_daemon_identity(Some(spawned_pid.wrapping_add(1)), &mut daemon)
                .expect("clean up mismatched child");

        assert!(!owns_ephemeral);
        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[test]
    fn spawned_daemon_missing_identity_fails_closed_after_reaping_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");

        let error = reconcile_spawned_daemon_identity(None, &mut daemon)
            .expect_err("missing identity must not transfer ownership");

        assert!(error.to_string().contains("did not report its process id"));
        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[test]
    fn spawned_daemon_matching_identity_retains_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");

        let owns_ephemeral = reconcile_spawned_daemon_identity(Some(daemon.id()), &mut daemon)
            .expect("accept matching child");

        assert!(owns_ephemeral);
        assert!(daemon.try_wait().expect("poll helper").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn spawned_daemon_startup_signal_cleanup_terminates_and_reaps_child() {
        let mut daemon =
            SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep")).expect("spawn helper");

        cleanup_spawned_daemon_after_signal(&mut daemon).expect("clean up signalled child");

        assert!(daemon.try_wait().expect("poll reaped helper").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn spawned_daemon_parent_only_sigterm_cleans_up_child() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let pid_path = temp.path().join("daemon.pid");
        let mut owner = spawned_daemon_helper_command("signal-owner");
        owner.env("ZEROCODE_SIGNAL_OWNER_PID_PATH", &pid_path);
        let mut owner = owner.spawn().expect("spawn signal owner");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let daemon_pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_path) {
                break pid.trim().parse::<u32>().expect("parse daemon pid");
            }
            assert!(
                owner.try_wait().expect("poll signal owner").is_none(),
                "signal owner exited before publishing daemon pid"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "signal owner did not publish daemon pid"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        let signal_result = unsafe { libc::kill(owner.id() as libc::pid_t, libc::SIGTERM) };
        assert_eq!(
            signal_result,
            0,
            "send SIGTERM: {}",
            std::io::Error::last_os_error()
        );
        assert!(owner.wait().expect("wait signal owner").success());

        let child_probe = unsafe { libc::kill(daemon_pid as libc::pid_t, 0) };
        assert_eq!(child_probe, -1, "spawned daemon pid {daemon_pid} survived");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn spawned_daemon_readiness_allows_cold_start_window() {
        assert_eq!(SPAWNED_DAEMON_CONNECT_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    #[ignore = "subprocess helper for spawned-daemon lifecycle tests"]
    fn spawned_daemon_subprocess_helper() {
        match std::env::var("ZEROCODE_SPAWNED_DAEMON_HELPER").as_deref() {
            Ok("sleep") => std::thread::sleep(Duration::from_secs(60)),
            Ok("sleep-short") => std::thread::sleep(Duration::from_secs(3)),
            Ok("stderr") => {
                eprint!("{}", "x".repeat(DAEMON_STDERR_LIMIT * 2));
                eprintln!("spawned-daemon-stderr-tail");
                std::process::exit(23);
            }
            Ok("unsafe-stderr") => {
                use std::io::Write;

                let mut stderr = std::io::stderr().lock();
                stderr
                    .write_all(&vec![0xff; DAEMON_STDERR_LIMIT * 2])
                    .expect("write invalid stderr");
                stderr
                    .write_all(b"\x1b[2J\0\runsafe-stderr-tail\r\n")
                    .expect("write control stderr");
                std::process::exit(23);
            }
            Ok("stderr-descendant") => {
                spawned_daemon_helper_command("sleep-short")
                    .spawn()
                    .expect("spawn stderr-inheriting descendant");
                eprintln!("stderr-descendant-parent-exit");
                std::process::exit(23);
            }
            Ok("exercise-inherited-stderr") => {
                let mut daemon =
                    SpawnedDaemon::spawn(spawned_daemon_helper_command("stderr-descendant"))
                        .expect("spawn stderr-descendant helper");
                let deadline = std::time::Instant::now() + Duration::from_secs(1);
                loop {
                    if daemon
                        .poll_exit()
                        .expect("poll stderr-descendant")
                        .is_some()
                    {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "stderr-descendant helper did not exit"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            #[cfg(unix)]
            Ok("signal-owner") => {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build signal runtime");
                runtime.block_on(async {
                    let mut sigterm =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("install SIGTERM handler");
                    let mut daemon = SpawnedDaemon::spawn(spawned_daemon_helper_command("sleep"))
                        .expect("spawn owned daemon helper");
                    let pid_path = std::env::var_os("ZEROCODE_SIGNAL_OWNER_PID_PATH")
                        .expect("signal owner pid path");
                    std::fs::write(pid_path, daemon.id().to_string())
                        .expect("publish owned daemon pid");
                    sigterm.recv().await;
                    cleanup_spawned_daemon_after_signal(&mut daemon)
                        .expect("clean up signalled daemon");
                });
            }
            other => panic!("unexpected helper mode: {other:?}"),
        }
    }

    #[test]
    fn ephemeral_daemon_command_sets_selected_socket() {
        let mut cmd = std::process::Command::new("zeroclaw");
        configure_ephemeral_daemon_command(
            &mut cmd,
            std::path::Path::new("/tmp/zeroclaw-config"),
            std::path::Path::new("/tmp/zeroclaw.sock"),
        );

        assert_eq!(
            cmd.get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "daemon",
                "--ephemeral",
                "--config-dir",
                "/tmp/zeroclaw-config",
            ]
        );
        assert_eq!(
            cmd.get_envs()
                .find(|(name, _)| *name == OsStr::new("ZEROCLAW_SOCKET"))
                .and_then(|(_, value)| value),
            Some(OsStr::new("/tmp/zeroclaw.sock"))
        );
    }

    #[test]
    fn flag_connect_overrides_config_uri() {
        let cfg = WssSection {
            uri: Some("wss://config:1".to_string()),
            ..Default::default()
        };
        let got = resolve_wss_target(Some("wss://flag:2".to_string()), false, &cfg);
        assert_eq!(got, Some(("wss://flag:2".to_string(), false)));
    }

    #[test]
    fn config_uri_used_when_no_flag() {
        let cfg = WssSection {
            uri: Some("wss://config:1".to_string()),
            ..Default::default()
        };
        let got = resolve_wss_target(None, false, &cfg);
        assert_eq!(got, Some(("wss://config:1".to_string(), false)));
    }

    #[test]
    fn no_uri_anywhere_is_local_socket() {
        let cfg = WssSection::default();
        assert_eq!(resolve_wss_target(None, false, &cfg), None);
    }

    #[test]
    fn skip_verify_is_flag_or_config() {
        let mut cfg = WssSection {
            uri: Some("wss://h:1".to_string()),
            ..Default::default()
        };
        cfg.tls.skip_verify = true;
        assert_eq!(
            resolve_wss_target(None, false, &cfg),
            Some(("wss://h:1".to_string(), true))
        );
        cfg.tls.skip_verify = false;
        assert_eq!(
            resolve_wss_target(None, true, &cfg),
            Some(("wss://h:1".to_string(), true))
        );
        assert_eq!(
            resolve_wss_target(None, false, &cfg),
            Some(("wss://h:1".to_string(), false))
        );
    }

    #[test]
    fn initialize_timeout_is_a_terminal_connection_error() {
        let err = anyhow::Error::new(client::DaemonInitializeTimeout::new(Duration::from_secs(
            10,
        )));

        assert!(is_terminal_connection_error(&err));
    }
}

#[cfg(test)]
mod confirm_insecure_tls_tests {
    //! Tests for [`crate::confirm_insecure_tls_with`], the test-seam
    //! extracted from the original `confirm_insecure_tls(url)` so the
    //! input → choice mapping and prompt content can be asserted
    //! deterministically without touching `stdin` / `stderr`.
    //!
    //! Insecure-TLS acceptance criterion coverage:
    //! 1. "Insecure TLS cannot be accepted without explicit confirmation"
    //!    — the empty / `n` / junk / uppercase-`N` / default branches all
    //!    return [`InsecureTlsChoice::Abort`].
    //! 2. "Decline/abort paths leave no persisted insecure-TLS choice"
    //!    — covered behaviorally by `apply_insecure_tls_choice_tests`,
    //!    which executes the production choice -> side-effect seam
    //!    (`crate::apply_insecure_tls_choice`) for `Once`, `Always`, and
    //!    `Abort` and asserts persistence only happens for `Always`.
    //! 3. "Mode transition tests cover the quickstart/chat handoff" is
    //!    covered by the existing `connection_tests::flag_connect_*` /
    //!    `config_uri_*` / `skip_verify_*` tests; this issue does not
    //!    change `resolve_wss_target`'s contract.
    //! 4. "prompt persistence behavior needed to test those transitions
    //!    deterministically" is covered by the existing
    //!    `route_acked_membership` / `persist_wss_route_ack_dedups` /
    //!    `persist_wss_route_ack_preserves_other_sections` tests in
    //!    `crate::config` — this issue does not duplicate that coverage.

    use super::InsecureTlsChoice::{Abort, Always, Once};
    use super::*;
    use std::io::Cursor;

    /// Drive [`confirm_insecure_tls_with`] with a deterministic stdin
    /// buffer and a fresh output buffer, returning the operator's
    /// choice and the captured prompt text.
    fn run(input: &str, url: &str) -> (InsecureTlsChoice, String) {
        let mut output = Vec::new();
        let choice = confirm_insecure_tls_with(Cursor::new(input), &mut output, url)
            .expect("confirm_insecure_tls_with must succeed on plain stdin read");
        let stderr = String::from_utf8(output).expect("prompt must be valid UTF-8");
        (choice, stderr)
    }

    #[test]
    fn confirm_input_y_returns_once() {
        assert!(matches!(run("y\n", "wss://example.test:1").0, Once));
    }

    #[test]
    fn confirm_input_yes_returns_once() {
        assert!(matches!(run("yes\n", "wss://example.test:1").0, Once));
    }

    #[test]
    fn confirm_input_a_returns_always() {
        assert!(matches!(run("a\n", "wss://example.test:1").0, Always));
    }

    #[test]
    fn confirm_input_always_returns_always() {
        assert!(matches!(run("always\n", "wss://example.test:1").0, Always));
    }

    #[test]
    fn confirm_input_n_returns_abort() {
        assert!(matches!(run("n\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_input_empty_returns_abort() {
        // Acceptance: insecure TLS cannot be accepted without explicit
        // confirmation. An empty stdin (e.g. operator hits enter without
        // typing) must default-decline.
        assert!(matches!(run("\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_input_junk_returns_abort() {
        // Acceptance: unknown input must default to the safe Abort
        // branch — only `y` / `yes` / `a` / `always` may opt into
        // verification-disabled transport.
        assert!(matches!(run("xyz\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_input_uppercase_lowercases_before_match() {
        // The match arm uses `to_ascii_lowercase()` so case variations
        // resolve identically. This is the seam's contract; pin both
        // "Once" and "Always" branches to defend against an
        // accidental case-sensitive refactor.
        assert!(matches!(run("Y\n", "wss://example.test:1").0, Once));
        assert!(matches!(run("YES\n", "wss://example.test:1").0, Once));
        assert!(matches!(run("ALWAYS\n", "wss://example.test:1").0, Always));
        // Uppercase `N` and `NO` must still resolve to Abort — they
        // are not in the affirmative set.
        assert!(matches!(run("N\n", "wss://example.test:1").0, Abort));
        assert!(matches!(run("NO\n", "wss://example.test:1").0, Abort));
    }

    #[test]
    fn confirm_prompt_writes_url_and_choice_menu_to_writer() {
        // The operator must see (a) which URL they are accepting
        // insecure-TLS for, and (b) the `[y/a/N]` choice menu, before
        // any answer is read. Capture the prompt text and pin both
        // invariants so a future refactor cannot silently truncate the
        // warning or the menu.
        let url = "wss://insecure-host.example:8443";
        let (_, stderr) = run("n\n", url);
        assert!(
            stderr.contains(url),
            "stderr prompt must contain the URL being confirmed; got: {stderr}"
        );
        assert!(
            stderr.contains("[y/a/N]"),
            "stderr prompt must show the y/a/N choice menu; got: {stderr}"
        );
        assert!(
            stderr.contains("WARNING"),
            "stderr prompt must lead with a WARNING banner so the \
             operator does not skim past an insecure-TLS confirmation; \
             got: {stderr}"
        );
    }
}

#[cfg(test)]
mod apply_insecure_tls_choice_tests {
    //! Behavior-level tests for [`crate::apply_insecure_tls_choice`], the
    //! test seam extracted from the `Once` / `Always` / `Abort` match
    //! that used to live inline in `run()`. These tests execute the
    //! production choice -> side-effect path directly (no source-text
    //! inspection) against a temporary config directory:
    //!
    //! - `Once` must not persist a route acknowledgement.
    //! - `Always` must persist the confirmed route, and only that route.
    //! - `Abort` must return the exact production error and persist
    //!   nothing.
    //! - `Always` must not disturb unrelated, pre-existing config
    //!   sections on disk.

    use super::*;

    const URL: &str = "wss://example.invalid:8443/";

    #[test]
    fn once_does_not_persist() {
        let dir = tempfile::tempdir().unwrap();
        apply_insecure_tls_choice(InsecureTlsChoice::Once, dir.path(), URL)
            .expect("Once must not error");
        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            !cfg.connection.wss.tls.route_acked(URL),
            "Once must leave no route acknowledgement behind"
        );
    }

    #[test]
    fn always_persists_exact_route() {
        let dir = tempfile::tempdir().unwrap();
        apply_insecure_tls_choice(InsecureTlsChoice::Always, dir.path(), URL)
            .expect("Always must not error");
        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            cfg.connection.wss.tls.route_acked(URL),
            "Always must persist the confirmed route"
        );
        assert_eq!(
            cfg.connection.wss.tls.skip_verify_routes,
            vec![URL.to_string()],
            "Always must store exactly the confirmed route, unmutated"
        );
    }

    #[test]
    fn abort_returns_error_and_persists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = apply_insecure_tls_choice(InsecureTlsChoice::Abort, dir.path(), URL)
            .expect_err("Abort must return an error");
        assert!(
            err.to_string()
                .contains("aborted: insecure TLS connection not confirmed"),
            "unexpected error message: {err}"
        );
        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            !cfg.connection.wss.tls.route_acked(URL),
            "Abort must leave no route acknowledgement behind"
        );
    }

    #[test]
    fn always_preserves_unrelated_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            config::config_path(dir.path()),
            "[theme]\nname = \"nord\"\n\n[future]\nkeep = true\n",
        )
        .unwrap();

        apply_insecure_tls_choice(InsecureTlsChoice::Always, dir.path(), URL)
            .expect("Always must not error");

        let on_disk = std::fs::read_to_string(config::config_path(dir.path())).unwrap();
        let doc: toml::Table = toml::from_str(&on_disk).unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("nord"));
        assert_eq!(doc["future"]["keep"].as_bool(), Some(true));

        let cfg = config::ensure_and_load(dir.path()).unwrap();
        assert!(
            cfg.connection.wss.tls.route_acked(URL),
            "Always must persist the confirmed route alongside unrelated sections"
        );
    }
}
