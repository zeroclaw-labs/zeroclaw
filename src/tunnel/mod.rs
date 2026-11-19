#[allow(unused_imports)]
pub use zeroclaw_runtime::tunnel::*;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;

    #[tokio::test]
    async fn kill_shared_terminates_and_clears_child() {
        let proc = new_shared_process();

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.args(["30"]);
            c
        };

        let child = cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sleep should spawn for lifecycle test");

        {
            let mut guard = proc.lock().await;
            *guard = Some(TunnelProcess {
                child,
                public_url: "https://example.test".into(),
            });
        }

        kill_shared(&proc).await.unwrap();

        let guard = proc.lock().await;
        assert!(guard.is_none());
    }
}
