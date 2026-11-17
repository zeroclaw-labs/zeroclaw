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
    let ScopedHttpAuth::SshKey {
        private_key,
        destinations,
    } = record.auth
    else {
        anyhow::bail!("wrong credential kind");
    };
    let get = |key| ssh_setting(effective, key);
    let port = get("port").and_then(|port| port.parse::<u16>().ok());
    ensure!(
        destinations.iter().any(|entry| get("hostname")
            .is_some_and(|host| entry.host.eq_ignore_ascii_case(host))
            && port == Some(entry.port)
            && get("user") == Some(entry.username.as_str())),
        "destination denied"
    );
    Ok((
        format!("{:x}", Sha256::digest(stored)),
        record.source,
        private_key,
    ))
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
            ensure!(
                !file.contains(['%', '$', '"', '\'', '\\']),
                "unsupported known-hosts path"
            );
            let path = if let Some(relative) = file.strip_prefix("~/") {
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
    master: Option<Child>,
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
) -> Result<Child> {
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
            "BatchMode=yes",
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
    let mut master = command.args(options).args(["--", host]).spawn()?;
    timeout(Duration::from_secs(15), async {
        while !path.exists() {
            ensure!(master.try_wait()?.is_none(), "master exited");
            sleep(Duration::from_millis(25)).await;
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
    let credential = alias
        .as_deref()
        .map(|alias| record(alias, effective))
        .transpose()?;
    let fingerprint =
        serde_json::to_vec(&(host, effective, alias, credential.as_ref().map(|c| &c.0)))?;
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
            )
            .await?,
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
    let mut entries = pool.lock().await;
    for (path, entry) in entries.iter_mut() {
        if let Ok(mut state) = entry.try_lock()
            && state.leases == 0
            && state.idle_since.is_some_and(|time| time.elapsed() >= idle)
        {
            if let Some(mut master) = state.master.take() {
                // Only a live master retired by this broker is eligible for reuse.
                state.failed = !matches!(master.try_wait(), Ok(None));
                let _ = master.kill().await;
            }
            let _ = std::fs::remove_file(path);
            state.idle_since = None;
        }
    }
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
                            if write(&mut stream, &Reply {path:Some(path)}).await.is_ok() {
                                let _ = stream.read_u8().await;
                            }
                            let mut state = entry.lock().await;
                            state.leases -= 1;
                            state.idle_since = Some(Instant::now());
                        }
                        Err(_) => { let _ = write(&mut stream, &Reply {path:None}).await; }
                    }
                });
            }
        }
    }
    jobs.shutdown().await;
    for entry in pool.lock().await.values() {
        if let Some(mut master) = entry.lock().await.master.take() {
            let _ = master.kill().await;
        }
    }
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
    let path = reply.path.context(message("tool-native-ssh-denied"))?;
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
        Err(_) => {
            eprintln!("{}", message("tool-native-ssh-denied"));
            255
        }
    }
}

#[cfg(test)]
mod tests {
    use super::signer_loader_args;
    use super::{
        Entry, Pool, SecretStore, SocketPath, parse, record_at, retire_idle, serve_inner,
        valid_task,
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::{process::Command, sync::Mutex};

    #[tokio::test]
    async fn idle_expiry_retires_live_master_and_marks_dead_master_failed() {
        let dir = tempfile::tempdir().unwrap();
        let pooled = Pool::default();
        for (name, alive, leases) in [("idle", true, 0), ("dead", false, 0), ("leased", true, 1)] {
            let socket = dir.path().join(name);
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let mut master = if alive {
                Command::new("/usr/bin/sleep")
                    .arg("60")
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap()
            } else {
                Command::new("/usr/bin/false")
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap()
            };
            if !alive {
                assert!(!master.wait().await.unwrap().success());
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
    async fn input_eof_stops_task_master_and_unlinks_broker_socket() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::default();
        let master = Command::new("/usr/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = master.id().unwrap();
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
