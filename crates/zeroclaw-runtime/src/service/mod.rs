use anyhow::{Context, Result, bail};
#[cfg(any(target_os = "macos", test))]
use std::collections::VecDeque;
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(any(target_os = "macos", test))]
use std::process::Stdio;
use std::str::FromStr;
#[cfg(any(target_os = "macos", test))]
use std::sync::{Arc, Condvar, Mutex};
#[cfg(any(target_os = "macos", test))]
use std::thread::JoinHandle;
#[cfg(any(target_os = "macos", test))]
use std::time::{Duration, Instant};
#[cfg(any(target_os = "macos", test))]
use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(any(target_os = "macos", test))]
use tokio::process::{Child, Command as TokioCommand};
use zeroclaw_config::schema::Config;

const SERVICE_LABEL: &str = "com.zeroclaw.daemon";
const WINDOWS_TASK_NAME: &str = "ZeroClaw Daemon";
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const SERVICE_LOG_MAX_BYTES: u64 = 8 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const SERVICE_LOG_COMPACT_BYTES: u64 = 4 * 1024 * 1024;
#[cfg(any(target_os = "linux", test))]
const OPENRC_STDOUT_LOG: &str = "/var/log/zeroclaw/access.log";
#[cfg(any(target_os = "linux", test))]
const OPENRC_STDERR_LOG: &str = "/var/log/zeroclaw/error.log";
#[cfg(any(target_os = "macos", test))]
const LAUNCHD_LOG_PENDING_BYTES: usize = 1024 * 1024;
#[cfg(any(target_os = "macos", test))]
const LAUNCHD_STOP_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(any(target_os = "macos", test))]
const LAUNCHD_PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct LaunchdCapturePaths {
    stdout: PathBuf,
    stderr: PathBuf,
}

#[cfg(any(target_os = "macos", test))]
fn launchd_capture_paths(config_dir: &Path) -> LaunchdCapturePaths {
    let logs = config_dir.join("logs");
    LaunchdCapturePaths {
        stdout: logs.join("daemon.stdout.log"),
        stderr: logs.join("daemon.stderr.log"),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
struct BoundedServiceLog {
    file: fs::File,
    len: u64,
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
impl BoundedServiceLog {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create log directory {}", parent.display()))?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("Failed to open service log {}", path.display()))?;
        let mut log = Self {
            len: file.metadata()?.len(),
            file,
        };
        if log.len > SERVICE_LOG_MAX_BYTES {
            log.retain_tail(SERVICE_LOG_MAX_BYTES)?;
        }
        Ok(log)
    }

    fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        if chunk.len() as u64 >= SERVICE_LOG_MAX_BYTES {
            let start = chunk.len() - SERVICE_LOG_MAX_BYTES as usize;
            return self.rewrite(&chunk[start..]);
        }
        if self.len + chunk.len() as u64 > SERVICE_LOG_MAX_BYTES {
            let headroom = SERVICE_LOG_MAX_BYTES - chunk.len() as u64;
            self.retain_tail(SERVICE_LOG_COMPACT_BYTES.min(headroom))?;
        }
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(chunk)?;
        self.len += chunk.len() as u64;
        Ok(())
    }

    fn retain_tail(&mut self, keep: u64) -> Result<()> {
        let keep = keep.min(self.len);
        let mut tail = vec![0; keep as usize];
        self.file.seek(SeekFrom::End(-(keep as i64)))?;
        self.file.read_exact(&mut tail)?;
        self.rewrite(&tail)
    }

    fn rewrite(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(bytes)?;
        self.file.set_len(bytes.len() as u64)?;
        self.file.flush()?;
        self.len = bytes.len() as u64;
        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
fn drain_bounded_service_log<R: Read>(mut input: R, path: &Path) -> Result<()> {
    let mut log = match BoundedServiceLog::open(path) {
        Ok(log) => Some(log),
        Err(error) => {
            let mut buffer = [0_u8; 16 * 1024];
            while input.read(&mut buffer)? != 0 {}
            return Err(error);
        }
    };
    let mut first_write_error = None;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .context("Failed to read service log input")?;
        if read == 0 {
            break;
        }
        if let Some(writer) = log.as_mut()
            && let Err(error) = writer.write_chunk(&buffer[..read])
        {
            first_write_error = Some(error.context(format!(
                "Failed to write bounded service log {}",
                path.display()
            )));
            log = None;
        }
    }
    if let Some(error) = first_write_error {
        return Err(error);
    }
    Ok(())
}

pub fn run_openrc_log_writer(stderr: bool) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stderr;
        bail!("the OpenRC log writer is only supported on Linux")
    }

    #[cfg(target_os = "linux")]
    {
        drain_bounded_service_log(std::io::stdin().lock(), openrc_log_path(stderr))
    }
}

#[cfg(any(target_os = "linux", test))]
fn openrc_log_path(stderr: bool) -> &'static Path {
    Path::new(if stderr {
        OPENRC_STDERR_LOG
    } else {
        OPENRC_STDOUT_LOG
    })
}

#[cfg(any(target_os = "macos", test))]
struct PendingLaunchdLog {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    closed: bool,
}

#[cfg(any(target_os = "macos", test))]
struct LaunchdLogSinkInner {
    pending: Mutex<PendingLaunchdLog>,
    ready: Condvar,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone)]
struct LaunchdLogSink(Arc<LaunchdLogSinkInner>);

#[cfg(any(target_os = "macos", test))]
impl LaunchdLogSink {
    fn push(&self, mut chunk: Vec<u8>) {
        if chunk.len() > LAUNCHD_LOG_PENDING_BYTES {
            chunk = chunk.split_off(chunk.len() - LAUNCHD_LOG_PENDING_BYTES);
        }
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.closed {
            return;
        }
        while pending.bytes + chunk.len() > LAUNCHD_LOG_PENDING_BYTES {
            let Some(discarded) = pending.chunks.pop_front() else {
                break;
            };
            pending.bytes -= discarded.len();
        }
        pending.bytes += chunk.len();
        pending.chunks.push_back(chunk);
        self.0.ready.notify_one();
    }

    fn close(&self) {
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.closed = true;
        self.0.ready.notify_one();
    }
}

#[cfg(any(target_os = "macos", test))]
struct LaunchdCaptureWriters {
    stdout: LaunchdLogSink,
    stderr: LaunchdLogSink,
    tasks: Vec<JoinHandle<()>>,
}

#[cfg(any(target_os = "macos", test))]
impl LaunchdCaptureWriters {
    fn open(paths: &LaunchdCapturePaths) -> Result<Self> {
        let stdout_log = BoundedServiceLog::open(&paths.stdout)?;
        let stderr_log = BoundedServiceLog::open(&paths.stderr)?;
        let (stdout, stdout_task) = spawn_launchd_log_writer(paths.stdout.clone(), stdout_log);
        let (stderr, stderr_task) = spawn_launchd_log_writer(paths.stderr.clone(), stderr_log);
        Ok(Self {
            stdout,
            stderr,
            tasks: vec![stdout_task, stderr_task],
        })
    }

    async fn finish(mut self) {
        self.stdout.close();
        self.stderr.close();
        let deadline = Instant::now() + LAUNCHD_PIPE_DRAIN_TIMEOUT;
        for task in self.tasks.drain(..) {
            while !task.is_finished() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if task.is_finished() {
                let _ = task.join();
            }
        }
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for LaunchdCaptureWriters {
    fn drop(&mut self) {
        self.stdout.close();
        self.stderr.close();
    }
}

#[cfg(any(target_os = "macos", test))]
fn spawn_launchd_log_writer(
    path: PathBuf,
    mut log: BoundedServiceLog,
) -> (LaunchdLogSink, JoinHandle<()>) {
    let inner = Arc::new(LaunchdLogSinkInner {
        pending: Mutex::new(PendingLaunchdLog {
            chunks: VecDeque::new(),
            bytes: 0,
            closed: false,
        }),
        ready: Condvar::new(),
    });
    let sink = LaunchdLogSink(Arc::clone(&inner));
    let task = std::thread::spawn(move || {
        let mut writable = true;
        loop {
            let chunk = {
                let mut pending = inner.pending.lock().unwrap_or_else(|e| e.into_inner());
                while pending.chunks.is_empty() && !pending.closed {
                    pending = inner.ready.wait(pending).unwrap_or_else(|e| e.into_inner());
                }
                let chunk = pending.chunks.pop_front();
                if let Some(ref chunk) = chunk {
                    pending.bytes -= chunk.len();
                } else if pending.closed {
                    break;
                }
                chunk
            };
            let Some(chunk) = chunk else {
                continue;
            };
            if writable && let Err(error) = log.write_chunk(&chunk) {
                eprintln!(
                    "launchd log write failed for {}; continuing without capture: {error:#}",
                    path.display()
                );
                writable = false;
            }
        }
    });
    (sink, task)
}

#[cfg(any(target_os = "macos", test))]
async fn drain_launchd_pipe<R>(mut pipe: R, sink: LaunchdLogSink)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0; 16 * 1024];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => sink.push(buffer[..read].to_vec()),
            Err(error) => {
                sink.push(format!("launchd log pipe read failed: {error}\n").into_bytes());
                break;
            }
        }
    }
}

pub async fn run_launchd_daemon(config_dir: &Path) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config_dir;
        bail!("the launchd daemon runner is only supported on macOS")
    }

    #[cfg(target_os = "macos")]
    {
        let paths = launchd_capture_paths(config_dir);
        run_with_launchd_capture(paths, || {
            let executable = std::env::current_exe()
                .context("Failed to resolve the launchd daemon executable")?;
            let mut command = TokioCommand::new(executable);
            command.arg("--config-dir").arg(config_dir).arg("daemon");
            Ok(command)
        })
        .await
    }
}

#[cfg(any(target_os = "macos", test))]
async fn run_with_launchd_capture<F>(paths: LaunchdCapturePaths, make_command: F) -> Result<()>
where
    F: FnOnce() -> Result<TokioCommand>,
{
    let writers = LaunchdCaptureWriters::open(&paths)?;
    let result = async {
        let command = make_command()?;
        supervise_launchd_child(command, &writers).await
    }
    .await;
    if let Err(error) = &result {
        writers
            .stderr
            .push(format!("launchd capture failed: {error:#}\n").into_bytes());
    }
    writers.finish().await;
    result
}

#[cfg(any(target_os = "macos", test))]
async fn supervise_launchd_child(
    mut command: TokioCommand,
    writers: &LaunchdCaptureWriters,
) -> Result<()> {
    #[cfg(any(target_os = "macos", all(test, unix)))]
    let mut signals = LaunchdSignals::new()?;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("Failed to start daemon child")?;
    let stdout = child
        .stdout
        .take()
        .context("daemon stdout pipe unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("daemon stderr pipe unavailable")?;
    let stdout_sink = writers.stdout.clone();
    let stderr_sink = writers.stderr.clone();
    let stdout_task = zeroclaw_spawn::spawn!(drain_launchd_pipe(stdout, stdout_sink));
    let stderr_task = zeroclaw_spawn::spawn!(drain_launchd_pipe(stderr, stderr_sink));

    #[cfg(any(target_os = "macos", all(test, unix)))]
    let outcome = wait_for_launchd_child(&mut child, &mut signals).await;
    #[cfg(all(test, not(unix)))]
    let outcome = wait_for_launchd_child(&mut child).await;
    finish_launchd_pipes(stdout_task, stderr_task).await;
    let status = outcome?;
    if status.success() {
        Ok(())
    } else {
        bail!("daemon child exited with status {status}")
    }
}

#[cfg(any(target_os = "macos", test))]
async fn finish_launchd_pipes(
    mut stdout: tokio::task::JoinHandle<()>,
    mut stderr: tokio::task::JoinHandle<()>,
) {
    if tokio::time::timeout(LAUNCHD_PIPE_DRAIN_TIMEOUT, async {
        let _ = tokio::join!(&mut stdout, &mut stderr);
    })
    .await
    .is_err()
    {
        stdout.abort();
        stderr.abort();
        let _ = tokio::join!(stdout, stderr);
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
struct LaunchdSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(any(target_os = "macos", all(test, unix)))]
impl LaunchdSignals {
    fn new() -> Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
async fn wait_for_launchd_child(
    child: &mut Child,
    signals: &mut LaunchdSignals,
) -> Result<std::process::ExitStatus> {
    let forwarded = tokio::select! {
        status = child.wait() => return status.context("Failed to wait for daemon child"),
        _ = signals.interrupt.recv() => libc::SIGINT,
        _ = signals.terminate.recv() => libc::SIGTERM,
    };
    stop_launchd_child(child, forwarded).await
}

#[cfg(any(target_os = "macos", all(test, unix)))]
async fn stop_launchd_child(
    child: &mut Child,
    signal: libc::c_int,
) -> Result<std::process::ExitStatus> {
    if let Err(error) = forward_launchd_signal(child, signal)
        && error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
            != Some(libc::ESRCH)
    {
        return Err(error);
    }

    match tokio::time::timeout(LAUNCHD_STOP_TIMEOUT, child.wait()).await {
        Ok(status) => status.context("Failed to wait for stopped daemon child"),
        Err(_) => {
            if let Some(status) = child
                .try_wait()
                .context("Failed to inspect daemon child after stop timeout")?
            {
                return Ok(status);
            }
            child
                .kill()
                .await
                .context("Failed to kill daemon child after stop timeout")?;
            child
                .wait()
                .await
                .context("Failed to reap daemon child after forced stop")
        }
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn forward_launchd_signal(child: &Child, signal: libc::c_int) -> Result<()> {
    let pid = child.id().context("daemon child PID unavailable")?;
    // SAFETY: `pid` belongs to the child owned by this runner, and the caller
    // passes only the SIGINT or SIGTERM value received from launchd.
    if unsafe { libc::kill(pid as libc::pid_t, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("Failed to forward launchd stop signal")
    }
}

#[cfg(all(test, not(unix)))]
async fn wait_for_launchd_child(child: &mut Child) -> Result<std::process::ExitStatus> {
    child
        .wait()
        .await
        .context("Failed to wait for daemon child")
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SystemdUserLinger {
    Enabled,
    Disabled { user: String },
    Unknown,
}

/// Supported init systems for service management
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InitSystem {
    /// Auto-detect based on system indicators
    #[default]
    Auto,
    /// systemd (via systemctl --user)
    Systemd,
    /// OpenRC (via rc-service)
    Openrc,
}

impl FromStr for InitSystem {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "systemd" => Ok(Self::Systemd),
            "openrc" => Ok(Self::Openrc),
            other => bail!(
                "Unknown init system: '{}'. Supported: auto, systemd, openrc",
                other
            ),
        }
    }
}

impl InitSystem {
    #[cfg(target_os = "linux")]
    pub fn resolve(self) -> Result<Self> {
        match self {
            Self::Auto => detect_init_system(),
            concrete => Ok(concrete),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn resolve(self) -> Result<Self> {
        match self {
            Self::Auto => Ok(Self::Systemd),
            concrete => Ok(concrete),
        }
    }
}

/// Detect the active init system on Linux
/// Checks for systemd and OpenRC in order, returning the first match.
/// Returns an error if neither is detected.
#[cfg(target_os = "linux")]
fn detect_init_system() -> Result<InitSystem> {
    // Check for systemd first (most common on modern Linux)
    if linux_systemd_runtime_present() {
        return Ok(InitSystem::Systemd);
    }

    // Check for OpenRC: requires /run/openrc AND openrc binary
    if Path::new("/run/openrc").exists() {
        // Check for OpenRC binaries: /sbin/openrc-run or rc-service in PATH
        if Path::new("/sbin/openrc-run").exists() || which::which("rc-service").is_ok() {
            return Ok(InitSystem::Openrc);
        }
    }

    bail!(
        "Could not detect init system. Supported: systemd, OpenRC. \
         Use --service-init to specify manually."
    );
}

pub(crate) fn linux_systemd_runtime_present() -> bool {
    cfg!(target_os = "linux") && Path::new("/run/systemd/system").exists()
}

fn windows_task_name() -> &'static str {
    WINDOWS_TASK_NAME
}

fn linux_service_base(config: &Config) -> String {
    let Some(dir_name) = config
        .config_path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    else {
        return "zeroclaw".to_string();
    };
    let base = dir_name.strip_prefix('.').unwrap_or(dir_name);
    if base == "zeroclaw" {
        return base.to_string();
    }
    if let Some(suffix) = base.strip_prefix("zeroclaw-")
        && !suffix.is_empty()
    {
        return base.to_string();
    }
    "zeroclaw".to_string()
}

fn linux_systemd_unit(config: &Config) -> String {
    format!("{}.service", linux_service_base(config))
}

fn linux_openrc_service(config: &Config) -> String {
    linux_service_base(config)
}

fn ensure_linux_default_install_scope(config: &Config, action: &str) -> Result<()> {
    let service = linux_service_base(config);
    if service == "zeroclaw" {
        return Ok(());
    }

    let config_dir = config
        .config_path
        .parent()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| config.config_path.display().to_string());
    bail!(
        "Linux service {action} only manages the default zeroclaw service. \
         Config directory {config_dir} maps to named service {service}; \
         provide that unit manually, then use service status/start/stop/restart/logs to manage it."
    );
}

fn linux_systemd_action_args(config: &Config, action: &str) -> Vec<String> {
    vec![
        "--user".to_string(),
        action.to_string(),
        linux_systemd_unit(config),
    ]
}

fn linux_openrc_action_args(config: &Config, action: &str) -> Vec<String> {
    vec![linux_openrc_service(config), action.to_string()]
}

fn linux_journalctl_args(config: &Config, lines: usize, follow: bool) -> Vec<String> {
    let mut args = vec![
        "--user".to_string(),
        "-u".to_string(),
        linux_systemd_unit(config),
        "-n".to_string(),
        lines.to_string(),
        "--no-pager".to_string(),
    ];
    if follow {
        args.push("-f".to_string());
    }
    args
}

fn linux_openrc_log_dir(config: &Config) -> PathBuf {
    Path::new("/var/log").join(linux_openrc_service(config))
}

/// Returns whether the ZeroClaw daemon service is currently running.
pub fn is_running(config: &Config) -> bool {
    if cfg!(target_os = "macos") {
        run_capture(Command::new("launchctl").arg("list"))
            .map(|out| out.lines().any(|l| l.contains(SERVICE_LABEL)))
            .unwrap_or(false)
    } else if cfg!(target_os = "linux") {
        is_running_linux(config)
    } else if cfg!(target_os = "windows") {
        run_capture(Command::new("schtasks").args([
            "/Query",
            "/TN",
            WINDOWS_TASK_NAME,
            "/FO",
            "LIST",
        ]))
        .map(|out| out.contains("Running"))
        .unwrap_or(false)
    } else {
        false
    }
}

fn is_running_linux(config: &Config) -> bool {
    // Try systemd first, then OpenRC — mirrors detect_init_system() order
    if run_capture(Command::new("systemctl").args(linux_systemd_action_args(config, "is-active")))
        .map(|out| out.trim() == "active")
        .unwrap_or(false)
    {
        return true;
    }
    run_capture(Command::new("rc-service").args(linux_openrc_action_args(config, "status")))
        .map(|out| out.contains("started"))
        .unwrap_or(false)
}

pub fn install(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        install_macos(config)
    } else if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        install_linux(config, resolved)
    } else if cfg!(target_os = "windows") {
        install_windows(config)
    } else {
        anyhow::bail!("Service management is supported on macOS and Linux only");
    }
}

pub fn start(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        // Ensure the Homebrew var directory exists before launchd tries to use it.
        // The plist may reference this path for WorkingDirectory and log files.
        let exe = std::env::current_exe().ok();
        if let Some(ref exe_path) = exe
            && let Some(var_dir) = homebrew_var_dir_from_exe(exe_path)
        {
            let _ = fs::create_dir_all(&var_dir);
        }
        let plist = macos_service_file()?;
        run_checked(Command::new("launchctl").arg("load").arg("-w").arg(&plist))?;
        run_checked(Command::new("launchctl").arg("start").arg(SERVICE_LABEL))?;
        println!("✅ Service started");
        Ok(())
    } else if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        start_linux(config, resolved)
    } else if cfg!(target_os = "windows") {
        let _ = config;
        run_checked(Command::new("schtasks").args(["/Run", "/TN", windows_task_name()]))?;
        println!("✅ Service started");
        Ok(())
    } else {
        let _ = config;
        anyhow::bail!("Service management is supported on macOS and Linux only")
    }
}

fn start_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run_checked(
                Command::new("systemctl").args(linux_systemd_action_args(config, "start")),
            )?;
            warn_if_systemd_user_linger_disabled();
        }
        InitSystem::Openrc => {
            run_checked(
                Command::new("rc-service").args(linux_openrc_action_args(config, "start")),
            )?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service started");
    Ok(())
}

pub fn stop(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        let plist = macos_service_file()?;
        let _ = run_checked(Command::new("launchctl").arg("stop").arg(SERVICE_LABEL));
        let _ = run_checked(
            Command::new("launchctl")
                .arg("unload")
                .arg("-w")
                .arg(&plist),
        );
        println!("✅ Service stopped");
        Ok(())
    } else if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        stop_linux(config, resolved)
    } else if cfg!(target_os = "windows") {
        let _ = config;
        let task_name = windows_task_name();
        let _ = run_checked(Command::new("schtasks").args(["/End", "/TN", task_name]));
        println!("✅ Service stopped");
        Ok(())
    } else {
        let _ = config;
        anyhow::bail!("Service management is supported on macOS and Linux only")
    }
}

fn stop_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let _ = run_checked(
                Command::new("systemctl").args(linux_systemd_action_args(config, "stop")),
            );
        }
        InitSystem::Openrc => {
            let _ = run_checked(
                Command::new("rc-service").args(linux_openrc_action_args(config, "stop")),
            );
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service stopped");
    Ok(())
}

pub fn restart(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        stop(config, init_system)?;
        start(config, init_system)?;
        println!("✅ Service restarted");
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        return restart_linux(config, resolved);
    }

    if cfg!(target_os = "windows") {
        stop(config, init_system)?;
        start(config, init_system)?;
        println!("✅ Service restarted");
        return Ok(());
    }

    anyhow::bail!("Service management is supported on macOS and Linux only")
}

fn restart_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run_checked(
                Command::new("systemctl").args(linux_systemd_action_args(config, "restart")),
            )?;
        }
        InitSystem::Openrc => {
            run_checked(
                Command::new("rc-service").args(linux_openrc_action_args(config, "restart")),
            )?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service restarted");
    Ok(())
}

pub fn status(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        let out = run_capture(Command::new("launchctl").arg("list"))?;
        let running = out.lines().any(|line| line.contains(SERVICE_LABEL));
        println!(
            "Service: {}",
            if running {
                "✅ running/loaded"
            } else {
                "❌ not loaded"
            }
        );
        println!("Unit: {}", macos_service_file()?.display().to_string());
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        return status_linux(config, resolved);
    }

    if cfg!(target_os = "windows") {
        let _ = config;
        let task_name = windows_task_name();
        let out =
            run_capture(Command::new("schtasks").args(["/Query", "/TN", task_name, "/FO", "LIST"]));
        match out {
            Ok(text) => {
                let running = text.contains("Running");
                println!(
                    "Service: {}",
                    if running {
                        "✅ running"
                    } else {
                        "❌ not running"
                    }
                );
                println!("Task: {}", task_name);
            }
            Err(_) => {
                println!("Service: ❌ not installed");
            }
        }
        return Ok(());
    }

    anyhow::bail!("Service management is supported on macOS and Linux only")
}

fn status_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let out = run_capture(
                Command::new("systemctl").args(linux_systemd_action_args(config, "is-active")),
            )
            .unwrap_or_else(|_| "unknown".into());
            println!("Service state: {}", out.trim());
            println!(
                "Unit: {}",
                linux_systemd_unit_file(config)?.display().to_string()
            );
        }
        InitSystem::Openrc => {
            let out = run_capture(
                Command::new("rc-service").args(linux_openrc_action_args(config, "status")),
            )
            .unwrap_or_else(|_| "unknown".into());
            println!("Service state: {}", out.trim());
            println!("Unit: /etc/init.d/{}", linux_openrc_service(config));
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    Ok(())
}

pub fn logs(config: &Config, init_system: InitSystem, lines: usize, follow: bool) -> Result<()> {
    if cfg!(target_os = "macos") {
        return logs_macos(config, lines, follow);
    }
    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        return logs_linux(config, resolved, lines, follow);
    }
    if cfg!(target_os = "windows") {
        return logs_windows(config, lines, follow);
    }
    anyhow::bail!("Service log viewing is supported on macOS, Linux, and Windows only")
}

fn logs_macos(config: &Config, lines: usize, follow: bool) -> Result<()> {
    // Try the launchd log files first (StandardOutPath / StandardErrorPath from the plist).
    // These are the most reliable source since they capture all daemon output.
    let exe = std::env::current_exe().ok();
    let homebrew_var_dir = exe.as_ref().and_then(|e| homebrew_var_dir_from_exe(e));
    let logs_dir = if let Some(ref var_dir) = homebrew_var_dir {
        var_dir.join("logs")
    } else {
        config
            .config_path
            .parent()
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join("logs")
    };

    let stderr_log = logs_dir.join("daemon.stderr.log");
    let stdout_log = logs_dir.join("daemon.stdout.log");

    // Prefer stderr log (most informative), fall back to stdout
    let log_file = if stderr_log.exists() {
        stderr_log
    } else if stdout_log.exists() {
        stdout_log
    } else {
        bail!(
            "No log files found in {}. Is the service installed?",
            logs_dir.display()
        );
    };

    if follow {
        let status = Command::new("tail")
            .args(["-n", &lines.to_string(), "-f"])
            .arg(&log_file)
            .status()
            .context("Failed to run tail")?;
        if !status.success() {
            bail!("tail exited with non-zero status");
        }
    } else {
        let status = Command::new("tail")
            .args(["-n", &lines.to_string()])
            .arg(&log_file)
            .status()
            .context("Failed to run tail")?;
        if !status.success() {
            bail!("tail exited with non-zero status");
        }
    }
    Ok(())
}

fn logs_linux(config: &Config, init_system: InitSystem, lines: usize, follow: bool) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let args = linux_journalctl_args(config, lines, follow);
            let status = Command::new("journalctl")
                .args(&args)
                .status()
                .context("Failed to run journalctl")?;
            if !status.success() {
                bail!("journalctl exited with non-zero status");
            }
        }
        InitSystem::Openrc => {
            // OpenRC logs go to /var/log/<service>/error.log (as configured in the init script).
            let log_dir = linux_openrc_log_dir(config);
            let log_file = log_dir.join("error.log");
            if !log_file.exists() {
                // Fall back to access log
                let access_log = log_dir.join("access.log");
                if !access_log.exists() {
                    bail!(
                        "No log files found at {}. Is the service installed?",
                        log_dir.display()
                    );
                }
                return tail_file(&access_log, lines, follow);
            }
            tail_file(&log_file, lines, follow)?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    Ok(())
}

fn logs_windows(config: &Config, lines: usize, follow: bool) -> Result<()> {
    let logs_dir = config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("logs");

    let stderr_log = logs_dir.join("daemon.stderr.log");
    let stdout_log = logs_dir.join("daemon.stdout.log");

    let log_file = if stderr_log.exists() {
        stderr_log
    } else if stdout_log.exists() {
        stdout_log
    } else {
        bail!(
            "No log files found in {}. Is the service installed?",
            logs_dir.display()
        );
    };

    if follow {
        // Windows: use PowerShell Get-Content -Wait for tail -f equivalent
        let status = Command::new("powershell")
            .args([
                "-Command",
                &format!(
                    "Get-Content -Path '{}' -Tail {} -Wait",
                    log_file.display().to_string(),
                    lines
                ),
            ])
            .status()
            .context("Failed to run PowerShell Get-Content")?;
        if !status.success() {
            bail!("PowerShell Get-Content exited with non-zero status");
        }
    } else {
        let status = Command::new("powershell")
            .args([
                "-Command",
                &format!(
                    "Get-Content -Path '{}' -Tail {}",
                    log_file.display().to_string(),
                    lines
                ),
            ])
            .status()
            .context("Failed to run PowerShell Get-Content")?;
        if !status.success() {
            bail!("PowerShell Get-Content exited with non-zero status");
        }
    }
    Ok(())
}

/// Tail a log file using the system `tail` command.
fn tail_file(path: &Path, lines: usize, follow: bool) -> Result<()> {
    let mut args = vec!["-n".to_string(), lines.to_string()];
    if follow {
        args.push("-f".to_string());
    }
    let status = Command::new("tail")
        .args(&args)
        .arg(path)
        .status()
        .context("Failed to run tail")?;
    if !status.success() {
        bail!("tail exited with non-zero status");
    }
    Ok(())
}

pub fn uninstall(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        ensure_linux_default_install_scope(config, "uninstall")?;
        stop_linux(config, resolved)?;
        return uninstall_linux(config, resolved);
    }

    stop(config, init_system)?;

    if cfg!(target_os = "macos") {
        let file = macos_service_file()?;
        if file.exists() {
            fs::remove_file(&file)
                .with_context(|| format!("Failed to remove {}", file.display().to_string()))?;
        }
        println!("✅ Service uninstalled ({})", file.display().to_string());
        return Ok(());
    }

    if cfg!(target_os = "windows") {
        let task_name = windows_task_name();
        let _ = run_checked(Command::new("schtasks").args(["/Delete", "/TN", task_name, "/F"]));
        // Remove the wrapper script. It now lives in the config dir root, but
        // older installs left it under logs/ — clean up both so an upgrade
        // doesn't strand the legacy copy.
        let base_dir = config
            .config_path
            .parent()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        for wrapper in [
            base_dir.join("zeroclaw-daemon.cmd"),
            base_dir.join("logs").join("zeroclaw-daemon.cmd"),
        ] {
            if wrapper.exists() {
                fs::remove_file(&wrapper).ok();
            }
        }
        println!("✅ Service uninstalled");
        return Ok(());
    }

    anyhow::bail!("Service management is supported on macOS and Linux only")
}

fn uninstall_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let file = linux_service_file(config)?;
            if file.exists() {
                fs::remove_file(&file)
                    .with_context(|| format!("Failed to remove {}", file.display().to_string()))?;
            }
            let _ = run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]));
            println!("✅ Service uninstalled ({})", file.display().to_string());
        }
        InitSystem::Openrc => {
            let init_script = Path::new("/etc/init.d/zeroclaw");
            if init_script.exists() {
                if let Err(err) =
                    run_checked(Command::new("rc-update").args(["del", "zeroclaw", "default"]))
                {
                    eprintln!(
                        "⚠️  Warning: Could not remove zeroclaw from OpenRC default runlevel: {err}"
                    );
                }
                fs::remove_file(init_script).with_context(|| {
                    format!("Failed to remove {}", init_script.display().to_string())
                })?;
            }
            println!("✅ Service uninstalled (/etc/init.d/zeroclaw)");
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    Ok(())
}

pub fn homebrew_var_dir_from_exe(exe: &Path) -> Option<PathBuf> {
    let resolved = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    let exe = resolved.as_path();

    if let Some(cellar) = exe
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "Cellar"))
    {
        return cellar
            .parent()
            .map(|prefix| prefix.join("var").join("zeroclaw"));
    }

    let prefix = exe.parent()?.parent()?;
    prefix
        .join("Cellar")
        .is_dir()
        .then(|| prefix.join("var").join("zeroclaw"))
}

#[cfg(test)]
mod homebrew_tests {
    use super::*;

    #[test]
    fn homebrew_var_dir_from_exe_detects_cellar_path() {
        let exe = PathBuf::from("/opt/homebrew/Cellar/zeroclaw/1.2.3/bin/zeroclaw");
        let var_dir = homebrew_var_dir_from_exe(&exe);
        assert_eq!(var_dir, Some(PathBuf::from("/opt/homebrew/var/zeroclaw")));
    }

    #[test]
    fn homebrew_var_dir_from_exe_detects_intel_cellar_path() {
        let exe = PathBuf::from("/usr/local/Cellar/zeroclaw/1.0.0/bin/zeroclaw");
        let var_dir = homebrew_var_dir_from_exe(&exe);
        assert_eq!(var_dir, Some(PathBuf::from("/usr/local/var/zeroclaw")));
    }

    #[test]
    fn homebrew_var_dir_from_exe_ignores_non_homebrew_path() {
        let exe = PathBuf::from("/home/user/.cargo/bin/zeroclaw");
        let var_dir = homebrew_var_dir_from_exe(&exe);
        assert_eq!(var_dir, None);
    }

    #[cfg(unix)]
    #[test]
    fn homebrew_var_dir_from_exe_detects_opt_symlink_layout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prefix = temp.path().join("homebrew");
        let cellar_bin = prefix.join("Cellar/zeroclaw/1.2.3/bin");
        std::fs::create_dir_all(&cellar_bin).expect("create Cellar binary dir");
        let cellar_exe = cellar_bin.join("zeroclaw");
        std::fs::write(&cellar_exe, "").expect("create fake executable");

        let opt_parent = prefix.join("opt");
        std::fs::create_dir_all(&opt_parent).expect("create opt dir");
        std::os::unix::fs::symlink(
            prefix.join("Cellar/zeroclaw/1.2.3"),
            opt_parent.join("zeroclaw"),
        )
        .expect("create opt symlink");

        let expected_prefix = prefix
            .canonicalize()
            .expect("canonicalize fake Homebrew prefix");
        let var_dir = homebrew_var_dir_from_exe(&prefix.join("opt/zeroclaw/bin/zeroclaw"));
        assert_eq!(var_dir, Some(expected_prefix.join("var/zeroclaw")));
    }
}

fn install_macos(config: &Config) -> Result<()> {
    let file = macos_service_file()?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }

    let exe = std::env::current_exe().context("Failed to resolve current executable")?;

    // When installed via Homebrew, use the Homebrew var directory for runtime
    // data so that `brew services start zeroclaw` works out of the box.
    let homebrew_var_dir = homebrew_var_dir_from_exe(&exe);
    if let Some(ref var_dir) = homebrew_var_dir {
        fs::create_dir_all(var_dir).with_context(|| {
            format!(
                "Failed to create Homebrew var directory: {}",
                var_dir.display()
            )
        })?;
    }

    let config_dir = homebrew_var_dir.as_deref().map_or_else(
        || {
            config
                .config_path
                .parent()
                .map_or_else(|| PathBuf::from("."), PathBuf::from)
        },
        Path::to_path_buf,
    );
    let logs_dir = config_dir.join("logs");
    fs::create_dir_all(&logs_dir)?;

    let plist = render_macos_launch_agent_plist(&exe, &config_dir, homebrew_var_dir.as_deref());

    fs::write(&file, plist)?;
    println!("✅ Installed launchd service: {}", file.display());
    if let Some(ref var_dir) = homebrew_var_dir {
        println!("   Homebrew var: {}", var_dir.display());
    }
    println!("   Start with: zeroclaw service start");
    Ok(())
}

/// Renders the macOS LaunchAgent plist; path arguments are XML-escaped before interpolation,
/// and the caller is responsible for writing the returned XML to the plist path.
fn render_macos_launch_agent_plist(
    exe: &Path,
    config_dir: &Path,
    homebrew_var_dir: Option<&Path>,
) -> String {
    let working_dir_section = if let Some(var_dir) = homebrew_var_dir {
        format!(
            r#"  <key>WorkingDirectory</key>
  <string>{working_dir}</string>
"#,
            working_dir = xml_escape(&var_dir.display().to_string()),
        )
    } else {
        String::new()
    };

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--config-dir</string>
    <string>{config_dir}</string>
    <string>service</string>
    <string>run-launchd-daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
{working_dir_section}
</dict>
</plist>
"#,
        label = SERVICE_LABEL,
        exe = xml_escape(&exe.display().to_string()),
        config_dir = xml_escape(&config_dir.display().to_string()),
        working_dir_section = working_dir_section,
    )
}

fn install_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    ensure_linux_default_install_scope(config, "install")?;

    match init_system {
        InitSystem::Systemd => install_linux_systemd(config),
        InitSystem::Openrc => install_linux_openrc(config),
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
}

fn install_linux_systemd(config: &Config) -> Result<()> {
    let file = linux_service_file(config)?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }

    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    let unit = format!(
        "[Unit]\n\
         Description=ZeroClaw daemon\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} daemon\n\
         Restart=always\n\
         RestartSec=3\n\
         # Ensure HOME is set so headless browsers can create profile/cache dirs.\n\
         Environment=HOME=%h\n\
         # Allow inheriting DISPLAY and XDG_RUNTIME_DIR from the user session\n\
         # so graphical/headless browsers can function correctly.\n\
         PassEnvironment=DISPLAY XDG_RUNTIME_DIR\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display()
    );

    fs::write(&file, unit)?;
    let _ = run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]));
    let _ = run_checked(Command::new("systemctl").args(["--user", "enable", "zeroclaw.service"]));
    println!(
        "✅ Installed systemd user service: {}",
        file.display().to_string()
    );
    println!("   Start with: zeroclaw service start");
    warn_if_systemd_user_linger_disabled();
    Ok(())
}

/// Check if the current process is running as root (Unix only)
#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: `getuid()` is a simple system call that returns the real user ID of the calling
    // process. It is always safe to call as it takes no arguments and returns a scalar value.
    // This is a well-established pattern in Rust for getting the current user ID.
    unsafe { libc::getuid() == 0 }
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

/// Check if the zeroclaw user exists and has expected properties.
/// Returns Ok if user doesn't exist (OpenRC will handle creation or fail gracefully).
/// Returns error if user exists but has unexpected properties.
fn check_zeroclaw_user() -> Result<()> {
    let output = Command::new("getent").args(["passwd", "zeroclaw"]).output();
    let is_alpine = Path::new("/etc/alpine-release").exists();

    let (del_cmd, add_cmd) = if is_alpine {
        (
            "deluser zeroclaw && delgroup zeroclaw",
            "addgroup -S zeroclaw && adduser -S -s /sbin/nologin -H -D -G zeroclaw zeroclaw",
        )
    } else {
        ("userdel zeroclaw", "useradd -r -s /sbin/nologin zeroclaw")
    };

    match output {
        Ok(output) if output.status.success() => {
            let passwd_entry = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = passwd_entry.split(':').collect();
            if parts.len() >= 7 {
                let uid = parts[2];
                let gid = parts[3];
                let home = parts[5];
                let shell = parts[6];

                if uid.parse::<u32>().unwrap_or(999) >= 1000 {
                    bail!(
                        "User 'zeroclaw' exists but has unexpected UID {} (expected system UID < 1000).\n\
                         Recreate with: sudo {} && sudo {}",
                        uid,
                        del_cmd,
                        add_cmd
                    );
                }

                if !shell.contains("nologin") && !shell.contains("false") {
                    bail!(
                        "User 'zeroclaw' exists but has unexpected shell '{}'.\n\
                         Expected nologin/false for security. Fix with: sudo {} && sudo {}",
                        shell,
                        del_cmd,
                        add_cmd
                    );
                }

                if home != "/var/lib/zeroclaw" && home != "/nonexistent" {
                    eprintln!(
                        "⚠️  Warning: zeroclaw user has home directory '{}' (expected /var/lib/zeroclaw or /nonexistent)",
                        home
                    );
                }

                let _ = gid;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn ensure_zeroclaw_user() -> Result<()> {
    let output = Command::new("getent").args(["passwd", "zeroclaw"]).output();
    if let Ok(output) = output
        && output.status.success()
    {
        return check_zeroclaw_user();
    }

    let is_alpine = Path::new("/etc/alpine-release").exists();

    if is_alpine {
        let group_output = Command::new("getent").args(["group", "zeroclaw"]).output();
        let group_exists = group_output.map(|o| o.status.success()).unwrap_or(false);

        if !group_exists {
            let output = Command::new("addgroup")
                .args(["-S", "zeroclaw"])
                .output()
                .context("Failed to create zeroclaw group")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("Failed to create zeroclaw group: {}", stderr.trim());
            }
            println!("✅ Created system group: zeroclaw");
        }

        let output = Command::new("adduser")
            .args([
                "-S",
                "-s",
                "/sbin/nologin",
                "-H",
                "-D",
                "-G",
                "zeroclaw",
                "zeroclaw",
            ])
            .output()
            .context("Failed to create zeroclaw user")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to create zeroclaw user: {}", stderr.trim());
        }
    } else {
        let output = Command::new("useradd")
            .args(["-r", "-s", "/sbin/nologin", "zeroclaw"])
            .output()
            .context("Failed to create zeroclaw user")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to create zeroclaw user: {}", stderr.trim());
        }
    }

    println!("✅ Created system user: zeroclaw");
    Ok(())
}

/// Change ownership of a path to zeroclaw:zeroclaw
#[cfg(unix)]
fn chown_to_zeroclaw(path: &Path) -> Result<()> {
    let output = Command::new("chown")
        .args(["zeroclaw:zeroclaw", &path.to_string_lossy()])
        .output()
        .context("Failed to run chown")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to change ownership of {} to zeroclaw:zeroclaw: {}",
            path.display().to_string(),
            stderr.trim(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_to_zeroclaw(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn chown_recursive_to_zeroclaw(path: &Path) -> Result<()> {
    let output = Command::new("chown")
        .args(["-R", "zeroclaw:zeroclaw", &path.to_string_lossy()])
        .output()
        .context("Failed to run recursive chown")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to recursively change ownership of {} to zeroclaw:zeroclaw: {}",
            path.display().to_string(),
            stderr.trim(),
        );
    }

    Ok(())
}

#[cfg(not(unix))]
fn chown_recursive_to_zeroclaw(_path: &Path) -> Result<()> {
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).with_context(|| {
        format!(
            "Failed to create directory {}",
            target.display().to_string()
        )
    })?;

    for entry in fs::read_dir(source)
        .with_context(|| format!("Failed to read directory {}", source.display().to_string()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to inspect {}", source_path.display().to_string()))?;

        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if file_type.is_file() {
            if target_path.exists() {
                continue;
            }
            fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "Failed to copy file {} -> {}",
                    source_path.display().to_string(),
                    target_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn resolve_invoking_user_config_dir() -> Option<PathBuf> {
    let sudo_user = std::env::var("SUDO_USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "root");

    if let Some(user) = sudo_user
        && let Ok(output) = Command::new("getent").args(["passwd", &user]).output()
        && output.status.success()
    {
        let entry = String::from_utf8_lossy(&output.stdout);
        let fields: Vec<&str> = entry.trim().split(':').collect();
        if fields.len() >= 6 {
            return Some(PathBuf::from(fields[5]).join(".zeroclaw"));
        }
    }

    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join(".zeroclaw"))
}

fn migrate_openrc_runtime_state_if_needed(config_dir: &Path) -> Result<()> {
    let target_config = config_dir.join("config.toml");
    if target_config.exists() {
        println!(
            "✅ Reusing existing OpenRC config at {}",
            target_config.display()
        );
        return Ok(());
    }

    let Some(source_dir) = resolve_invoking_user_config_dir() else {
        return Ok(());
    };

    let source_config = source_dir.join("config.toml");
    if !source_config.exists() {
        return Ok(());
    }

    copy_dir_recursive(&source_dir, config_dir)?;
    println!(
        "✅ Migrated runtime state from {} to {}",
        source_dir.display().to_string(),
        config_dir.display()
    );
    Ok(())
}

fn shell_single_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}

fn openrc_log_writer_command(exe_path: &Path, stream: &str) -> String {
    format!(
        "{} service run-openrc-log-writer {stream}",
        exe_path.to_string_lossy()
    )
}

fn openrc_executable_path_is_safe(exe_path: &Path) -> bool {
    let raw = exe_path.to_string_lossy();
    !raw.is_empty()
        && raw.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'.' | b'-' | b'+')
        })
}

#[cfg(unix)]
fn build_openrc_writability_probe_command(path: &Path, has_runuser: bool) -> (String, Vec<String>) {
    let probe = format!("test -w {}", shell_single_quote(&path.to_string_lossy()));
    if has_runuser {
        (
            "runuser".to_string(),
            vec![
                "-u".to_string(),
                "zeroclaw".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                probe,
            ],
        )
    } else {
        (
            "su".to_string(),
            vec![
                "-s".to_string(),
                "/bin/sh".to_string(),
                "-c".to_string(),
                probe,
                "zeroclaw".to_string(),
            ],
        )
    }
}

#[cfg(unix)]
fn ensure_openrc_runtime_path_writable(path: &Path) -> Result<()> {
    let has_runuser = which::which("runuser").is_ok();
    let (program, args) = build_openrc_writability_probe_command(path, has_runuser);
    let output = Command::new(&program)
        .args(args.iter().map(String::as_str))
        .output()
        .with_context(|| {
            format!(
                "Failed to verify OpenRC runtime write access for {}",
                path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let details = if stderr.trim().is_empty() {
            "write-access probe failed"
        } else {
            stderr.trim()
        };
        bail!(
            "OpenRC runtime user 'zeroclaw' cannot write {} ({details}). \
             Re-run `sudo zeroclaw service install` and ensure ownership is zeroclaw:zeroclaw.",
            path.display().to_string(),
        );
    }

    Ok(())
}

#[cfg(unix)]
fn ensure_openrc_runtime_dirs_writable(
    config_dir: &Path,
    workspace_dir: &Path,
    log_dir: &Path,
) -> Result<()> {
    for path in [config_dir, workspace_dir, log_dir] {
        ensure_openrc_runtime_path_writable(path)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_openrc_runtime_dirs_writable(
    _config_dir: &Path,
    _workspace_dir: &Path,
    _log_dir: &Path,
) -> Result<()> {
    Ok(())
}

/// Warn if the binary path is in a user home directory
fn warn_if_binary_in_home(exe_path: &Path) {
    let path_str = exe_path.to_string_lossy();
    if path_str.contains("/home/") || path_str.contains(".cargo/bin") {
        eprintln!(
            "⚠️  Warning: Binary path '{}' appears to be in a user home directory.\n\
             For system-wide OpenRC service, consider installing to /usr/local/bin:\n\
             sudo cp '{}' /usr/local/bin/zeroclaw",
            exe_path.display().to_string(),
            exe_path.display()
        );
    }
}

/// Generate OpenRC init script content (pure function for testability)
fn generate_openrc_script(exe_path: &Path, config_dir: &Path) -> String {
    let output_logger = openrc_log_writer_command(exe_path, "stdout");
    let error_logger = openrc_log_writer_command(exe_path, "stderr");
    format!(
        r#"#!/sbin/openrc-run

name="zeroclaw"
description="ZeroClaw daemon"

command="{exe}"
command_args="--config-dir {config_dir} daemon"
command_background="yes"
command_user="zeroclaw:zeroclaw"
pidfile="/run/${{RC_SVCNAME}}.pid"
umask 027
output_logger="{output_logger}"
error_logger="{error_logger}"

# Provide HOME so headless browsers can create profile/cache directories.
# Without this, Chromium/Firefox fail with sandbox or profile errors.
export HOME="/var/lib/zeroclaw"

depend() {{
    need net
    after firewall
}}

start_pre() {{
    checkpath --directory --owner zeroclaw:zeroclaw --mode 0750 /var/lib/zeroclaw
}}
"#,
        exe = exe_path.display().to_string(),
        config_dir = config_dir.display().to_string(),
        output_logger = output_logger,
        error_logger = error_logger,
    )
}

fn resolve_openrc_executable() -> Result<PathBuf> {
    let preferred = Path::new("/usr/local/bin/zeroclaw");
    if preferred.exists() {
        return Ok(preferred.to_path_buf());
    }

    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    Ok(exe)
}

fn install_linux_openrc(config: &Config) -> Result<()> {
    if !is_root() {
        bail!(
            "OpenRC service installation requires root privileges.\n\
             Please run with sudo: sudo zeroclaw service install"
        );
    }

    let exe = resolve_openrc_executable()?;
    if !openrc_executable_path_is_safe(&exe) {
        bail!(
            "OpenRC service executable path contains unsupported shell characters: {}. Install ZeroClaw at /usr/local/bin/zeroclaw and retry",
            exe.display()
        );
    }
    ensure_zeroclaw_user()?;
    warn_if_binary_in_home(&exe);

    let config_dir = Path::new("/etc/zeroclaw");
    let workspace_dir = config_dir.join("workspace");
    let log_dir = Path::new("/var/log/zeroclaw");

    if !config_dir.exists() {
        fs::create_dir_all(config_dir)
            .with_context(|| format!("Failed to create {}", config_dir.display().to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(config_dir, fs::Permissions::from_mode(0o755)).with_context(
                || {
                    format!(
                        "Failed to set permissions on {}",
                        config_dir.display().to_string()
                    )
                },
            )?;
        }
        println!("✅ Created directory: {}", config_dir.display().to_string());
    }

    migrate_openrc_runtime_state_if_needed(config_dir)?;

    if !workspace_dir.exists() {
        fs::create_dir_all(&workspace_dir)
            .with_context(|| format!("Failed to create {}", workspace_dir.display().to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&workspace_dir, fs::Permissions::from_mode(0o750)).with_context(
                || {
                    format!(
                        "Failed to set permissions on {}",
                        workspace_dir.display().to_string()
                    )
                },
            )?;
        }
        chown_to_zeroclaw(&workspace_dir)?;
        println!(
            "✅ Created directory: {} (owned by zeroclaw:zeroclaw)",
            workspace_dir.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&workspace_dir, fs::Permissions::from_mode(0o750)).with_context(
            || {
                format!(
                    "Failed to set permissions on {}",
                    workspace_dir.display().to_string()
                )
            },
        )?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(config_dir, fs::Permissions::from_mode(0o755)).with_context(|| {
            format!(
                "Failed to set permissions on {}",
                config_dir.display().to_string()
            )
        })?;
        let config_path = config_dir.join("config.toml");
        if config_path.exists() {
            fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).with_context(
                || {
                    format!(
                        "Failed to set permissions on {}",
                        config_path.display().to_string()
                    )
                },
            )?;
        }
        let secret_key_path = config_dir.join(".secret_key");
        if secret_key_path.exists() {
            fs::set_permissions(&secret_key_path, fs::Permissions::from_mode(0o600)).with_context(
                || {
                    format!(
                        "Failed to set permissions on {}",
                        secret_key_path.display().to_string()
                    )
                },
            )?;
        }
    }

    chown_recursive_to_zeroclaw(config_dir)?;

    let created_log_dir = !log_dir.exists();
    if created_log_dir {
        fs::create_dir_all(log_dir)
            .with_context(|| format!("Failed to create {}", log_dir.display().to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(log_dir, fs::Permissions::from_mode(0o750)).with_context(|| {
                format!(
                    "Failed to set permissions on {}",
                    log_dir.display().to_string()
                )
            })?;
        }
    }

    chown_to_zeroclaw(log_dir)?;

    ensure_openrc_runtime_dirs_writable(config_dir, &workspace_dir, log_dir)?;

    if created_log_dir {
        println!(
            "✅ Created directory: {} (owned by zeroclaw:zeroclaw)",
            log_dir.display()
        );
    }

    let init_script = generate_openrc_script(&exe, config_dir);
    let init_path = Path::new("/etc/init.d/zeroclaw");
    fs::write(init_path, init_script)
        .with_context(|| format!("Failed to write {}", init_path.display().to_string()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(init_path, fs::Permissions::from_mode(0o755)).with_context(|| {
            format!(
                "Failed to set permissions on {}",
                init_path.display().to_string()
            )
        })?;
    }

    run_checked(Command::new("rc-update").args(["add", "zeroclaw", "default"]))?;
    println!("✅ Installed OpenRC service: /etc/init.d/zeroclaw");
    println!("   Config path: /etc/zeroclaw/config.toml");
    println!("   Start with: sudo zeroclaw service start");
    let _ = config;
    Ok(())
}

fn install_windows(config: &Config) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    let base_dir = config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let logs_dir = base_dir.join("logs");
    fs::create_dir_all(&logs_dir)?;

    // The launch wrapper is an install artifact, not log output — keep it in
    // the config dir root so the logs dir holds only `.log` files. (Previously
    // it landed in logs/, where a `.cmd` next to the daemon's log files reads
    // as misplaced.)
    let wrapper = base_dir.join("zeroclaw-daemon.cmd");
    let stdout_log = logs_dir.join("daemon.stdout.log");
    let stderr_log = logs_dir.join("daemon.stderr.log");

    let wrapper_content = format!(
        "@echo off\r\n\"{}\" daemon >>\"{}\" 2>>\"{}\"",
        exe.display().to_string(),
        stdout_log.display().to_string(),
        stderr_log.display()
    );
    fs::write(&wrapper, &wrapper_content)?;

    let task_name = windows_task_name();

    // Remove any existing task first (ignore errors if it doesn't exist)
    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", task_name, "/F"])
        .output();

    run_checked(Command::new("schtasks").args([
        "/Create",
        "/TN",
        task_name,
        "/SC",
        "ONLOGON",
        "/TR",
        &format!("\"{}\"", wrapper.display().to_string()),
        "/RL",
        "LIMITED",
        "/F",
    ]))?;

    println!("✅ Installed Windows scheduled task: {}", task_name);
    println!("   Wrapper: {}", wrapper.display().to_string());
    println!("   Logs: {}", logs_dir.display().to_string());
    println!("   Start with: zeroclaw service start");
    Ok(())
}

fn macos_service_file() -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist")))
}

fn linux_service_file(config: &Config) -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    // `service install` remains default-instance only; named instances can be
    // managed when operators provide matching units themselves.
    let _ = config;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join("zeroclaw.service"))
}

fn linux_systemd_unit_file(config: &Config) -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(linux_systemd_unit(config)))
}

fn run_checked(command: &mut Command) -> Result<()> {
    let output = command.output().context("Failed to spawn command")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Command failed: {}", stderr.trim());
    }
    Ok(())
}

pub fn run_capture(command: &mut Command) -> Result<String> {
    let output = command.output().context("Failed to spawn command")?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).to_string();
    }
    Ok(text)
}

pub fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(unix)]
fn current_loginctl_user_target() -> Option<String> {
    // SAFETY: getuid() has no preconditions and returns the real UID of the
    // process. loginctl accepts the numeric UID, which avoids trusting $USER.
    Some(unsafe { libc::getuid() }.to_string())
}

#[cfg(not(unix))]
fn current_loginctl_user_target() -> Option<String> {
    None
}

fn parse_loginctl_linger_property(output: &str) -> Option<bool> {
    output.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("Linger") {
            return None;
        }
        let value = value.trim();
        if value.eq_ignore_ascii_case("yes") {
            Some(true)
        } else if value.eq_ignore_ascii_case("no") {
            Some(false)
        } else {
            None
        }
    })
}

pub(crate) fn systemd_user_linger_status() -> SystemdUserLinger {
    let Some(user) = current_loginctl_user_target() else {
        return SystemdUserLinger::Unknown;
    };

    let output = Command::new("loginctl")
        .args(["show-user", user.as_str(), "--property=Linger"])
        .output();

    match output {
        Ok(output) => systemd_user_linger_status_from_output(
            user,
            output.status.success(),
            &String::from_utf8_lossy(&output.stdout),
        ),
        Err(_) => SystemdUserLinger::Unknown,
    }
}

fn systemd_user_linger_status_from_output(
    user: String,
    success: bool,
    stdout: &str,
) -> SystemdUserLinger {
    if !success {
        return SystemdUserLinger::Unknown;
    }

    match parse_loginctl_linger_property(stdout) {
        Some(true) => SystemdUserLinger::Enabled,
        Some(false) => SystemdUserLinger::Disabled { user },
        None => SystemdUserLinger::Unknown,
    }
}

fn systemd_linger_hint(user: &str) -> String {
    crate::i18n::get_required_cli_string_with_args(
        "cli-service-systemd-linger-disabled-warning",
        &[("user", user)],
    )
}

fn warn_if_systemd_user_linger_disabled() {
    if let SystemdUserLinger::Disabled { user } = systemd_user_linger_status() {
        eprintln!("⚠️  {}", systemd_linger_hint(&user));
    }
}

// Plain `#[cfg(test)]` is intentional: these pure renderer tests have no
// integration dependencies and should run in every zeroclaw-runtime test build.
#[cfg(test)]
mod macos_plist_tests {
    use super::*;

    #[test]
    fn macos_plist_renderer_uses_plain_xml_quotes() {
        let plist = render_macos_launch_agent_plist(
            Path::new("/opt/homebrew/bin/zeroclaw"),
            Path::new("/opt/homebrew/var/zeroclaw"),
            Some(Path::new("/opt/homebrew/var/zeroclaw")),
        );

        assert!(!plist.contains(r#"\""#));
        assert!(plist.starts_with(r#"<?xml version="1.0" encoding="UTF-8"?>"#));
        assert!(plist.contains(
            r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#
        ));
        assert!(plist.contains(r#"<plist version="1.0">"#));
        assert!(plist.contains("<key>WorkingDirectory</key>"));
    }

    #[test]
    fn macos_plist_renderer_escapes_paths_and_omits_homebrew_section_when_absent() {
        let plist = render_macos_launch_agent_plist(
            Path::new("/tmp/Zero<&>\"'Claw/bin/zeroclaw"),
            Path::new("/tmp/Zero<&>\"'Claw"),
            None,
        );

        assert!(plist.contains("/tmp/Zero&lt;&amp;&gt;&quot;&apos;Claw/bin/zeroclaw"));
        assert!(plist.contains("/tmp/Zero&lt;&amp;&gt;&quot;&apos;Claw"));
        assert!(!plist.contains("<key>EnvironmentVariables</key>"));
        assert!(!plist.contains("<key>WorkingDirectory</key>"));
    }

    #[test]
    fn macos_plist_routes_only_launchd_through_bounded_capture() {
        let plist = render_macos_launch_agent_plist(
            Path::new("/usr/local/bin/zeroclaw"),
            Path::new("/Users/test/.zeroclaw"),
            None,
        );

        assert!(plist.contains("<string>--config-dir</string>"));
        assert!(plist.contains("<string>/Users/test/.zeroclaw</string>"));
        assert!(plist.contains("<string>service</string>"));
        assert!(plist.contains("<string>run-launchd-daemon</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(!plist.contains("<key>StandardOutPath</key>"));
        assert!(!plist.contains("<key>StandardErrorPath</key>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_plist_renderer_emits_plutil_parseable_xml() {
        let plist = render_macos_launch_agent_plist(
            Path::new("/tmp/Zero<&>\"'Claw/bin/zeroclaw"),
            Path::new("/tmp/Zero<&>\"'Claw/var/zeroclaw"),
            Some(Path::new("/tmp/Zero<&>\"'Claw/var/zeroclaw")),
        );

        let file = std::env::temp_dir().join(format!(
            "zeroclaw-launch-agent-plist-{}.plist",
            std::process::id()
        ));
        fs::write(&file, plist).expect("write plist fixture");

        let output = Command::new("plutil")
            .arg("-lint")
            .arg(&file)
            .output()
            .expect("run plutil");
        let _ = fs::remove_file(&file);

        assert!(
            output.status.success(),
            "plutil failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(test)]
mod bounded_service_log_tests {
    use super::*;

    #[test]
    fn opening_oversized_log_keeps_newest_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("daemon.stdout.log");
        let mut original = vec![b'a'; SERVICE_LOG_MAX_BYTES as usize + 17];
        original[17..].fill(b'b');
        fs::write(&path, original).expect("write oversized log");

        drop(BoundedServiceLog::open(&path).expect("open bounded log"));

        let bytes = fs::read(&path).expect("read compacted log");
        assert_eq!(bytes.len(), SERVICE_LOG_MAX_BYTES as usize);
        assert!(bytes.iter().all(|byte| *byte == b'b'));
    }

    #[test]
    fn crossing_limit_compacts_before_appending_newest_chunk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("daemon.stderr.log");
        let mut log = BoundedServiceLog::open(&path).expect("open bounded log");
        log.write_chunk(&vec![b'a'; 5 * 1024 * 1024])
            .expect("write initial chunk");
        log.write_chunk(&vec![b'b'; 4 * 1024 * 1024])
            .expect("write overflowing chunk");
        drop(log);

        let bytes = fs::read(&path).expect("read bounded log");
        assert_eq!(bytes.len(), SERVICE_LOG_MAX_BYTES as usize);
        assert!(bytes[..4 * 1024 * 1024].iter().all(|byte| *byte == b'a'));
        assert!(bytes[4 * 1024 * 1024..].iter().all(|byte| *byte == b'b'));
    }

    #[test]
    fn oversized_chunk_keeps_only_its_tail() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("daemon.stdout.log");
        let mut log = BoundedServiceLog::open(&path).expect("open bounded log");
        let mut chunk = vec![b'a'; SERVICE_LOG_MAX_BYTES as usize + 23];
        chunk[23..].fill(b'z');
        log.write_chunk(&chunk).expect("write oversized chunk");
        drop(log);

        let bytes = fs::read(&path).expect("read bounded log");
        assert_eq!(bytes.len(), SERVICE_LOG_MAX_BYTES as usize);
        assert!(bytes.iter().all(|byte| *byte == b'z'));
    }

    #[test]
    fn openrc_stream_worker_writes_to_the_bounded_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("access.log");

        drain_bounded_service_log(std::io::Cursor::new(b"openrc-output"), &path)
            .expect("drain logger input");

        assert_eq!(fs::read(path).expect("read log"), b"openrc-output");
    }

    #[test]
    fn openrc_streams_map_to_the_established_log_paths() {
        assert_eq!(openrc_log_path(false), Path::new(OPENRC_STDOUT_LOG));
        assert_eq!(openrc_log_path(true), Path::new(OPENRC_STDERR_LOG));
        assert_eq!(
            openrc_log_path(false),
            Path::new("/var/log/zeroclaw/access.log")
        );
        assert_eq!(
            openrc_log_path(true),
            Path::new("/var/log/zeroclaw/error.log")
        );
    }

    #[test]
    fn openrc_stream_worker_drains_input_when_destination_cannot_open() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = vec![b'x'; 64 * 1024];
        let mut input = std::io::Cursor::new(bytes.clone());

        drain_bounded_service_log(&mut input, dir.path())
            .expect_err("directory cannot be opened as a service log");

        assert_eq!(input.position(), bytes.len() as u64);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn openrc_stream_worker_drains_input_after_destination_write_fails() {
        let bytes = vec![b'x'; 64 * 1024];
        let mut input = std::io::Cursor::new(bytes.clone());

        drain_bounded_service_log(&mut input, Path::new("/dev/full"))
            .expect_err("writes to /dev/full must fail");

        assert_eq!(input.position(), bytes.len() as u64);
    }

    #[test]
    fn pending_output_evicts_oldest_chunks() {
        let inner = Arc::new(LaunchdLogSinkInner {
            pending: Mutex::new(PendingLaunchdLog {
                chunks: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            ready: Condvar::new(),
        });
        let sink = LaunchdLogSink(Arc::clone(&inner));
        sink.push(vec![b'a'; 700 * 1024]);
        sink.push(vec![b'b'; 700 * 1024]);

        let pending = inner.pending.lock().expect("pending queue");
        assert!(pending.bytes <= LAUNCHD_LOG_PENDING_BYTES);
        assert_eq!(pending.chunks.len(), 1);
        assert!(pending.chunks[0].iter().all(|byte| *byte == b'b'));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runner_captures_both_child_streams() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = launchd_capture_paths(dir.path());
        run_with_launchd_capture(paths, || {
            let mut command = TokioCommand::new("/bin/sh");
            command.args(["-c", "printf stdout-value; printf stderr-value >&2"]);
            Ok(command)
        })
        .await
        .expect("capture child output");

        assert_eq!(
            fs::read(dir.path().join("logs/daemon.stdout.log")).expect("read stdout"),
            b"stdout-value"
        );
        assert_eq!(
            fs::read(dir.path().join("logs/daemon.stderr.log")).expect("read stderr"),
            b"stderr-value"
        );
    }

    #[tokio::test]
    async fn capture_opens_before_command_resolution() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = launchd_capture_paths(dir.path());
        let error = run_with_launchd_capture(paths, || bail!("fixture resolution failed"))
            .await
            .expect_err("resolution should fail");

        assert!(error.to_string().contains("fixture resolution failed"));
        let stderr = fs::read_to_string(dir.path().join("logs/daemon.stderr.log"))
            .expect("read bootstrap diagnostics");
        assert!(stderr.contains("launchd capture failed: fixture resolution failed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn nonzero_child_exit_is_reported_in_bounded_stderr() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = launchd_capture_paths(dir.path());
        let error = run_with_launchd_capture(paths, || {
            let mut command = TokioCommand::new("/bin/sh");
            command.args(["-c", "exit 7"]);
            Ok(command)
        })
        .await
        .expect_err("child should fail");

        assert!(error.to_string().contains("status"));
        let stderr = fs::read_to_string(dir.path().join("logs/daemon.stderr.log"))
            .expect("read failure diagnostics");
        assert!(stderr.contains("daemon child exited with status"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_signal_reaps_owned_child_and_preserves_status() {
        let mut child = TokioCommand::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn child");
        let status = tokio::time::timeout(
            Duration::from_secs(2),
            stop_launchd_child(&mut child, libc::SIGTERM),
        )
        .await
        .expect("child stop timeout")
        .expect("stop child");
        assert!(!status.success());
        assert!(child.try_wait().expect("inspect child").is_some());
    }
}

#[cfg(test)]
mod linux_service_tests {
    use super::*;

    fn config_at(path: &str) -> Config {
        Config {
            config_path: PathBuf::from(path),
            ..Config::default()
        }
    }

    #[test]
    fn linux_service_base_derives_named_instance_from_config_dir() {
        assert_eq!(
            linux_service_base(&config_at("/home/user/.zeroclaw-p100-104/config.toml")),
            "zeroclaw-p100-104"
        );
        assert_eq!(
            linux_service_base(&config_at("/home/user/zeroclaw-prod/config.toml")),
            "zeroclaw-prod"
        );
    }

    #[test]
    fn linux_service_base_falls_back_for_default_and_unrelated_dirs() {
        assert_eq!(
            linux_service_base(&config_at("/home/user/.zeroclaw/config.toml")),
            "zeroclaw"
        );
        assert_eq!(
            linux_service_base(&config_at("/tmp/scratch/config.toml")),
            "zeroclaw"
        );
        assert_eq!(
            linux_service_base(&config_at("/home/user/.zeroclaw-/config.toml")),
            "zeroclaw"
        );
        assert_eq!(linux_service_base(&config_at("config.toml")), "zeroclaw");
    }

    #[test]
    fn linux_service_control_args_use_named_instance() {
        let config = config_at("/home/user/.zeroclaw-p100-104/config.toml");

        assert_eq!(
            linux_systemd_action_args(&config, "start"),
            ["--user", "start", "zeroclaw-p100-104.service"]
        );
        assert_eq!(
            linux_openrc_action_args(&config, "status"),
            ["zeroclaw-p100-104", "status"]
        );
    }

    #[test]
    fn linux_openrc_log_dir_uses_named_instance() {
        assert_eq!(
            linux_openrc_log_dir(&config_at("/home/user/.zeroclaw/config.toml")),
            PathBuf::from("/var/log/zeroclaw")
        );
        assert_eq!(
            linux_openrc_log_dir(&config_at("/home/user/.zeroclaw-p100-104/config.toml")),
            PathBuf::from("/var/log/zeroclaw-p100-104")
        );
    }

    #[test]
    fn linux_install_scope_rejects_named_instances() {
        assert!(
            ensure_linux_default_install_scope(
                &config_at("/home/user/.zeroclaw/config.toml"),
                "install"
            )
            .is_ok()
        );

        let err = ensure_linux_default_install_scope(
            &config_at("/home/user/.zeroclaw-p100-104/config.toml"),
            "install",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("only manages the default zeroclaw service"));
        assert!(err.contains("zeroclaw-p100-104"));
    }

    #[test]
    fn linux_journalctl_args_use_named_instance() {
        let config = config_at("/home/user/.zeroclaw-p100-104/config.toml");

        assert_eq!(
            linux_journalctl_args(&config, 50, true),
            [
                "--user",
                "-u",
                "zeroclaw-p100-104.service",
                "-n",
                "50",
                "--no-pager",
                "-f"
            ]
        );
    }

    #[test]
    fn parse_loginctl_linger_property_reads_yes_and_no() {
        assert_eq!(
            parse_loginctl_linger_property("Linger=yes\nUID=1000\n"),
            Some(true)
        );
        assert_eq!(
            parse_loginctl_linger_property("UID=1000\nLinger=no\n"),
            Some(false)
        );
    }

    #[test]
    fn parse_loginctl_linger_property_is_case_and_whitespace_tolerant() {
        assert_eq!(
            parse_loginctl_linger_property("  linger = YeS  \n"),
            Some(true)
        );
        assert_eq!(parse_loginctl_linger_property("LINGER = No\n"), Some(false));
    }

    #[test]
    fn parse_loginctl_linger_property_ignores_unusable_output() {
        assert_eq!(parse_loginctl_linger_property("UID=1000\nName=dan\n"), None);
        assert_eq!(parse_loginctl_linger_property("Linger=maybe\n"), None);
        assert_eq!(parse_loginctl_linger_property(""), None);
    }

    #[test]
    fn systemd_user_linger_status_requires_successful_loginctl() {
        assert_eq!(
            systemd_user_linger_status_from_output("1000".to_string(), false, "Linger=no\n"),
            SystemdUserLinger::Unknown
        );
    }

    #[test]
    fn systemd_user_linger_status_maps_disabled_user_target() {
        assert_eq!(
            systemd_user_linger_status_from_output("1000".to_string(), true, "Linger=no\n"),
            SystemdUserLinger::Disabled {
                user: "1000".to_string()
            }
        );
    }

    #[test]
    fn systemd_linger_hint_names_enable_command() {
        let hint = systemd_linger_hint("1000");
        assert!(hint.contains("may stop after logout"));
        assert!(hint.contains("loginctl enable-linger 1000"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn linux_service_file_stays_default_for_install_path() {
        let file =
            linux_service_file(&config_at("/home/user/.zeroclaw-p100-104/config.toml")).unwrap();
        let path = file.to_string_lossy();
        assert!(path.ends_with(".config/systemd/user/zeroclaw.service"));
    }
}

#[cfg(test)]
mod service_helper_tests {
    use super::*;

    #[test]
    fn xml_escape_escapes_reserved_chars() {
        let escaped = xml_escape("<&>\"' and text");
        assert_eq!(escaped, "&lt;&amp;&gt;&quot;&apos; and text");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_capture_reads_stdout() {
        let out = run_capture(Command::new("sh").args(["-c", "echo hello"]))
            .expect("stdout capture should succeed");
        assert_eq!(out.trim(), "hello");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_capture_falls_back_to_stderr() {
        let out = run_capture(Command::new("sh").args(["-c", "echo warn 1>&2"]))
            .expect("stderr capture should succeed");
        assert_eq!(out.trim(), "warn");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_checked_errors_on_non_zero_status() {
        let err = run_checked(Command::new("sh").args(["-c", "exit 17"]))
            .expect_err("non-zero exit should error");
        assert!(err.to_string().contains("Command failed"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn linux_service_file_has_expected_suffix() {
        let file = linux_service_file(&Config::default()).unwrap();
        let path = file.to_string_lossy();
        assert!(path.ends_with(".config/systemd/user/zeroclaw.service"));
    }

    #[test]
    fn windows_task_name_is_constant() {
        assert_eq!(windows_task_name(), "ZeroClaw Daemon");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_capture_reads_stdout_windows() {
        let out = run_capture(Command::new("cmd").args(["/C", "echo hello"]))
            .expect("stdout capture should succeed");
        assert_eq!(out.trim(), "hello");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_checked_errors_on_non_zero_status_windows() {
        let err = run_checked(Command::new("cmd").args(["/C", "exit /b 17"]))
            .expect_err("non-zero exit should error");
        assert!(err.to_string().contains("Command failed"));
    }

    #[test]
    fn init_system_from_str_parses_valid_values() {
        assert_eq!("auto".parse::<InitSystem>().unwrap(), InitSystem::Auto);
        assert_eq!("AUTO".parse::<InitSystem>().unwrap(), InitSystem::Auto);
        assert_eq!(
            "systemd".parse::<InitSystem>().unwrap(),
            InitSystem::Systemd
        );
        assert_eq!(
            "SYSTEMD".parse::<InitSystem>().unwrap(),
            InitSystem::Systemd
        );
        assert_eq!("openrc".parse::<InitSystem>().unwrap(), InitSystem::Openrc);
        assert_eq!("OPENRC".parse::<InitSystem>().unwrap(), InitSystem::Openrc);
    }

    #[test]
    fn init_system_from_str_rejects_unknown() {
        let err = "unknown"
            .parse::<InitSystem>()
            .expect_err("should reject unknown");
        assert!(err.to_string().contains("Unknown init system"));
        assert!(err.to_string().contains("Supported: auto, systemd, openrc"));
    }

    #[test]
    fn init_system_default_is_auto() {
        assert_eq!(InitSystem::default(), InitSystem::Auto);
    }

    #[cfg(unix)]
    #[test]
    fn is_root_matches_system_uid() {
        // SAFETY: `getuid()` is a simple system call that returns the real user ID of the calling
        // process. It is always safe to call as it takes no arguments and returns a scalar value.
        // This test verifies our `is_root()` wrapper returns the same result as the raw syscall.
        assert_eq!(is_root(), unsafe { libc::getuid() == 0 });
    }

    #[test]
    fn generate_openrc_script_contains_required_directives() {
        use std::path::PathBuf;

        let exe_path = PathBuf::from("/usr/local/bin/zeroclaw");
        let script = generate_openrc_script(&exe_path, Path::new("/etc/zeroclaw"));

        assert!(script.starts_with("#!/sbin/openrc-run"));
        assert!(script.contains("name=\"zeroclaw\""));
        assert!(script.contains("description=\"ZeroClaw daemon\""));
        assert!(script.contains("command=\"/usr/local/bin/zeroclaw\""));
        assert!(script.contains("command_args=\"--config-dir /etc/zeroclaw daemon\""));
        assert!(!script.contains("env ZEROCLAW_CONFIG_DIR"));
        assert!(!script.contains("env ZEROCLAW_WORKSPACE"));
        assert!(script.contains("command_background=\"yes\""));
        assert!(script.contains("command_user=\"zeroclaw:zeroclaw\""));
        assert!(script.contains("pidfile=\"/run/${RC_SVCNAME}.pid\""));
        assert!(script.contains("umask 027"));
        assert!(script.contains(
            "output_logger=\"/usr/local/bin/zeroclaw service run-openrc-log-writer stdout\""
        ));
        assert!(script.contains(
            "error_logger=\"/usr/local/bin/zeroclaw service run-openrc-log-writer stderr\""
        ));
        assert!(!script.contains("output_log="));
        assert!(!script.contains("error_log="));
        assert!(script.contains("depend()"));
        assert!(script.contains("need net"));
        assert!(script.contains("after firewall"));
    }

    #[test]
    fn generate_openrc_script_sets_home_for_browser() {
        use std::path::PathBuf;

        let exe_path = PathBuf::from("/usr/local/bin/zeroclaw");
        let script = generate_openrc_script(&exe_path, Path::new("/etc/zeroclaw"));

        assert!(
            script.contains("export HOME=\"/var/lib/zeroclaw\""),
            "OpenRC script must set HOME for headless browser support"
        );
    }

    #[test]
    fn generate_openrc_script_creates_home_directory() {
        use std::path::PathBuf;

        let exe_path = PathBuf::from("/usr/local/bin/zeroclaw");
        let script = generate_openrc_script(&exe_path, Path::new("/etc/zeroclaw"));

        assert!(
            script.contains("start_pre()"),
            "OpenRC script must have start_pre to create HOME dir"
        );
        assert!(
            script.contains("checkpath --directory --owner zeroclaw:zeroclaw"),
            "start_pre must ensure /var/lib/zeroclaw exists with correct ownership"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_single_quote_escapes_single_quotes() {
        assert_eq!(
            shell_single_quote("/tmp/weird'path"),
            "'/tmp/weird'\"'\"'path'"
        );
    }

    #[test]
    fn openrc_executable_path_rejects_shell_syntax() {
        assert!(openrc_executable_path_is_safe(Path::new(
            "/usr/local/bin/zeroclaw"
        )));
        assert!(!openrc_executable_path_is_safe(Path::new(
            "/opt/zero claw/bin/zeroclaw"
        )));
        assert!(!openrc_executable_path_is_safe(Path::new(
            "/opt/$zero/bin/zeroclaw"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn openrc_writability_probe_prefers_runuser_when_available() {
        let (program, args) =
            build_openrc_writability_probe_command(Path::new("/etc/zeroclaw"), true);
        assert_eq!(program, "runuser");
        assert_eq!(
            args,
            vec![
                "-u".to_string(),
                "zeroclaw".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "test -w '/etc/zeroclaw'".to_string()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn openrc_writability_probe_falls_back_to_su() {
        let (program, args) =
            build_openrc_writability_probe_command(Path::new("/etc/zeroclaw/workspace"), false);
        assert_eq!(program, "su");
        assert_eq!(
            args,
            vec![
                "-s".to_string(),
                "/bin/sh".to_string(),
                "-c".to_string(),
                "test -w '/etc/zeroclaw/workspace'".to_string(),
                "zeroclaw".to_string()
            ]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn tail_file_errors_on_missing_file() {
        let missing = Path::new("/tmp/zeroclaw-test-nonexistent-log-file.log");
        let result = tail_file(missing, 10, false);
        assert!(result.is_err(), "tail on missing file should fail");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn tail_file_reads_existing_file() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let log = dir.path().join("test-tail.log");
        fs::write(&log, "line1\nline2\nline3\nline4\nline5\n").unwrap();
        // tail should succeed on existing file
        let result = tail_file(&log, 3, false);
        assert!(result.is_ok(), "tail on existing file should succeed");
    }
}
