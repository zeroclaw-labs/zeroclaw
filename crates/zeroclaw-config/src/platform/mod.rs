pub mod docker;
pub mod executable;
pub mod native;

pub use docker::DockerRuntime;
pub use executable::{resolve_executable, resolve_executable_with_path};
pub use native::NativeRuntime;
pub use zeroclaw_api::runtime_traits::{RuntimeAdapter, ShellDialect, ShellProfile};

use crate::schema::{RuntimeConfig, RuntimeKind};
use std::ffi::OsStr;

pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    create_runtime_with_path(config, None)
}

/// Create a runtime, optionally resolving host launchers against an injected
/// PATH before the runtime's child workspace is applied.
pub fn create_runtime_with_path(
    config: &RuntimeConfig,
    path: Option<&OsStr>,
) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    #[cfg(not(unix))]
    let _ = path;

    match config.kind {
        RuntimeKind::Native => {
            let shell = config.shell.clone().unwrap_or_else(|| "sh".into());
            #[cfg(unix)]
            validate_shell(&shell)?;
            #[cfg(windows)]
            validate_shell_windows(&shell)?;
            #[cfg(unix)]
            let shell_path = path
                .map(|path| {
                    let configured_shell = if zeroclaw_api::platform::is_android() {
                        OsStr::new("/system/bin/sh")
                    } else {
                        OsStr::new(&shell)
                    };
                    resolve_executable_with_path(configured_shell, std::env::split_paths(path))
                        .map_err(|error| {
                            anyhow::Error::new(error).context(format!(
                                "native runtime shell {configured_shell:?} could not be resolved in the injected PATH"
                            ))
                        })
                })
                .transpose()?;
            #[cfg(not(unix))]
            let shell_path = None;
            Ok(Box::new(NativeRuntime::with_shell_and_resolved_path(
                shell, shell_path,
            )))
        }
        RuntimeKind::Docker => {
            #[cfg(unix)]
            let docker_path = path
                .map(|path| {
                    resolve_executable_with_path(OsStr::new("docker"), std::env::split_paths(path))
                        .map_err(|error| {
                            anyhow::Error::new(error).context(
                            "Docker runtime launcher could not be resolved in the injected PATH",
                        )
                        })
                })
                .transpose()?;
            #[cfg(not(unix))]
            let docker_path = None;
            Ok(Box::new(DockerRuntime::with_resolved_launcher(
                config.docker.clone(),
                docker_path,
            )))
        }
        RuntimeKind::Cloudflare => anyhow::bail!(
            "runtime.kind='cloudflare' is not implemented yet. Use runtime.kind='native' for now."
        ),
    }
}

#[cfg(unix)]
fn validate_shell(shell: &str) -> anyhow::Result<()> {
    // Android pins the shell to /system/bin/sh; the configured value is never
    // used, so don't reject it.
    if zeroclaw_api::platform::is_android() {
        return Ok(());
    }

    if shell.trim().is_empty() {
        anyhow::bail!("runtime.shell must not be empty or whitespace");
    }

    let path = std::path::Path::new(shell);
    if !path.is_absolute()
        && (!matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        ) || path.components().count() != 1
            || shell.contains('/')
            || shell.contains('\\'))
    {
        anyhow::bail!(
            "runtime.shell {shell:?} is a relative path; use a bare name resolved on PATH (e.g. \"bash\") or an absolute path (e.g. \"/bin/bash\")"
        );
    }

    Ok(())
}

/// Validate a configured `runtime.shell` on Windows.
///
/// Unlike the Unix check this does not resolve a binary on `PATH`: on Windows
/// `runtime.shell` selects the interpreter family (`cmd.exe` vs PowerShell),
/// and the interpreter is located at spawn time. The only fail-fast condition
/// worth catching up front is an empty/whitespace value, which would otherwise
/// spawn with no program.
#[cfg(windows)]
fn validate_shell_windows(shell: &str) -> anyhow::Result<()> {
    if shell.trim().is_empty() {
        anyhow::bail!("runtime.shell must not be empty or whitespace");
    }
    Ok(())
}

/// Write an executable shell shim into `dir` that records, on stdout, that it
/// ran (`SHIM_RAN`) and each argument it received (`arg:<value>`). Used by
/// tests to prove a configured shell is the binary that actually executes a
/// command and that it receives the `-c <command>` boundary.
#[cfg(all(test, unix))]
fn write_recording_shim(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let shim = dir.join("recording-shell");
    std::fs::write(
        &shim,
        "#!/bin/sh\necho SHIM_RAN\nfor a in \"$@\"; do echo \"arg:$a\"; done\n",
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    shim
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{RuntimeConfig, RuntimeKind};

    #[test]
    fn factory_native() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "native");
        assert!(rt.has_shell_access());
    }

    #[test]
    fn factory_docker() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Docker,
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert_eq!(rt.name(), "docker");
        assert!(rt.has_shell_access());
    }

    #[cfg(unix)]
    #[test]
    fn factory_resolves_native_and_docker_launchers_from_injected_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let native_shell = dir.path().join("native-shell");
        let docker = dir.path().join("docker");
        for launcher in [&native_shell, &docker] {
            std::fs::write(launcher, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let native = create_runtime_with_path(
            &RuntimeConfig {
                kind: RuntimeKind::Native,
                shell: Some("native-shell".into()),
                ..RuntimeConfig::default()
            },
            Some(dir.path().as_os_str()),
        )
        .unwrap();
        let native_command = native
            .build_shell_command("echo native", &std::env::temp_dir())
            .unwrap();
        assert_eq!(
            native_command.as_std().get_program(),
            native_shell.canonicalize().unwrap().as_os_str()
        );

        let docker_runtime = create_runtime_with_path(
            &RuntimeConfig {
                kind: RuntimeKind::Docker,
                ..RuntimeConfig::default()
            },
            Some(dir.path().as_os_str()),
        )
        .unwrap();
        let docker_command = docker_runtime
            .build_shell_command("echo docker", &std::env::temp_dir())
            .unwrap();
        assert_eq!(
            docker_command.as_std().get_program(),
            docker.canonicalize().unwrap().as_os_str()
        );
    }

    #[test]
    fn factory_cloudflare_errors() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Cloudflare,
            ..RuntimeConfig::default()
        };
        match create_runtime(&cfg) {
            Err(err) => assert!(err.to_string().contains("not implemented")),
            Ok(_) => panic!("cloudflare runtime should error"),
        }
    }

    #[test]
    fn unknown_runtime_kind_loads_as_native() {
        let parsed: RuntimeConfig = toml::from_str("kind = \"wasm-edge-unknown\"").unwrap();
        assert_eq!(parsed.kind, RuntimeKind::Native);
        let empty: RuntimeConfig = toml::from_str("kind = \"\"").unwrap();
        assert_eq!(empty.kind, RuntimeKind::Native);
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn factory_native_default_shell_is_sh() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: None,
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        let cmd = rt
            .build_shell_command("echo hi", &std::env::temp_dir())
            .unwrap();
        let expected = crate::platform::resolve_executable(std::ffi::OsStr::new("sh")).unwrap();
        assert_eq!(
            cmd.as_std().get_program(),
            expected.as_os_str(),
            "default shell should use the resolved executable path"
        );
    }

    // ── Shell validation ─────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn validate_shell_rejects_empty_or_whitespace() {
        for bad in ["", "   ", "\t", " \n "] {
            assert!(
                validate_shell(bad).is_err(),
                "shell {bad:?} should be rejected"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_build_rejects_nonexistent_absolute_path() {
        let err = NativeRuntime::with_shell("/no/such/shell/binary".into())
            .build_shell_command("echo test", &std::env::temp_dir())
            .unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "error should name the missing path, got: {err}"
        );
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn native_build_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = NativeRuntime::with_shell(dir.path().to_string_lossy().into_owned())
            .build_shell_command("echo test", &std::env::temp_dir())
            .unwrap_err();
        assert!(
            err.to_string().contains("not a regular file"),
            "error should identify the non-file shell target, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_shell_rejects_relative_path() {
        // Relative path-style values are rejected purely by shape (before any
        // filesystem access): they would validate from the process cwd but
        // execute from the workspace dir, so the validated and executed
        // binaries could differ. Bare names and absolute paths are unaffected.
        for rel in ["./sh", "bin/sh", "../sh", "tools/bin/sh"] {
            let err = validate_shell(rel).unwrap_err();
            assert!(
                err.to_string().contains("relative path"),
                "relative shell {rel:?} should be rejected, got: {err}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_build_rejects_bare_name_not_on_path() {
        let err = NativeRuntime::with_shell("zc-no-such-shell-on-path".into())
            .build_shell_command("echo test", &std::env::temp_dir())
            .unwrap_err();
        assert!(
            err.to_string().contains("not found on PATH"),
            "error should mention PATH, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_build_rejects_nonexecutable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-executable");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = NativeRuntime::with_shell(file.to_string_lossy().into_owned())
            .build_shell_command("echo test", &std::env::temp_dir())
            .unwrap_err();
        assert!(
            err.to_string().contains("not executable"),
            "error should mention executability, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_shell_accepts_sh() {
        validate_shell("sh").expect("'sh' must resolve on PATH");
    }

    // ── End-to-end: the configured shell actually runs the command ──

    #[cfg(unix)]
    #[tokio::test]
    async fn factory_executes_command_under_configured_shell() {
        let dir = tempfile::tempdir().unwrap();
        let shim = write_recording_shim(dir.path());

        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some(shim.to_string_lossy().into_owned()),
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        let output = rt
            .build_shell_command("echo factory-shim", dir.path())
            .unwrap()
            .output()
            .await
            .unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("SHIM_RAN"),
            "configured shim should run, got: {stdout:?}"
        );
        assert!(
            stdout.contains("arg:-c"),
            "shim should receive -c, got: {stdout:?}"
        );
        assert!(
            stdout.contains("arg:echo factory-shim"),
            "shim should receive the command, got: {stdout:?}"
        );
    }
}
