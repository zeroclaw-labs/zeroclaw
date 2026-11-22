#[path = "task_ssh_factors.rs"]
mod factors;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    process::{Child, Command},
    sync::Mutex,
    task::JoinSet,
    time::{sleep, timeout},
};
use zeroclaw_config::{
    schema::{Config, ScopedCredentialSource, ScopedHttpAuth, ScopedHttpCredential},
    secrets::SecretStore,
};

const SSH: &str = "/usr/bin/ssh";
const FOREGROUND: [&str; 2] = ["-o", "ForkAfterAuthentication=no"];
const FRAME_LIMIT: u64 = 16_384;

fn message(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

#[derive(Serialize, Deserialize)]
struct Request {
    argv: Vec<String>,
    credential: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn parse(args: &[String]) -> Result<(&[String], &str, &[String])> {
    let invalid = || anyhow::Error::msg(message("tool-native-ssh-arguments"));
    let mut i = 0;
    while let Some(flag) = args.get(i).filter(|arg| arg.starts_with('-')) {
        if flag == "--" {
            break;
        }
        let value = args.get(i + 1).ok_or_else(invalid)?;
        let allowed = match flag.as_str() {
            "-p" => value.parse::<u16>().is_ok_and(|port| port != 0),
            "-l" => {
                !value.is_empty()
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            }
            "-o" => value.split_once('=').is_some_and(|(key, value)| {
                !value.is_empty()
                    && !value.chars().any(char::is_control)
                    && matches!(
                        key.to_ascii_lowercase().as_str(),
                        "batchmode" | "connecttimeout" | "loglevel"
                    )
            }),
            _ => false,
        };
        if !allowed {
            return Err(invalid());
        }
        i += 2;
    }
    let options = &args[..i];
    let rest = if args.get(i).is_some_and(|arg| arg == "--") {
        &args[i + 1..]
    } else {
        &args[i..]
    };
    let (host, commands) = rest.split_first().ok_or_else(invalid)?;
    ensure!(
        !commands.is_empty()
            && !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:@[]-".contains(&b)),
        "{}",
        invalid()
    );
    Ok((options, host, commands))
}

fn directory() -> PathBuf {
    if let Some(root) = std::env::var_os("ZEROCLAW_SSH_STATE_DIR") {
        return root.into();
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join(format!("zeroclaw-ssh-{}", unsafe { libc::geteuid() }))
}

fn valid_task(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn child() -> Command {
    let mut command = Command::new("/usr/bin/setpriv");
    command
        .args(["--pdeathsig", "TERM"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn output(command: &mut Command, limit: u64) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let out = child.stdout.take().context("missing stdout")?;
    let mut result = Vec::new();
    out.take(limit + 1).read_to_end(&mut result).await?;
    ensure!(
        result.len() as u64 <= limit && child.wait().await?.success(),
        "command failed"
    );
    Ok(result)
}

fn record(alias: &str, effective: &str) -> Result<(String, ScopedCredentialSource, String)> {
    let root = PathBuf::from(
        std::env::var_os("ZEROCLAW_SSH_CREDENTIAL_DIR").context("missing catalog directory")?,
    );
    record_at(&root, alias, effective)
}

fn record_at(
    root: &Path,
    alias: &str,
    effective: &str,
) -> Result<(String, ScopedCredentialSource, String)> {
    ensure!(ScopedHttpCredential::valid_name(alias), "invalid alias");
    let config: Config = toml::from_str(&std::fs::read_to_string(root.join("config.toml"))?)?;
    let stored = config
        .http_request
        .scoped_credentials
        .get(alias)
        .filter(|value| config.secrets.encrypt && value.starts_with("enc2:"))
        .context("missing encrypted credential")?;
    let decoded = SecretStore::new(root, true).decrypt(stored)?;
    let record: ScopedHttpCredential = serde_json::from_str(&decoded)?;
    ensure_scope(&record, effective)?;
    enforce_selected_profile(&record, alias, effective)?;
    let ScopedHttpAuth::SshKey { private_key, .. } = record.auth else {
        anyhow::bail!("wrong credential kind");
    };
    Ok((
        format!("{:x}", Sha256::digest(stored)),
        record.source,
        private_key,
    ))
}

// Existing key records may predate named profiles. Once an operator
// configures profiles for an alias, that alias must follow the exact profile.
fn enforce_selected_profile(
    record: &ScopedHttpCredential,
    alias: &str,
    effective: &str,
) -> Result<()> {
    use zeroclaw_config::ssh_auth_profile::SshAuthentication;

    if record.ssh_profiles.is_empty() {
        return Ok(());
    }
    let host = ssh_setting(effective, "hostname").context("missing effective hostname")?;
    let port = ssh_setting(effective, "port")
        .context("missing effective port")?
        .parse::<u16>()?;
    let username = ssh_setting(effective, "user").context("missing effective user")?;
    let mut profiles = record
        .ssh_profiles
        .iter()
        .filter(|profile| profile.destination.matches(host, port, username));
    let profile = profiles
        .next()
        .context("No matching SSH authentication profile")?;
    ensure!(
        profiles.next().is_none(),
        "Ambiguous SSH authentication profiles"
    );
    profile.validate()?;
    match &profile.authentication {
        SshAuthentication::PublicKey { key_alias } if key_alias == alias => Ok(()),
        _ => anyhow::bail!("SSH authentication mode is unavailable"),
    }
}

fn ensure_scope(record: &ScopedHttpCredential, effective: &str) -> Result<()> {
    let host = ssh_setting(effective, "hostname").context("missing effective hostname")?;
    let port = ssh_setting(effective, "port")
        .context("missing effective port")?
        .parse::<u16>()?;
    let user = ssh_setting(effective, "user").context("missing effective user")?;
    ensure!(
        record.ssh_grants().is_some_and(|grants| {
            grants
                .iter()
                .any(|destination| destination.matches(host, port, user))
        }),
        "destination denied"
    );
    Ok(())
}

fn ssh_setting<'a>(effective: &'a str, key: &str) -> Option<&'a str> {
    effective
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find_map(|(name, value)| (name == key).then_some(value))
}

fn signer_loader_args(effective: &str, home: Option<&Path>) -> Result<Vec<std::ffi::OsString>> {
    let host = ssh_setting(effective, "hostname").context("missing effective hostname")?;
    let username = ssh_setting(effective, "user").context("missing effective user")?;
    let port = ssh_setting(effective, "port")
        .context("missing effective port")?
        .parse::<u16>()?;
    let valid = |value: &str, punctuation: &[u8]| {
        !value.is_empty()
            && !value.starts_with('-')
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || punctuation.contains(&b))
    };
    ensure!(
        valid(host, b".-:") && valid(username, b"_-") && port != 0,
        "invalid effective destination"
    );
    let lookup = match ssh_setting(effective, "hostkeyalias").filter(|alias| *alias != "none") {
        Some(alias) => {
            ensure!(valid(alias, b".-:"), "invalid host key alias");
            alias.to_owned()
        }
        None if port != 22 => format!("[{host}]:{port}"),
        None => host.to_owned(),
    };
    let mut args = Vec::<std::ffi::OsString>::new();
    for setting in ["userknownhostsfile", "globalknownhostsfile"] {
        for file in ssh_setting(effective, setting)
            .into_iter()
            .flat_map(str::split_whitespace)
        {
            if file == "none" {
                continue;
            }
            let home_relative = file.strip_prefix("~/").or_else(|| file.strip_prefix("%d/"));
            ensure!(
                !home_relative
                    .unwrap_or(file)
                    .contains(['%', '$', '"', '\'', '\\']),
                "unsupported known-hosts path"
            );
            let path = if let Some(relative) = home_relative {
                home.context("missing home for known-hosts path")?
                    .join(relative)
            } else {
                PathBuf::from(file)
            };
            ensure!(path.is_absolute(), "known-hosts path must be absolute");
            args.push("-H".into());
            args.push(path.into_os_string());
        }
    }
    ensure!(!args.is_empty(), "no configured known-hosts files");
    args.extend([
        "-t".into(),
        "20".into(),
        "-h".into(),
        format!("{username}@{lookup}").into(),
        "-".into(),
    ]);
    Ok(args)
}

// A separate process owns the SSH process group. Its stdin is a lease held
// only by the broker; broker SIGKILL closes it even though Rust Drop cannot run.
// The supervisor deliberately survives that death long enough to kill the group.
async fn supervise_master(
    program: std::ffi::OsString,
    args: Vec<std::ffi::OsString>,
) -> Result<i32> {
    // SAFETY: these getters have no preconditions; refuse group 1 (broadcast).
    let group = unsafe { libc::getpgrp() };
    let pid = unsafe { libc::getpid() };
    ensure!(
        pid > 1 && group == pid,
        "supervisor requires its own process group"
    );
    let mut terminated = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let (end, mut ended) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        let _ = end.send(());
    });
    tokio::select! {
        _ = &mut ended => {},
        _ = terminated.recv() => {},
        _ = child.wait() => {},
    }
    // The registered TERM handler keeps this supervisor alive through grace.
    // Group KILL also terminates the supervisor, even if the SSH leader exited.
    unsafe {
        libc::kill(-group, libc::SIGTERM);
    }
    sleep(Duration::from_millis(150)).await;
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
    anyhow::bail!("supervisor group cleanup failed")
}

fn supervised_master_command(command: &Command) -> Result<Command> {
    let source = command.as_std();
    let mut supervisor = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    supervisor
        .arg("--supervise-master")
        .arg(source.get_program())
        .args(source.get_args());
    #[cfg(test)]
    {
        use std::os::unix::ffi::OsStrExt;
        let argv: Vec<Vec<u8>> = std::iter::once(source.get_program())
            .chain(source.get_args())
            .map(|arg| arg.as_bytes().to_vec())
            .collect();
        supervisor
            .args([
                "--exact",
                "task_ssh_broker::tests::supervisor_process_entry",
                "--nocapture",
            ])
            .env(
                "ZEROCLAW_TEST_SSH_SUPERVISOR",
                serde_json::to_string(&argv)?,
            );
    }
    for (key, value) in source.get_envs() {
        match value {
            Some(value) => {
                supervisor.env(key, value);
            }
            None => {
                supervisor.env_remove(key);
            }
        }
    }
    if let Some(directory) = source.get_current_dir() {
        supervisor.current_dir(directory);
    }
    supervisor
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    Ok(supervisor)
}

struct OwnedMaster {
    child: Child,
    input: Option<tokio::process::ChildStdin>,
    group: i32,
}

impl OwnedMaster {
    fn spawn(command: &mut Command) -> Result<Self> {
        let mut child = supervised_master_command(command)?.spawn()?;
        let input = child.stdin.take().context("missing supervisor lease")?;
        let group = child.id().context("missing SSH supervisor pid")? as i32;
        Ok(Self {
            child,
            input: Some(input),
            group,
        })
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    async fn kill(&mut self) -> std::io::Result<()> {
        drop(self.input.take());
        // Kill the group even if the leader has already exited: a proxy can
        // ignore closed pipes and remain in the original process group.
        unsafe {
            libc::kill(-self.group, libc::SIGTERM);
        }
        sleep(Duration::from_millis(150)).await;
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
        let result = self.child.wait().await.map(|_| ());
        self.group = 0;
        result
    }
}

impl Drop for OwnedMaster {
    fn drop(&mut self) {
        drop(self.input.take());
        if self.group > 0 {
            unsafe {
                libc::kill(-self.group, libc::SIGKILL);
            }
            let _ = self.child.start_kill();
        }
    }
}

fn require_unused_socket(path: &Path) -> Result<()> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("SSH socket path already exists"),
    }
}

async fn signer(
    path: &Path,
    source: ScopedCredentialSource,
    value: String,
    effective: &str,
) -> Result<Child> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let loader_args = signer_loader_args(effective, home.as_deref())?;
    let bytes = match source {
        ScopedCredentialSource::Local => value.into_bytes(),
        ScopedCredentialSource::OpenBao => {
            let (program, args) = source.command(&value).context("invalid reference")?;
            timeout(
                Duration::from_secs(12),
                output(Command::new(program).args(args), FRAME_LIMIT),
            )
            .await??
        }
        _ => anyhow::bail!("unsupported SSH credential source"),
    };
    let key = std::str::from_utf8(&bytes)?.trim_end();
    ensure!(
        bytes.len() as u64 <= FRAME_LIMIT
            && key.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----")
            && key.ends_with("-----END OPENSSH PRIVATE KEY-----"),
        "invalid SSH key"
    );
    require_unused_socket(path)?;
    let mut signer = child()
        .args(["/usr/bin/ssh-agent", "-D", "-a"])
        .arg(path)
        .spawn()?;
    timeout(Duration::from_secs(3), async {
        while !path.exists() {
            ensure!(signer.try_wait()?.is_none(), "signer exited");
            sleep(Duration::from_millis(25)).await;
        }
        Result::<()>::Ok(())
    })
    .await??;
    ensure!(signer.try_wait()?.is_none(), "signer exited");
    let mut loader = child()
        .arg("/usr/bin/ssh-add")
        .args(loader_args)
        .env("SSH_AUTH_SOCK", path)
        .env("SSH_ASKPASS_REQUIRE", "never")
        .stdin(Stdio::piped())
        .spawn()?;
    timeout(Duration::from_secs(5), async {
        let mut stdin = loader.stdin.take().context("missing key pipe")?;
        stdin.write_all(key.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        drop(stdin);
        ensure!(loader.wait().await?.success(), "key rejected");
        Result::<()>::Ok(())
    })
    .await??;
    Ok(signer)
}

#[derive(Default)]
struct Entry {
    master: Option<OwnedMaster>,
    failed: bool,
    leases: usize,
    idle_since: Option<Instant>,
}

type Pool = Arc<Mutex<HashMap<PathBuf, Arc<Mutex<Entry>>>>>;

struct SocketPath(PathBuf);
impl Drop for SocketPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn open(
    path: &Path,
    options: &[String],
    host: &str,
    credential: Option<(ScopedCredentialSource, String)>,
    effective: &str,
    factors: Option<&factors::Factors>,
) -> Result<OwnedMaster> {
    require_unused_socket(path)?;
    require_unused_socket(&path.with_extension("agent"))?;
    let socket = SocketPath(path.with_extension("agent"));
    let mut agent = if let Some((source, value)) = credential {
        Some(signer(&socket.0, source, value, effective).await?)
    } else {
        None
    };
    let mut command = child();
    command
        .args([SSH, "-M", "-N", "-S"])
        .arg(path)
        .args(FOREGROUND)
        .args([
            "-o",
            "ControlMaster=yes",
            "-o",
            "ControlPersist=no",
            "-o",
            if factors.is_some() {
                "BatchMode=no"
            } else {
                "BatchMode=yes"
            },
            "-o",
            "ForwardAgent=no",
            "-o",
            "ConnectTimeout=12",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
        ]);
    if agent.is_some() {
        command
            .args(["-o", "StrictHostKeyChecking=yes", "-o"])
            .arg(format!("IdentityAgent={}", socket.0.display()));
        command.args([
            "-o",
            "IdentitiesOnly=no",
            "-o",
            "IdentityFile=none",
            "-o",
            "PreferredAuthentications=publickey",
        ]);
    }
    let challenge_socket = path.with_extension("ask");
    let listener = if let Some(factors) = factors {
        require_unused_socket(&challenge_socket)?;
        let listener = UnixListener::bind(&challenge_socket)?;
        let cleanup = SocketPath(challenge_socket);
        let use_pam = matches!(
            factors.profile.authentication,
            zeroclaw_config::ssh_auth_profile::SshAuthentication::Pam { .. }
        );
        command
            .args([
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "NumberOfPasswordPrompts=1",
                "-o",
                "PubkeyAuthentication=no",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "PermitLocalCommand=no",
                "-o",
                "ProxyCommand=none",
                "-o",
                "ProxyJump=none",
                "-o",
                "ForwardX11=no",
                "-o",
                if use_pam {
                    "PreferredAuthentications=keyboard-interactive"
                } else {
                    "PreferredAuthentications=password"
                },
                "-o",
                if use_pam {
                    "KbdInteractiveAuthentication=yes"
                } else {
                    "PasswordAuthentication=yes"
                },
            ])
            .env("SSH_ASKPASS", std::env::current_exe()?)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(factors::ASKPASS_SOCKET, &cleanup.0);
        Some((listener, cleanup))
    } else {
        None
    };
    let mut master = OwnedMaster::spawn(command.args(options).args(["--", host]))?;
    timeout(Duration::from_secs(30), async {
        if let (Some(factors), Some((listener, _cleanup))) = (factors, listener) {
            factors.authenticate(listener, &mut master, path).await?;
        } else {
            while !path.exists() {
                ensure!(master.try_wait()?.is_none(), "master exited");
                sleep(Duration::from_millis(25)).await;
            }
        }
        Result::<()>::Ok(())
    })
    .await??;
    if let Some(agent) = &mut agent {
        agent.kill().await?;
    }
    ensure!(master.try_wait()?.is_none(), "master exited");
    Ok(master)
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let mut line = Vec::new();
    BufReader::new(stream.take(FRAME_LIMIT + 1))
        .read_until(b'\n', &mut line)
        .await?;
    ensure!(
        line.len() as u64 <= FRAME_LIMIT && line.ends_with(b"\n"),
        "invalid frame"
    );
    Ok(serde_json::from_slice(&line)?)
}

async fn write<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    ensure!(bytes.len() as u64 <= FRAME_LIMIT, "frame too large");
    stream.write_all(&bytes).await?;
    Ok(())
}

async fn lease(
    stream: &mut UnixStream,
    dir: &Path,
    pool: &Pool,
) -> Result<(PathBuf, Arc<Mutex<Entry>>)> {
    let request: Request = timeout(Duration::from_secs(5), read(stream)).await??;
    let (options, host, _) = parse(&request.argv)?;
    let effective = timeout(
        Duration::from_secs(10),
        output(
            Command::new(SSH)
                .arg("-G")
                .args(FOREGROUND)
                .args(options)
                .args(["--", host]),
            65_536,
        ),
    )
    .await??;
    let effective = std::str::from_utf8(&effective)?;
    let alias = request
        .credential
        .or_else(|| std::env::var("ZEROCLAW_SSH_DEFAULT_CREDENTIAL").ok());
    let factors = alias
        .as_deref()
        .map(|alias| factors::select(alias, effective))
        .transpose()
        .context("tool-native-ssh-credential")?
        .flatten();
    let credential = if factors.is_some() {
        None
    } else {
        alias
            .as_deref()
            .map(|alias| record(alias, effective))
            .transpose()
            .context("tool-native-ssh-credential")?
    };
    let factor_fingerprint = factors
        .as_ref()
        .map(factors::Factors::fingerprint)
        .transpose()?;
    let fingerprint = serde_json::to_vec(&(
        host,
        effective,
        alias,
        credential.as_ref().map(|c| &c.0),
        factor_fingerprint,
    ))?;
    let key = format!("{:x}", Sha256::digest(fingerprint));
    let path = dir.join(&key[..24]);
    let entry = pool.lock().await.entry(path.clone()).or_default().clone();
    let mut state = entry.lock().await;
    ensure!(!state.failed, "failed connection");
    if state.master.is_none() {
        state.failed = true;
        state.master = Some(
            open(
                &path,
                options,
                host,
                credential.map(|(_, source, value)| (source, value)),
                effective,
                factors.as_ref(),
            )
            .await
            .context("tool-native-ssh-connection")?,
        );
        state.failed = false;
    }
    ensure!(
        state
            .master
            .as_mut()
            .context("missing master")?
            .try_wait()?
            .is_none(),
        "master exited"
    );
    if let Some(factors) = &factors {
        factors.revalidate()?;
    }
    state.leases += 1;
    drop(state);
    Ok((path, entry))
}

async fn serve(id: &str) -> Result<()> {
    ensure!(valid_task(id), "invalid task");
    let root = directory();
    let create = |path: &Path| std::fs::DirBuilder::new().mode(0o700).create(path);
    if let Err(e) = create(&root)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(e.into());
    }
    let info = root.symlink_metadata()?;
    ensure!(
        info.is_dir() && info.uid() == unsafe { libc::geteuid() } && info.mode() & 0o077 == 0,
        "insecure directory"
    );
    let dir = root.join(id);
    let idle =
        std::env::var("ZEROCLAW_SSH_IDLE_TIMEOUT_SECS").map_or(Ok(1800), |s| s.parse::<u64>())?;
    ensure!((1..=86_400).contains(&idle), "invalid idle interval");
    create(&dir)?;
    // The directory was created exclusively above and is owned by this task.
    let result = serve_inner(
        &root,
        &dir,
        Duration::from_secs(idle),
        Pool::default(),
        std::io::stdin(),
    )
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

async fn retire_idle(pool: &Pool, idle: Duration) {
    let entries: Vec<_> = pool
        .lock()
        .await
        .iter()
        .map(|(path, entry)| (path.clone(), Arc::clone(entry)))
        .collect();
    let mut cleanup = JoinSet::new();
    for (path, entry) in entries {
        cleanup.spawn(async move {
            if let Ok(mut state) = entry.try_lock()
                && state.leases == 0
                && state.idle_since.is_some_and(|time| time.elapsed() >= idle)
            {
                if let Some(mut master) = state.master.take() {
                    state.failed = !matches!(master.try_wait(), Ok(None));
                    let _ = master.kill().await;
                }
                // Hold this entry's lock through termination so an old master
                // cannot unlink a replacement connection's control socket.
                let _ = std::fs::remove_file(path);
                state.idle_since = None;
            }
        });
    }
    while cleanup.join_next().await.is_some() {}
}

async fn stop_masters(pool: &Pool) {
    let entries: Vec<_> = pool.lock().await.values().cloned().collect();
    let mut cleanup = JoinSet::new();
    for entry in entries {
        cleanup.spawn(async move {
            if let Some(mut master) = entry.lock().await.master.take() {
                let _ = master.kill().await;
            }
        });
    }
    while cleanup.join_next().await.is_some() {}
}

async fn serve_inner(
    root: &Path,
    dir: &Path,
    idle: Duration,
    pool: Pool,
    mut input: impl std::io::Read + Send + 'static,
) -> Result<()> {
    let path = dir.join("broker");
    let listener = UnixListener::bind(&path)?;
    let _socket = SocketPath(path);
    println!("{}", serde_json::json!({"ready":true,"state_dir":root}));
    let mut jobs = JoinSet::new();
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    // Reading stdin on a dedicated thread avoids a blocking Tokio stdin task.
    let (end, mut ended) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut input, &mut std::io::sink());
        let _ = end.send(());
    });
    loop {
        tokio::select! {
            biased;
            _ = &mut ended => break,
            Some(_) = jobs.join_next(), if !jobs.is_empty() => {},
            _ = ticks.tick() => {
                retire_idle(&pool, idle).await;
            },
            incoming = listener.accept(), if jobs.len() < 64 => {
                let (mut stream, _) = incoming?;
                let (dir, pool) = (dir.to_owned(), pool.clone());
                jobs.spawn(async move {
                    match lease(&mut stream, &dir, &pool).await {
                        Ok((path, entry)) => {
                            if write(&mut stream, &Reply {path:Some(path), error:None}).await.is_ok() {
                                let _ = stream.read_u8().await;
                            }
                            let mut state = entry.lock().await;
                            state.leases -= 1;
                            state.idle_since = Some(Instant::now());
                        }
                        Err(error) => {
                            let key = error.chain()
                                .map(|cause| cause.to_string())
                                .find(|text| matches!(text.as_str(), "tool-native-ssh-credential" | "tool-native-ssh-connection"))
                                .unwrap_or_else(|| "tool-native-ssh-denied".into());
                            let _ = write(&mut stream, &Reply {path:None, error:Some(key)}).await;
                        }
                    }
                });
            }
        }
    }
    jobs.shutdown().await;
    stop_masters(&pool).await;
    Ok(())
}

async fn client(mut args: Vec<String>) -> Result<i32> {
    let id = std::env::var("ZEROCLAW_SSH_TASK")?;
    ensure!(valid_task(&id), "invalid task");
    let credential = if args.first().is_some_and(|arg| arg == "--credential") {
        ensure!(args.len() >= 3, "missing alias");
        args.remove(0);
        Some(args.remove(0))
    } else {
        None
    };
    let (options, host, command) = parse(&args)?;
    let mut stream = UnixStream::connect(directory().join(id).join("broker")).await?;
    write(
        &mut stream,
        &Request {
            argv: args.clone(),
            credential,
        },
    )
    .await?;
    let reply: Reply = timeout(Duration::from_secs(48), read(&mut stream)).await??;
    let error_key = reply
        .error
        .as_deref()
        .filter(|key| {
            matches!(
                *key,
                "tool-native-ssh-credential" | "tool-native-ssh-connection"
            )
        })
        .unwrap_or("tool-native-ssh-denied");
    let path = reply.path.context(message(error_key))?;
    let status = Command::new(SSH)
        .arg("-S")
        .arg(path)
        .args(FOREGROUND)
        .args([
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPersist=no",
            "-o",
            "ProxyCommand=/usr/bin/false",
            "-o",
            "BatchMode=yes",
            "-o",
            "ForwardAgent=no",
        ])
        .args(options)
        .args(["--", host])
        .args(command)
        .kill_on_drop(true)
        .status()
        .await?;
    Ok(status.code().unwrap_or(255))
}

pub fn run(args: Vec<String>) -> i32 {
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| {
            runtime.block_on(async {
                if let Some(socket) = std::env::var_os(factors::ASKPASS_SOCKET)
                    && args.first().is_none_or(|arg| arg != "--supervise-master")
                {
                    return factors::askpass(socket.into(), &args).await;
                }
                if args.first().is_some_and(|arg| arg == "--supervise-master") {
                    ensure!(
                        args.len() > 5
                            && args[1] == "/usr/bin/setpriv"
                            && args[2] == "--pdeathsig"
                            && args[3] == "TERM"
                            && args[4] == SSH
                            && args[5] == "-M",
                        "invalid supervisor command"
                    );
                    return supervise_master(
                        args[1].clone().into(),
                        args[2..].iter().map(Into::into).collect(),
                    )
                    .await;
                }
                if let [flag, id] = args.as_slice()
                    && flag == "--serve"
                {
                    serve(id).await?;
                    return Ok(0);
                }
                client(args).await
            })
        });
    match result {
        Ok(code) => code,
        Err(error) => {
            let display = error.to_string();
            let allowed = [
                "tool-native-ssh-credential",
                "tool-native-ssh-connection",
                "tool-native-ssh-arguments",
            ]
            .iter()
            .map(|key| message(key))
            .find(|text| *text == display);
            eprintln!(
                "{}",
                allowed.unwrap_or_else(|| message("tool-native-ssh-denied"))
            );
            255
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Entry, Pool, SecretStore, SocketPath, parse, record_at, retire_idle, serve_inner,
        valid_task,
    };
    use super::{OwnedMaster, require_unused_socket, signer_loader_args};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::{process::Command, sync::Mutex};

    // These two entries run only in isolated child test processes. They are
    // fixtures for the real abrupt-parent-death regression below.
    #[test]
    fn supervisor_process_entry() {
        let Ok(payload) = std::env::var("ZEROCLAW_TEST_SSH_SUPERVISOR") else {
            return;
        };
        use std::os::unix::ffi::OsStringExt;
        let mut argv: Vec<std::ffi::OsString> = serde_json::from_str::<Vec<Vec<u8>>>(&payload)
            .unwrap()
            .into_iter()
            .map(std::ffi::OsString::from_vec)
            .collect();
        let program = argv.remove(0);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(super::supervise_master(program, argv))
            .unwrap();
    }

    #[test]
    fn broker_process_entry() {
        let Some(pidfile) = std::env::var_os("ZEROCLAW_TEST_BROKER_PROXY") else {
            return;
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let master = OwnedMaster::spawn(
                    Command::new("/bin/sh")
                        .arg("-c")
                        .arg(r#"trap '' TERM; sleep 60 & echo $! > "$1"; wait"#)
                        .arg("proxy-fixture")
                        .arg(&pidfile),
                )
                .unwrap();
                std::fs::write(
                    std::path::Path::new(&pidfile).with_extension("group"),
                    master.group.to_string(),
                )
                .unwrap();
                std::future::pending::<()>().await;
                drop(master);
            });
    }

    #[tokio::test]
    async fn abrupt_broker_death_stops_term_ignoring_proxy_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("proxy.pid");
        let mut broker = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "task_ssh_broker::tests::broker_process_entry",
                "--nocapture",
            ])
            .env("ZEROCLAW_TEST_BROKER_PROXY", &pidfile)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let (proxy, group) = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let proxy = std::fs::read_to_string(&pidfile)
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok());
                let group = std::fs::read_to_string(pidfile.with_extension("group"))
                    .ok()
                    .and_then(|s| s.trim().parse::<i32>().ok());
                if let (Some(proxy), Some(group)) = (proxy, group) {
                    break (proxy, group);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("proxy fixture must become ready");
        // SIGKILL: no broker destructor or normal EOF closeout can run.
        broker.kill().await.unwrap();
        broker.wait().await.unwrap();
        let stopped = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let alive = |pid| {
                    std::fs::read_to_string(format!("/proc/{pid}/stat"))
                        .is_ok_and(|s| s.split_whitespace().nth(2) != Some("Z"))
                };
                if !alive(proxy) && !alive(group) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if stopped.is_err() {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        assert!(
            stopped.is_ok(),
            "proxy and supervisor must stop after broker SIGKILL"
        );
    }

    #[test]
    fn prebound_fake_agent_and_master_sockets_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["agent", "master"] {
            let path = dir.path().join(name);
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            assert!(require_unused_socket(&path).is_err());
            assert!(path.exists());
            drop(listener);
        }
    }

    #[tokio::test]
    async fn signer_rejects_prebound_socket_before_delivering_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("fixture");
        assert!(
            std::process::Command::new("/usr/bin/ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(&key)
                .status()
                .unwrap()
                .success()
        );
        let path = dir.path().join("fake-agent");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let result = super::signer(
            &path, zeroclaw_config::schema::ScopedCredentialSource::Local,
            std::fs::read_to_string(&key).unwrap(),
            "hostname fixture.example\nport 22\nuser fixture\nuserknownhostsfile /tmp/fixture-known-hosts\nglobalknownhostsfile none\n"
        ).await;
        assert!(result.is_err());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(path.exists());
    }

    #[tokio::test]
    async fn owned_master_stops_proxy_after_leader_exits() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("proxy.pid");
        let mut master = OwnedMaster::spawn(
            Command::new("/bin/sh")
                .arg("-c")
                .arg(r#"trap '' TERM; sleep 60 & echo $! > "$1"; exit 0"#)
                .arg("fixture")
                .arg(&pidfile),
        )
        .unwrap();
        master.child.wait().await.unwrap();
        let pid = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        master.kill().await.unwrap();
        for _ in 0..40 {
            let status = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            if status.is_err() || status.unwrap().split_whitespace().nth(2) == Some("Z") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("proxy remained alive after task shutdown");
    }

    #[tokio::test]
    async fn idle_expiry_retires_live_master_and_marks_dead_master_failed() {
        let dir = tempfile::tempdir().unwrap();
        let pooled = Pool::default();
        for (name, alive, leases) in [("idle", true, 0), ("dead", false, 0), ("leased", true, 1)] {
            let socket = dir.path().join(name);
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let mut master = if alive {
                OwnedMaster::spawn(Command::new("/usr/bin/sleep").arg("60")).unwrap()
            } else {
                OwnedMaster::spawn(&mut Command::new("/usr/bin/false")).unwrap()
            };
            if !alive {
                assert!(!master.child.wait().await.unwrap().success());
            }
            let entry = Arc::new(Mutex::new(Entry {
                master: Some(master),
                idle_since: Some(Instant::now()),
                leases,
                ..Entry::default()
            }));
            pooled
                .lock()
                .await
                .insert(socket.clone(), Arc::clone(&entry));
            retire_idle(&pooled, Duration::ZERO).await;
            let state = entry.lock().await;
            assert_eq!(state.failed, !alive);
            assert_eq!(state.master.is_some(), leases != 0);
            assert_eq!(state.idle_since.is_some(), leases != 0);
            assert_eq!(socket.exists(), leases != 0);
            drop(listener);
        }
    }

    #[tokio::test]
    async fn many_connections_close_within_the_task_cleanup_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::default();
        for index in 0..70 {
            let master = OwnedMaster::spawn(Command::new("/usr/bin/sleep").arg("60")).unwrap();
            pool.lock().await.insert(
                dir.path().join(index.to_string()),
                Arc::new(Mutex::new(Entry {
                    master: Some(master),
                    ..Entry::default()
                })),
            );
        }
        let (input, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(writer);
        tokio::time::timeout(
            Duration::from_secs(3),
            serve_inner(dir.path(), dir.path(), Duration::from_secs(60), pool, input),
        )
        .await
        .expect("seventy masters must stop before the task's ten-second deadline")
        .unwrap();
    }

    #[tokio::test]
    async fn input_eof_stops_task_master_and_unlinks_broker_socket() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::default();
        let master = OwnedMaster::spawn(Command::new("/usr/bin/sleep").arg("60")).unwrap();
        let pid = master.child.id().unwrap();
        let entry = Arc::new(Mutex::new(Entry {
            master: Some(master),
            leases: 1,
            ..Entry::default()
        }));
        pool.lock()
            .await
            .insert(dir.path().join("master"), Arc::clone(&entry));
        let (input, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let socket = dir.path().join("broker");
        let close = async {
            while !socket.exists() {
                tokio::task::yield_now().await;
            }
            drop(writer);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                serve_inner(dir.path(), dir.path(), Duration::from_secs(60), pool, input),
                close
            )
        })
        .await
        .expect("broker must finish after EOF");
        result.unwrap();
        assert!(entry.lock().await.master.is_none());
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(!socket.exists());
    }

    #[test]
    fn signer_loader_requires_destination_scope_and_known_host_files() {
        let base = "hostname build.example\nport 2222\nuser runner\nuserknownhostsfile ~/.ssh/known_hosts\nglobalknownhostsfile none\n";
        let args = signer_loader_args(base, Some(Path::new("/home/test"))).unwrap();
        assert_eq!(
            args,
            [
                "-H",
                "/home/test/.ssh/known_hosts",
                "-t",
                "20",
                "-h",
                "runner@[build.example]:2222",
                "-"
            ]
        );
        let tokenized = base.replace("~/", "%d/");
        assert_eq!(
            signer_loader_args(&tokenized, Some(Path::new("/home/test"))).unwrap(),
            args
        );
        assert!(signer_loader_args(&tokenized, None).is_err());
        let unsupported = base.replace("~/", "%h/");
        assert!(signer_loader_args(&unsupported, Some(Path::new("/home/test"))).is_err());
        let alias = format!("{base}hostkeyalias build-pin\n");
        let args = signer_loader_args(&alias, Some(Path::new("/home/test"))).unwrap();
        assert_eq!(args[5], "runner@build-pin");
        assert!(signer_loader_args(base, None).is_err());
        assert!(signer_loader_args("hostname build.example\nport 22\nuser runner", None).is_err());
    }

    #[test]
    fn signer_socket_path_is_removed_on_scope_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let cleanup = SocketPath(path.clone());
        assert!(path.exists());
        drop(cleanup);
        assert!(!path.exists());
        drop(listener);
    }
    use std::path::Path;

    fn write_catalog(root: &Path, auth: serde_json::Value, encrypted: bool, encrypt_enabled: bool) {
        let record = serde_json::json!({ "origin": "", "auth": auth }).to_string();
        let stored = if encrypted {
            SecretStore::new(root, true).encrypt(&record).unwrap()
        } else {
            record
        };
        let config = serde_json::json!({
            "secrets": {"encrypt": encrypt_enabled},
            "http_request": {"scoped_credentials": {"FIXTURE": stored}}
        });
        std::fs::write(root.join("config.toml"), toml::to_string(&config).unwrap()).unwrap();
    }

    #[test]
    fn encrypted_key_is_limited_to_the_effective_host_port_and_user() {
        let dir = tempfile::tempdir().unwrap();
        write_catalog(
            dir.path(),
            serde_json::json!({
                "kind": "ssh_key",
                "private_key": "synthetic-key",
                "destinations": [{"host": "build.example", "port": 2222, "username": "runner"}]
            }),
            true,
            true,
        );
        let allowed = "hostname BUILD.example\nport 2222\nuser runner\n";
        let (revision, _, key) = record_at(dir.path(), "FIXTURE", allowed).unwrap();
        assert_eq!(revision.len(), 64);
        assert_eq!(key, "synthetic-key");
        assert!(record_at(dir.path(), "MISSING", allowed).is_err());
        for effective in [
            "hostname other.example\nport 2222\nuser runner\n",
            "hostname build.example\nport 22\nuser runner\n",
            "hostname build.example\nport 2222\nuser other\n",
            "hostname build.example\nuser runner\n",
        ] {
            assert!(record_at(dir.path(), "FIXTURE", effective).is_err());
        }
    }

    #[test]
    fn catalog_rejects_plaintext_disabled_encryption_and_non_ssh_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let key = serde_json::json!({
            "kind": "ssh_key",
            "private_key": "synthetic-key",
            "destinations": [{"host": "build.example", "port": 2222, "username": "runner"}]
        });
        let effective = "hostname build.example\nport 2222\nuser runner\n";
        write_catalog(dir.path(), key.clone(), false, true);
        assert!(record_at(dir.path(), "FIXTURE", effective).is_err());
        write_catalog(dir.path(), key, true, false);
        assert!(record_at(dir.path(), "FIXTURE", effective).is_err());
        write_catalog(
            dir.path(),
            serde_json::json!({"kind": "api_key", "value": "synthetic-token"}),
            true,
            true,
        );
        assert!(record_at(dir.path(), "FIXTURE", effective).is_err());
    }

    #[test]
    fn only_managed_options_and_explicit_commands_are_admitted() {
        let args = ["-p", "2222", "--", "example", "-O", "exit"].map(str::to_owned);
        let (options, host, command) = parse(&args).unwrap();
        assert_eq!(options, ["-p", "2222"]);
        assert_eq!(host, "example");
        assert_eq!(command, ["-O", "exit"]);
        for args in [
            vec!["-o", "ProxyCommand=other", "example", "true"],
            vec!["-o", "BatchMode=yes\nProxyCommand=other", "example", "true"],
            vec!["-p", "0", "example", "true"],
            vec!["example"],
        ] {
            assert!(parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        }
        assert!(valid_task("0123456789abcdef0123456789abcdef"));
        assert!(!valid_task("../0123456789abcdef0123456789abc"));
    }
}

#[cfg(test)]
#[test]
fn scoped_ssh_admission_checks_hostname_port_and_user_for_each_factor() -> Result<()> {
    for auth in [
        serde_json::json!({"kind":"user_password","username":"fixture","password":"synthetic"}),
        serde_json::json!({"kind":"totp","seed":"synthetic"}),
    ] {
        let mut entry: ScopedHttpCredential = serde_json::from_value(serde_json::json!({
            "origin":"",
            "auth":auth,
            "ssh_factor_destinations":[{"host":"guest.example.test","port":2222,"username":"fixture"}]
        }))?;
        let allowed = "hostname GUEST.example.test
port 2222
user fixture";
        ensure_scope(&entry, allowed)?;
        for wrong in [
            "hostname other.example.test
port 2222
user fixture",
            "hostname guest.example.test
port 22
user fixture",
            "hostname guest.example.test
port 2222
user other",
        ] {
            assert!(ensure_scope(&entry, wrong).is_err());
        }
        entry.ssh_factor_destinations.clear();
        assert!(ensure_scope(&entry, allowed).is_err());
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn saved_ssh_profile_cannot_fall_back_to_another_mode_or_destination() -> Result<()> {
    let mut record: ScopedHttpCredential = serde_json::from_value(serde_json::json!({
        "origin":"",
        "auth":{"kind":"ssh_key","private_key":"synthetic","destinations":[]},
        "ssh_profiles":[{
            "destination":{"host":"guest.example.test","port":2222,"username":"fixture"},
            "authentication":{"mode":"public_key","key_alias":"KEY"}
        }]
    }))?;
    let effective = "hostname guest.example.test
port 2222
user fixture";
    enforce_selected_profile(&record, "KEY", effective)?;
    assert!(enforce_selected_profile(&record, "OTHER", effective).is_err());
    assert!(
        enforce_selected_profile(
            &record,
            "KEY",
            "hostname guest.example.test
port 22
user fixture"
        )
        .is_err()
    );
    record.ssh_profiles[0].authentication =
        zeroclaw_config::ssh_auth_profile::SshAuthentication::Password {
            password_alias: "PASSWORD".into(),
        };
    assert!(enforce_selected_profile(&record, "KEY", effective).is_err());
    Ok(())
}
