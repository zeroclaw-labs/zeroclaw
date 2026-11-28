//! A tool-loop-owned private browser lease. No handle is model-selectable.
//!
//! The turn adapter creates the scope; calls outside it fail closed. The
//! supervisor is the sole owner of the child and its pipes. Dropping the
//! scope closes the input lease even if a command is in flight. The helper
//! must close its non-persistent context/browser on EOF before exiting.
#[cfg(not(feature = "tools-external"))]
pub async fn scope<T>(
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    future.await
}

#[cfg(feature = "tools-external")]
pub use enabled::scope;
#[cfg(all(feature = "tools-external", test))]
pub(crate) use enabled::scope_with_helper;
#[cfg(feature = "tools-external")]
pub(crate) use enabled::{active, finish, protecting, request};

#[cfg(feature = "tools-external")]
mod enabled {
    use anyhow::{Context, Result};
    use serde_json::{Value, json};
    use std::{
        future::Future,
        process::Stdio,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        process::{Child, ChildStdin, ChildStdout, Command},
        sync::{mpsc, oneshot},
    };

    tokio::task_local! {
        static TASK: Arc<Task>;
    }

    struct Request {
        payload: Value,
        reply: oneshot::Sender<Result<Value>>,
    }

    struct Task {
        requests: mpsc::Sender<Request>,
        private_reserved: AtomicBool,
    }

    struct EndLease(Option<oneshot::Sender<()>>);

    impl Drop for EndLease {
        fn drop(&mut self) {
            // The receiver also treats a dropped sender as termination.
            drop(self.0.take());
        }
    }

    /// A fresh capability for each tool-loop invocation, including nested agents.
    /// No session, caller string, alias or model argument can select another scope.
    pub async fn scope<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
        scope_with_helper(std::env::var_os("ZEROCLAW_MANAGED_BROWSER_HELPER"), future).await
    }

    pub(crate) async fn scope_with_helper<T>(
        helper: Option<std::ffi::OsString>,
        future: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let (requests, receiver) = mpsc::channel(8);
        if helper.is_none() {
            // Ordinary turns do not create an idle supervisor. Still mask
            // any outer owner so a nested invocation cannot inherit it.
            drop(receiver);
            return TASK
                .scope(
                    Arc::new(Task {
                        requests,
                        private_reserved: AtomicBool::new(false),
                    }),
                    future,
                )
                .await;
        }
        let (end, ended) = oneshot::channel();
        let lease = EndLease(Some(end));
        let worker = zeroclaw_spawn::spawn!(supervise(helper, receiver, ended));
        let result = TASK
            .scope(
                Arc::new(Task {
                    requests,
                    private_reserved: AtomicBool::new(false),
                }),
                future,
            )
            .await;
        drop(lease);
        // Normal completion/errors await cleanup. If this future is cancelled,
        // EndLease still signals the independently supervised cleanup owner.
        worker
            .await
            .context("Managed browser supervisor failed")??;
        result
    }

    pub(crate) fn active() -> bool {
        TASK.try_with(|_| ()).is_ok()
    }

    pub(crate) fn protecting() -> bool {
        TASK.try_with(|task| task.private_reserved.load(Ordering::Acquire))
            .unwrap_or(false)
    }

    pub(crate) async fn finish() -> Result<()> {
        request(json!({"op":"close"})).await?;
        Ok(())
    }

    /// Only the credential consumer calls this private typed-pipe bridge.
    pub(crate) async fn request(payload: Value) -> Result<Value> {
        let task = TASK
            .try_with(Arc::clone)
            .context("Managed browser task missing")?;
        let op = payload["op"]
            .as_str()
            .context("Managed browser operation missing")?;
        anyhow::ensure!(
            matches!(op, "open" | "origin" | "submit" | "close"),
            "Unsupported managed browser operation"
        );
        if payload["op"] == "open" {
            anyhow::ensure!(
                !task.private_reserved.swap(true, Ordering::AcqRel),
                "Managed browser already reserved"
            );
        } else if op != "close" {
            anyhow::ensure!(
                task.private_reserved.load(Ordering::Acquire),
                "Managed browser not opened"
            );
        }
        let (reply, result) = oneshot::channel();
        task.requests
            .send(Request { payload, reply })
            .await
            .map_err(|_| anyhow::anyhow!("Managed browser task ended"))?;
        result.await.context("Managed browser operation ended")?
    }

    struct Worker {
        child: Child,
        input: Option<ChildStdin>,
        output: BufReader<ChildStdout>,
    }

    impl Worker {
        fn start(helper: &std::ffi::OsStr) -> Result<Self> {
            anyhow::ensure!(
                std::path::Path::new(helper).is_absolute(),
                "Managed browser helper must be absolute"
            );
            let mut command = Command::new(helper);
            command.env_clear();
            // Keep only OS/browser startup inputs. In particular DEBUG, PWDEBUG,
            // NODE_OPTIONS and credential-provider environment never reach it.
            for key in [
                "PATH",
                "SystemRoot",
                "WINDIR",
                "TEMP",
                "TMP",
                "HOME",
                "USERPROFILE",
                "LOCALAPPDATA",
                "XDG_RUNTIME_DIR",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .context("Managed browser helper unavailable")?;
            let input = child
                .stdin
                .take()
                .context("Managed browser input missing")?;
            let output = child
                .stdout
                .take()
                .context("Managed browser output missing")?;
            Ok(Self {
                child,
                input: Some(input),
                output: BufReader::new(output),
            })
        }

        async fn exchange(&mut self, payload: Value) -> Result<Value> {
            let mut bytes = serde_json::to_vec(&payload)?;
            anyhow::ensure!(bytes.len() <= 65_536, "Managed browser request too large");
            bytes.push(b'\n');
            let input = self.input.as_mut().context("Managed browser lease ended")?;
            input.write_all(&bytes).await?;
            input.flush().await?;
            // Never accumulate arbitrary page output or diagnostics.
            let mut line = Vec::new();
            use tokio::io::AsyncReadExt;
            (&mut self.output)
                .take(1025)
                .read_until(b'\n', &mut line)
                .await?;
            anyhow::ensure!(
                line.len() <= 1024 && line.last() == Some(&b'\n'),
                "Invalid managed browser response"
            );
            let response: Value = serde_json::from_slice(&line)?;
            anyhow::ensure!(response["ok"] == true, "Managed browser refused operation");
            // Only a canonical HTTPS origin may accompany the bounded acknowledgement.
            let origin = response.get("origin").and_then(Value::as_str);
            if let Some(origin) = origin {
                let url = reqwest::Url::parse(origin)?;
                anyhow::ensure!(
                    url.scheme() == "https" && url.origin().ascii_serialization() == origin,
                    "Invalid managed browser origin"
                );
                Ok(json!({"ok": true, "origin": origin}))
            } else {
                Ok(json!({"ok": true}))
            }
        }

        async fn close(mut self) -> Result<()> {
            drop(self.input.take()); // EOF is the helper's authoritative cleanup signal.
            let status = tokio::time::timeout(Duration::from_secs(15), self.child.wait())
                .await
                .context("Managed browser cleanup timed out")??;
            anyhow::ensure!(status.success(), "Managed browser cleanup failed");
            Ok(())
        }
    }

    async fn supervise(
        helper: Option<std::ffi::OsString>,
        mut requests: mpsc::Receiver<Request>,
        mut ended: oneshot::Receiver<()>,
    ) -> Result<()> {
        let mut worker: Option<Worker> = None;
        loop {
            let next =
                tokio::select! { biased; _ = &mut ended => break, next = requests.recv() => next };
            let Some(mut next) = next else { break };
            if next.payload["op"] == "close" {
                let result = match worker.take() {
                    Some(worker) => worker.close().await,
                    None => Ok(()),
                };
                let failed = result.is_err();
                let _ = next.reply.send(result.map(|()| json!({"ok":true})));
                anyhow::ensure!(!failed, "Managed browser cleanup failed");
                return Ok(());
            }
            if worker.is_none() {
                let started = helper
                    .as_deref()
                    .context("Managed browser helper not configured")
                    .and_then(Worker::start);
                match started {
                    Ok(started) => worker = Some(started),
                    Err(error) => {
                        let _ = next.reply.send(Err(error));
                        break;
                    }
                }
            }
            let Some(current) = worker.as_mut() else {
                break;
            };
            let result = tokio::select! {
                biased;
                _ = &mut ended => break,
                _ = next.reply.closed() => break,
                result = tokio::time::timeout(Duration::from_secs(35), current.exchange(next.payload)) =>
                    result.context("Managed browser command timed out").and_then(|result| result),
            };
            let failed = result.is_err();
            let _ = next.reply.send(result);
            if failed {
                break;
            }
        }
        drop(requests);
        if let Some(worker) = worker {
            worker.close().await?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        fn helper(dir: &std::path::Path) -> std::path::PathBuf {
            let file = dir.join(if cfg!(windows) {
                "helper.cmd"
            } else {
                "helper"
            });
            let marker = dir.join("lifecycle");
            #[cfg(windows)]
            {
                let script = r#"import { appendFileSync } from 'node:fs';
import { createInterface } from 'node:readline';
const marker = __MARKER__;
appendFileSync(marker, `${process.pid} start\n`);
for await (const line of createInterface({ input: process.stdin })) {
  process.stdout.write('{"ok":true,"origin":"https://login.example.test","text":"synthetic-secret","cookies":"synthetic-session"}\n');
}
appendFileSync(marker, `${process.pid} close\n`);
"#.replace("__MARKER__", &serde_json::to_string(&marker).unwrap());
                std::fs::write(dir.join("fixture-helper.mjs"), script).unwrap();
                std::fs::write(&file, "@echo off\r\nnode \"%~dp0fixture-helper.mjs\"\r\n").unwrap();
            }
            #[cfg(unix)]
            {
                std::fs::write(&file, format!(r#"#!/bin/sh
printf '%s start\n' "$$" >> '{}'
while IFS= read -r line; do
  printf '%s\n' '{{"ok":true,"origin":"https://login.example.test","text":"synthetic-secret","cookies":"synthetic-session"}}'
done
printf '%s close\n' "$$" >> '{}'
"#, marker.display(), marker.display())).unwrap();
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            file
        }

        #[tokio::test]
        async fn nested_tasks_have_distinct_leases_and_do_not_export_helper_data() {
            let dir = tempfile::tempdir().unwrap();
            let file = helper(dir.path());
            scope_with_helper(Some(file.clone().into_os_string()), async {
                assert!(!protecting());
                let reply = request(json!({"op":"open"})).await?;
                assert_eq!(
                    reply,
                    json!({"ok":true,"origin":"https://login.example.test"})
                );
                assert!(protecting());
                scope_with_helper(Some(file.into_os_string()), async {
                    assert!(
                        !protecting(),
                        "child must not inherit parent's private context"
                    );
                    request(json!({"op":"open"})).await?;
                    assert!(protecting());
                    Ok(())
                })
                .await?;
                assert!(protecting());
                request(json!({"op":"origin"})).await?;
                Ok(())
            })
            .await
            .unwrap();
            let lines = std::fs::read_to_string(dir.path().join("lifecycle")).unwrap();
            let owners: Vec<_> = lines
                .lines()
                .filter(|line| line.ends_with("start"))
                .collect();
            assert_eq!(owners.len(), 2);
            assert_ne!(owners[0], owners[1]);
            assert_eq!(
                lines.lines().filter(|line| line.ends_with("close")).count(),
                2
            );
            assert!(!active());
        }

        #[tokio::test]
        async fn error_and_dropped_owner_close_the_input_lease() {
            for cancel in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let file = helper(dir.path());
                let (started, ready) = oneshot::channel();
                let task = zeroclaw_spawn::spawn!(scope_with_helper(
                    Some(file.into_os_string()),
                    async move {
                        request(json!({"op":"open"})).await?;
                        let _ = started.send(());
                        if cancel {
                            std::future::pending::<()>().await;
                        }
                        Err::<(), anyhow::Error>(anyhow::anyhow!("synthetic task error"))
                    }
                ));
                ready.await.unwrap();
                if cancel {
                    task.abort();
                }
                let _ = task.await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let lifecycle =
                            std::fs::read_to_string(dir.path().join("lifecycle")).unwrap();
                        if lifecycle.contains("close") {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
            }
        }
    }
}
