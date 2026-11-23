//! Optional task-owned SSH helper lifecycle; never private-key material.
use anyhow::{Context, Result};
use std::future::Future;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

tokio::task_local! {
    static SSH_TASK: SshTask;
}

#[derive(Clone)]
pub struct SshTask {
    pub id: String,
    pub state_dir: String,
    #[cfg(feature = "portable-ssh")]
    pub portable: Option<PortableTaskOwner>,
}

#[cfg(feature = "portable-ssh")]
#[derive(Clone)]
pub struct PortableTaskOwner {
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pool: std::sync::Arc<tokio::sync::Mutex<crate::portable_ssh_auth::PortableSshTask>>,
}

#[cfg(feature = "portable-ssh")]
impl PortableTaskOwner {
    fn from_operator_config() -> Option<Self> {
        std::env::var_os("ZEROCLAW_SSH_KNOWN_HOSTS")?;
        std::env::var_os("ZEROCLAW_SSH_CREDENTIAL_DIR")?;
        Some(Self {
            active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            pool: Default::default(),
        })
    }

    pub async fn execute_for_alias(
        &self,
        alias: &str,
        host: &str,
        port: u16,
        username: Option<&str>,
        command: &str,
        timeout: std::time::Duration,
    ) -> Result<crate::portable_ssh_auth::PortableSshOutput> {
        let root =
            std::env::var_os("ZEROCLAW_SSH_CREDENTIAL_DIR").context("SSH catalog unavailable")?;
        let catalog = std::path::PathBuf::from(root).join("config.toml");
        let (record, _) =
            crate::scoped_credentials::load_scoped_credential_snapshot(Some(&catalog), alias)?;
        let mut destinations = record.ssh_profiles.into_iter().filter_map(|profile| {
            let dest = profile.destination;
            (matches!(
                profile.authentication,
                zeroclaw_config::ssh_auth_profile::SshAuthentication::Portable { .. }
            ) && dest.host.eq_ignore_ascii_case(host)
                && dest.port == port
                && username.is_none_or(|user| user == dest.username))
            .then_some(dest)
        });
        let destination = destinations
            .next()
            .context("SSH destination is not authorized")?;
        anyhow::ensure!(
            destinations.next().is_none(),
            "Specify the authorized SSH username for this destination"
        );
        self.execute(alias, destination, command, timeout).await
    }

    pub async fn execute(
        &self,
        alias: &str,
        destination: zeroclaw_config::schema::SshCredentialDestination,
        command: &str,
        timeout: std::time::Duration,
    ) -> Result<crate::portable_ssh_auth::PortableSshOutput> {
        let mut pool = self.pool.lock().await;
        anyhow::ensure!(
            self.active.load(std::sync::atomic::Ordering::Acquire),
            "SSH task ended"
        );
        let root =
            std::env::var_os("ZEROCLAW_SSH_CREDENTIAL_DIR").context("SSH catalog unavailable")?;
        let known_hosts =
            std::env::var_os("ZEROCLAW_SSH_KNOWN_HOSTS").context("SSH host pins unavailable")?;
        anyhow::ensure!(destination.is_valid(), "Invalid SSH destination");
        let pin =
            crate::portable_ssh_auth::host_pin(&destination, std::path::Path::new(&known_hosts))?;
        let catalog = std::path::PathBuf::from(root).join("config.toml");
        pool.execute(&catalog, alias, destination, pin, command, timeout)
            .await
    }
}

#[cfg(feature = "portable-ssh")]
struct PortableTaskGuard(Option<PortableTaskOwner>);

#[cfg(feature = "portable-ssh")]
impl Drop for PortableTaskGuard {
    fn drop(&mut self) {
        if let Some(owner) = &self.0 {
            owner
                .active
                .store(false, std::sync::atomic::Ordering::Release);
            if let Ok(mut pool) = owner.pool.try_lock() {
                *pool = Default::default();
            }
        }
    }
}

async fn run_owned<T>(task: SshTask, future: impl Future<Output = T>) -> T {
    #[cfg(feature = "portable-ssh")]
    let owner = PortableTaskGuard(task.portable.clone());
    let result = SSH_TASK.scope(task, future).await;
    #[cfg(feature = "portable-ssh")]
    if let Some(ref portable) = owner.0 {
        portable
            .active
            .store(false, std::sync::atomic::Ordering::Release);
        let pool = std::mem::take(&mut *portable.pool.lock().await);
        pool.close().await;
    }
    result
}

pub fn current() -> Option<SshTask> {
    SSH_TASK.try_with(Clone::clone).ok()
}

/// Restore an existing owner only for foreground work awaited by its parent.
/// This does not create a helper, transfer its lease or resolve credentials.
/// Detached/background jobs must establish their own task lifetime instead.
pub async fn scope_inherited<T>(task: Option<SshTask>, future: impl Future<Output = T>) -> T {
    match task {
        Some(task) => SSH_TASK.scope(task, future).await,
        None => future.await,
    }
}

struct Lease {
    input: Option<ChildStdin>,
    child: Child,
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Closing this pipe is the authoritative end signal, including when
        // the enclosing future is dropped. Do not kill the cleanup broker.
        drop(self.input.take());
    }
}

pub async fn scope<T>(
    future: std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
) -> Result<T> {
    scope_with_helper(std::env::var_os("ZEROCLAW_SSH_TASK_HELPER"), future).await
}

pub async fn scope_with_agent<T>(
    agent: Option<std::ffi::OsString>,
    future: std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
) -> Result<T> {
    scope_with_helper_and_agent(std::env::var_os("ZEROCLAW_SSH_TASK_HELPER"), agent, future).await
}

async fn scope_with_helper<T>(
    helper: Option<std::ffi::OsString>,
    future: std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
) -> Result<T> {
    scope_with_helper_and_agent(helper, None, future).await
}

async fn scope_with_helper_and_agent<T>(
    helper: Option<std::ffi::OsString>,
    agent: Option<std::ffi::OsString>,
    future: std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
) -> Result<T> {
    if current().is_some() {
        return future.await;
    }
    #[cfg(feature = "portable-ssh")]
    let portable = PortableTaskOwner::from_operator_config();
    let task = uuid::Uuid::new_v4().simple().to_string();
    let Some(helper) = helper else {
        #[cfg(feature = "portable-ssh")]
        if portable.is_some() {
            return run_owned(
                SshTask {
                    id: task,
                    state_dir: String::new(),
                    portable,
                },
                future,
            )
            .await;
        }
        return future.await;
    };
    let mut command = Command::new(helper);
    if let Some(agent) = agent {
        command.env("SSH_AUTH_SOCK", agent);
    }
    let mut child = command
        .arg("--serve")
        .arg(&task)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context(crate::i18n::get_required_tool_string(
            "tool-ssh-task-start-failed",
        ))?;
    let mut lease = Lease {
        input: child.stdin.take(),
        child,
    };
    let output = lease
        .child
        .stdout
        .take()
        .context(crate::i18n::get_required_tool_string(
            "tool-ssh-task-readiness-pipe",
        ))?;
    let mut reader = BufReader::new(output);
    let mut line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reader.read_line(&mut line),
    )
    .await
    .context(crate::i18n::get_required_tool_string(
        "tool-ssh-task-readiness-timeout",
    ))??;
    let ready: serde_json::Value = serde_json::from_str(&line).context(
        crate::i18n::get_required_tool_string("tool-ssh-task-readiness-invalid"),
    )?;
    anyhow::ensure!(
        ready["ready"].as_bool() == Some(true),
        crate::i18n::get_required_tool_string("tool-ssh-task-readiness-failed")
    );
    let state_dir = ready["state_dir"]
        .as_str()
        .context(crate::i18n::get_required_tool_string(
            "tool-ssh-task-state-directory",
        ))?
        .to_string();
    let result = run_owned(
        SshTask {
            id: task,
            state_dir,
            #[cfg(feature = "portable-ssh")]
            portable,
        },
        future,
    )
    .await;
    drop(lease.input.take());
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), lease.child.wait())
        .await
        .context(crate::i18n::get_required_tool_string(
            "tool-ssh-task-cleanup-timeout",
        ))?
        .context(crate::i18n::get_required_tool_string(
            "tool-ssh-task-cleanup-failed",
        ))?;
    anyhow::ensure!(
        status.success(),
        "{} ({status})",
        crate::i18n::get_required_tool_string("tool-ssh-task-cleanup-status")
    );
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn helper(directory: &std::path::Path) -> std::path::PathBuf {
        let file = directory.join("helper");
        let marker = directory.join("closed");
        std::fs::write(&file, format!("#!/bin/sh\nprintf '{{\"ready\":true,\"state_dir\":\"/tmp\"}}\\n'\ncat >/dev/null\nprintf closed > '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        file
    }

    #[tokio::test]
    async fn foreground_spawn_reuses_parent_owner_and_does_not_close_its_lease() {
        let directory = tempfile::tempdir().unwrap();
        let command = helper(directory.path());
        scope_with_helper_and_agent(
            Some(command.into_os_string()),
            Some("/synthetic/session-agent.sock".into()),
            Box::pin(async {
                let parent = current().unwrap();
                let mut workers = Vec::new();
                for _ in 0..2 {
                    let inherited = Some(parent.clone());
                    workers.push(zeroclaw_spawn::spawn!(async move {
                        assert!(current().is_none(), "Tokio does not inherit task locals");
                        scope_inherited(inherited, async { current().unwrap() }).await
                    }));
                }
                for worker in workers {
                    let child = worker.await.unwrap();
                    assert_eq!(child.id, parent.id);
                    assert_eq!(child.state_dir, parent.state_dir);
                }
                assert!(!directory.path().join("closed").exists());
                Ok(())
            }),
        )
        .await
        .unwrap();
        assert!(directory.path().join("closed").exists());
        assert!(current().is_none());
    }

    #[tokio::test]
    async fn session_agents_reach_the_broker_without_crossing_tasks() {
        let inherited = std::env::var_os("SSH_AUTH_SOCK");
        async fn run(agent: &'static str) {
            let directory = tempfile::tempdir().unwrap();
            let command = helper(directory.path());
            let marker = directory.path().join("agent");
            let source = std::fs::read_to_string(&command).unwrap();
            std::fs::write(
                &command,
                source.replace(
                    "cat >/dev/null",
                    &format!(
                        "printf '%s' \"$SSH_AUTH_SOCK\" > '{}'\ncat >/dev/null",
                        marker.display()
                    ),
                ),
            )
            .unwrap();
            scope_with_helper_and_agent(
                Some(command.into_os_string()),
                Some(agent.into()),
                Box::pin(async {
                    tokio::task::yield_now().await;
                    Ok(())
                }),
            )
            .await
            .unwrap();
            assert_eq!(std::fs::read_to_string(marker).unwrap(), agent);
        }
        tokio::join!(run("/session-a/agent.sock"), run("/session-b/agent.sock"));
        assert_eq!(std::env::var_os("SSH_AUTH_SOCK"), inherited);
    }

    #[tokio::test]
    async fn successful_and_failed_turns_close_the_helper() {
        for fail in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let command = helper(directory.path());
            let result = scope_with_helper(
                Some(command.into_os_string()),
                Box::pin(async move {
                    assert!(current().is_some());
                    if fail {
                        anyhow::bail!("operation failed");
                    }
                    Ok(())
                }),
            )
            .await;
            assert_eq!(result.is_err(), fail);
            assert!(directory.path().join("closed").exists());
            assert!(current().is_none());
        }
    }

    #[tokio::test]
    async fn cancelling_the_owner_closes_the_helper_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let command = helper(directory.path());
        let (started, ready) = tokio::sync::oneshot::channel();
        let owner = zeroclaw_spawn::spawn!(async move {
            scope_with_helper(
                Some(command.into_os_string()),
                Box::pin(async move {
                    started.send(()).unwrap();
                    std::future::pending::<Result<()>>().await
                }),
            )
            .await
        });
        ready.await.unwrap();
        owner.abort();
        let _ = owner.await;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !directory.path().join("closed").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
}
