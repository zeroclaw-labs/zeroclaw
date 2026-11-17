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

const SSH: &str = "/usr/bin/ssh";
const FOREGROUND: [&str; 2] = ["-o", "ForkAfterAuthentication=no"];
const FRAME_LIMIT: u64 = 16_384;

fn message(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
}

#[derive(Serialize, Deserialize)]
struct Request {
    argv: Vec<String>,
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

struct OwnedMaster {
    child: Child,
    group: i32,
}

impl OwnedMaster {
    fn spawn(command: &mut Command) -> Result<Self> {
        command.process_group(0);
        let child = command.spawn()?;
        let group = child.id().context("missing SSH master pid")? as i32;
        Ok(Self { child, group })
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    async fn kill(&mut self) -> std::io::Result<()> {
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

async fn open(path: &Path, options: &[String], host: &str) -> Result<OwnedMaster> {
    require_unused_socket(path)?;
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
    let mut master = OwnedMaster::spawn(command.args(options).args(["--", host]))?;
    timeout(Duration::from_secs(15), async {
        while !path.exists() {
            ensure!(master.try_wait()?.is_none(), "master exited");
            sleep(Duration::from_millis(25)).await;
        }
        Result::<()>::Ok(())
    })
    .await??;
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
    let fingerprint = serde_json::to_vec(&(host, effective))?;
    let key = format!("{:x}", Sha256::digest(fingerprint));
    let path = dir.join(&key[..24]);
    let entry = pool.lock().await.entry(path.clone()).or_default().clone();
    let mut state = entry.lock().await;
    ensure!(!state.failed, "failed connection");
    if state.master.is_none() {
        state.failed = true;
        state.master = Some(
            open(&path, options, host)
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
                                .find(|text| matches!(text.as_str(), "tool-native-ssh-connection"))
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

async fn client(args: Vec<String>) -> Result<i32> {
    let id = std::env::var("ZEROCLAW_SSH_TASK")?;
    ensure!(valid_task(&id), "invalid task");
    let (options, host, command) = parse(&args)?;
    let mut stream = UnixStream::connect(directory().join(id).join("broker")).await?;
    write(&mut stream, &Request { argv: args.clone() }).await?;
    let reply: Reply = timeout(Duration::from_secs(48), read(&mut stream)).await??;
    let error_key = reply
        .error
        .as_deref()
        .filter(|key| matches!(*key, "tool-native-ssh-connection"))
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
            let allowed = ["tool-native-ssh-connection", "tool-native-ssh-arguments"]
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
        Entry, OwnedMaster, Pool, SocketPath, parse, require_unused_socket, retire_idle,
        serve_inner, valid_task,
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::{process::Command, sync::Mutex};

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
    fn broker_socket_path_is_removed_on_scope_exit() {
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
