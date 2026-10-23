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
}

pub fn current() -> Option<SshTask> {
    SSH_TASK.try_with(Clone::clone).ok()
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

async fn scope_with_helper<T>(
    helper: Option<std::ffi::OsString>,
    future: std::pin::Pin<Box<dyn Future<Output = Result<T>> + Send + '_>>,
) -> Result<T> {
    let Some(helper) = helper else {
        return future.await;
    };
    if current().is_some() {
        return future.await;
    }
    let task = uuid::Uuid::new_v4().simple().to_string();
    let mut child = Command::new(helper)
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
    let result = SSH_TASK
        .scope(
            SshTask {
                id: task,
                state_dir,
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
