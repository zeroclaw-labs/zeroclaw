pub mod docker;
pub mod native;

pub use docker::DockerRuntime;
pub use native::NativeRuntime;
pub use zeroclaw_api::runtime_traits::RuntimeAdapter;

use crate::schema::{RuntimeConfig, RuntimeKind};

pub fn create_runtime(config: &RuntimeConfig) -> anyhow::Result<Box<dyn RuntimeAdapter>> {
    match config.kind {
        RuntimeKind::Native => {
            let shell = config.shell.clone().unwrap_or_else(|| "sh".into());
            #[cfg(unix)]
            validate_shell(&shell)?;
            Ok(Box::new(NativeRuntime::with_shell(shell)))
        }
        RuntimeKind::Docker => Ok(Box::new(DockerRuntime::new(config.docker.clone()))),
        RuntimeKind::Cloudflare => anyhow::bail!(
            "runtime.kind='cloudflare' is not implemented yet. Use runtime.kind='native' for now."
        ),
    }
}

/// Validate a configured native shell before it is installed as the runtime
/// shell, so a bad value fails fast at startup with an actionable message
/// instead of breaking every `tool:shell` invocation later.
///
/// Bare names (e.g. `"sh"`, `"bash"`) are resolved against `PATH`; absolute
/// paths (e.g. `"/bin/zsh"`) are checked directly. The resolved binary must
/// exist and be executable.
///
/// Relative paths with separators (e.g. `"./myshell"`, `"bin/sh"`) are
/// rejected: validation runs from the process working directory, but the
/// runtime executes commands with `current_dir` set to the workspace, so a
/// relative value could validate against one directory and execute against
/// another (or resolve to a different workspace-local binary). Requiring a
/// bare PATH name or an absolute path keeps selection workspace-independent.
///
/// Unix-only: Windows ignores `runtime.shell` (always `cmd.exe`), so the call
/// is `#[cfg(unix)]`-gated; Android (always `/system/bin/sh`) is skipped below.
#[cfg(unix)]
fn validate_shell(shell: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Android pins the shell to /system/bin/sh; the configured value is never
    // used, so don't reject it.
    if zeroclaw_api::platform::is_android() {
        return Ok(());
    }

    if shell.trim().is_empty() {
        anyhow::bail!("runtime.shell must not be empty or whitespace");
    }

    let path = std::path::Path::new(shell);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else if path.components().count() > 1 {
        anyhow::bail!(
            "runtime.shell {shell:?} is a relative path; use a bare name resolved on PATH (e.g. \"bash\") or an absolute path (e.g. \"/bin/bash\")"
        );
    } else {
        match std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(shell))
            .find(|candidate| candidate.is_file())
        {
            Some(found) => found,
            None => anyhow::bail!(
                "runtime.shell {shell:?} was not found on PATH; use an absolute path or install the shell"
            ),
        }
    };

    if !resolved.exists() {
        anyhow::bail!(
            "runtime.shell {shell:?} (resolved to {}) does not exist",
            resolved.display()
        );
    }

    // Coarse check: reject only when no execute bit is set at all. A precise
    // "can *we* execute it" test (uid/gid vs. the file owner) buys little —
    // the kernel's spawn is the real authority (ACLs, caps, mount flags) — and
    // this is a fail-fast sanity check, not a security gate.
    let mode = match resolved.metadata() {
        Ok(meta) => meta.permissions().mode(),
        Err(e) => anyhow::bail!(
            "runtime.shell {shell:?} (resolved to {}) could not be inspected: {e}",
            resolved.display()
        ),
    };
    if mode & 0o111 == 0 {
        anyhow::bail!(
            "runtime.shell {shell:?} (resolved to {}) is not executable",
            resolved.display()
        );
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
        let debug = format!("{cmd:?}");
        #[cfg(not(target_os = "windows"))]
        assert!(
            debug.contains("\"sh\""),
            "default shell should be 'sh', got: {debug}"
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
    fn validate_shell_rejects_nonexistent_absolute_path() {
        let err = validate_shell("/no/such/shell/binary").unwrap_err();
        assert!(
            err.to_string().contains("does not exist"),
            "error should name the missing path, got: {err}"
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
    fn validate_shell_rejects_bare_name_not_on_path() {
        let err = validate_shell("zc-no-such-shell-on-path").unwrap_err();
        assert!(
            err.to_string().contains("not found on PATH"),
            "error should mention PATH, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_shell_rejects_nonexecutable_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-executable");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = validate_shell(file.to_str().unwrap()).unwrap_err();
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

    /// Wire a recording shim through the config factory and prove the command
    /// executes under *that* shell with the expected `<shell> -c <command>`
    /// boundary — not merely that the shell name appears in a debug string.
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
