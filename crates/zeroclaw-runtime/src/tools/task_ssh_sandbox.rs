//! Preserve the sandbox boundary while the host-side SSH broker owns a turn.
use crate::security::Sandbox;
use std::{ffi::OsStr, process::Command, sync::Arc};

pub(super) struct TaskSshSandbox(pub(super) Arc<dyn Sandbox>);

impl TaskSshSandbox {
    fn check_task(&self) -> std::io::Result<()> {
        if self.0.name() != "none" && zeroclaw_tools::task_ssh::current().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                crate::i18n::get_required_cli_string("tool-shell-ssh-broker-sandbox-unsupported"),
            ));
        }
        Ok(())
    }
}

impl Sandbox for TaskSshSandbox {
    fn wrap_command(&self, command: &mut Command) -> std::io::Result<()> {
        self.check_task()?;
        self.0.wrap_command(command)
    }
    fn wrap_shell_command(
        &self,
        command: &mut Command,
        shell: Option<&OsStr>,
    ) -> std::io::Result<()> {
        self.check_task()?;
        self.0.wrap_shell_command(command, shell)
    }
    fn check_initialization(&self) -> std::io::Result<()> {
        self.0.check_initialization()
    }
    fn is_available(&self) -> bool {
        self.0.is_available()
    }
    fn name(&self) -> &str {
        self.0.name()
    }
    fn description(&self) -> &str {
        self.0.description()
    }
    fn coding_cli_unsupported_reason(&self) -> Option<&'static str> {
        self.0.coding_cli_unsupported_reason()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Witness {
        wraps: Arc<AtomicUsize>,
        backend: &'static str,
    }
    impl Sandbox for Witness {
        fn wrap_command(&self, _: &mut Command) -> std::io::Result<()> {
            self.wraps.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn is_available(&self) -> bool {
            true
        }
        fn name(&self) -> &str {
            self.backend
        }
        fn description(&self) -> &str {
            "synthetic sandbox"
        }
    }
    #[tokio::test]
    async fn ssh_task_blocks_all_sandboxed_subprocess_wrappers_before_launch() {
        let wraps = Arc::new(AtomicUsize::new(0));
        let sandbox = TaskSshSandbox(Arc::new(Witness {
            wraps: wraps.clone(),
            backend: "firejail",
        }));
        let mut command = Command::new("synthetic-not-executed");
        sandbox.wrap_command(&mut command).unwrap();
        zeroclaw_tools::task_ssh::scope_inherited(
            Some(zeroclaw_tools::task_ssh::SshTask {
                id: "synthetic-task".into(),
                state_dir: "synthetic-state".into(),
            }),
            async {
                assert_eq!(
                    sandbox.wrap_command(&mut command).unwrap_err().kind(),
                    std::io::ErrorKind::PermissionDenied
                );
                assert_eq!(
                    sandbox
                        .wrap_shell_command(&mut command, None)
                        .unwrap_err()
                        .kind(),
                    std::io::ErrorKind::PermissionDenied
                );
                assert_eq!(wraps.load(Ordering::SeqCst), 1);
                let unsandboxed = TaskSshSandbox(Arc::new(Witness {
                    wraps: wraps.clone(),
                    backend: "none",
                }));
                unsandboxed.wrap_command(&mut command).unwrap();
            },
        )
        .await;
        sandbox.wrap_command(&mut command).unwrap();
        assert_eq!(wraps.load(Ordering::SeqCst), 3);
    }
}
