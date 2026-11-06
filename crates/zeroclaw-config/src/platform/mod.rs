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
            let shell = config.shell.clone().unwrap_or_else(native::default_shell);
            #[cfg(unix)]
            validate_shell(&shell)?;
            #[cfg(windows)]
            validate_shell_windows(&shell)?;
            // A shell that is absent from the host is a missing capability (e.g.
            // the shell-free distroless image), not a construction failure: the
            // runtime reports no shell, so LLM-only turns still run and
            // shell-dependent tools are not registered. Without an injected
            // PATH the shell keeps being resolved at command build time; only
            // its availability is checked here.
            #[cfg(unix)]
            let shell_path = {
                let configured_shell = if zeroclaw_api::platform::is_android() {
                    OsStr::new("/system/bin/sh")
                } else {
                    OsStr::new(&shell)
                };
                let ambient_path = std::env::var_os("PATH").unwrap_or_default();
                let search_path = path.unwrap_or(ambient_path.as_os_str());
                // Only an absent shell is a missing capability. A shell that
                // exists but is unusable (not executable, a directory) is a
                // misconfiguration and keeps its earlier behavior: an error
                // with an injected PATH, a failure at command build time
                // otherwise.
                if native::shell_is_absent(configured_shell, std::env::split_paths(search_path)) {
                    // The factory runs for every status read and cron call;
                    // announce the condition once per process.
                    static WARNED: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note,
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "shell": configured_shell.to_string_lossy(),
                            })),
                            "native runtime shell was not found; shell commands are unavailable"
                        );
                    }
                    return Ok(Box::new(NativeRuntime::without_shell(shell)));
                }
                path.map(|path| {
                    resolve_executable_with_path(configured_shell, std::env::split_paths(path))
                        .map_err(|error| {
                            anyhow::Error::new(error).context(format!(
                                "native runtime shell {configured_shell:?} could not be resolved in the injected PATH"
                            ))
                        })
                })
                .transpose()?
            };
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

    // ── Shell-free runtime ───────────────────────────────────
    //
    // Android pins its shell to `/system/bin/sh`: it never reports the shell
    // absent and `validate_shell` accepts any configured value, so these cases
    // do not apply there.

    #[cfg(all(unix, not(target_os = "android")))]
    fn assert_reports_no_shell(rt: &dyn RuntimeAdapter) {
        use crate::platform::ShellDialect;

        assert_eq!(rt.name(), "native");
        assert!(!rt.has_shell_access());
        assert_eq!(rt.shell_dialect(), ShellDialect::None);
        assert_eq!(rt.shell_profile(), None);
        assert_eq!(rt.shell_program(), None);
        let dir = std::env::temp_dir();
        let err = rt.build_shell_command("echo hi", &dir).unwrap_err();
        assert!(
            err.to_string().contains("no usable shell"),
            "build_shell_command should fail closed, got: {err}"
        );
        let err = rt
            .build_shell_command_with_effective_path(
                "echo hi",
                &dir,
                Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("no usable shell"),
            "an effective PATH must not bypass the missing shell, got: {err}"
        );
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn factory_native_without_shell_on_ambient_path_reports_no_shell() {
        // Only an absolute path here: it does not depend on the process PATH.
        // Bare names on the ambient PATH are covered with a controlled PATH in
        // `tests/native_shell_absent_ambient_path.rs`.
        let shell = "/nonexistent-zeroclaw-shell/bin/sh";
        let rt = create_runtime(&RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some(shell.into()),
            ..RuntimeConfig::default()
        })
        .unwrap_or_else(|e| panic!("shell {shell:?} must not fail construction: {e}"));
        assert_reports_no_shell(rt.as_ref());
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn factory_native_without_shell_in_injected_path_reports_no_shell() {
        let empty = tempfile::tempdir().unwrap();
        let rt = create_runtime_with_path(
            &RuntimeConfig {
                kind: RuntimeKind::Native,
                shell: Some("sh".into()),
                ..RuntimeConfig::default()
            },
            Some(empty.path().as_os_str()),
        )
        .expect("an injected PATH without sh must not fail construction");
        assert_reports_no_shell(rt.as_ref());
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn factory_native_unusable_shell_is_not_reported_as_missing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-executable");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some(file.to_string_lossy().into_owned()),
            ..RuntimeConfig::default()
        };

        // Ambient PATH: unchanged, the failure surfaces when a command is built.
        let rt = create_runtime(&cfg).unwrap();
        assert!(rt.has_shell_access());
        assert!(
            rt.build_shell_command("echo hi", &std::env::temp_dir())
                .is_err()
        );
        // Injected PATH: still a construction error, as before.
        assert!(create_runtime_with_path(&cfg, Some(dir.path().as_os_str())).is_err());
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn factory_native_absent_shell_with_unusable_path_entries_reports_no_shell() {
        // The resolver reports the error of an unusable entry (here a regular
        // file used as a directory) instead of NotFound when the shell is
        // nowhere to be found; that must still mean "absent", not "unusable".
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("file");
        std::fs::write(&not_a_dir, "").unwrap();
        let empty = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([not_a_dir.as_path(), empty.path()]).unwrap();

        let rt = create_runtime_with_path(
            &RuntimeConfig {
                kind: RuntimeKind::Native,
                shell: Some("sh".into()),
                ..RuntimeConfig::default()
            },
            Some(path.as_os_str()),
        )
        .expect("an absent shell must not fail construction");
        assert_reports_no_shell(rt.as_ref());
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn factory_native_unprobeable_path_entry_is_not_reported_as_missing_shell() {
        use std::os::unix::fs::PermissionsExt;

        // The only PATH entry that could hold the shell cannot be searched, so
        // its absence is not established: the earlier behavior stays (an error
        // with an injected PATH, a failure at command build time otherwise).
        let outer = tempfile::tempdir().unwrap();
        let locked = outer.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let shell = locked.join("sh");
        std::fs::write(&shell, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Restore on exit (also on panic) so the tempdir can be removed.
        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            }
        }
        let _restore = Restore(locked.clone());
        if std::fs::symlink_metadata(&shell).is_ok() {
            eprintln!(
                "skipped: directory permissions are not enforced for this process (running as root?)"
            );
            return;
        }

        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some("sh".into()),
            ..RuntimeConfig::default()
        };
        let err = create_runtime_with_path(&cfg, Some(locked.as_os_str()))
            .err()
            .expect("an unprobeable entry must not be reported as an absent shell");
        assert!(
            format!("{err:#}").contains("could not be resolved in the injected PATH"),
            "the resolver diagnostic must be kept, got: {err:#}"
        );

        // Absolute shell path below the same directory, ambient PATH:
        // construction succeeds with a shell that is still advertised, and the
        // resolver error surfaces when a command is built.
        let rt = create_runtime(&RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some(shell.to_string_lossy().into_owned()),
            ..RuntimeConfig::default()
        })
        .unwrap();
        assert!(rt.has_shell_access());
        let err = rt
            .build_shell_command("echo hi", &std::env::temp_dir())
            .unwrap_err();
        assert!(
            !err.to_string().contains("no usable shell"),
            "an unprobeable shell must report the resolver error, got: {err}"
        );
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn factory_native_broken_symlink_shell_is_present_but_unusable() {
        // A dangling symlink is a filesystem entry, so the shell is not
        // "absent": it is a misconfiguration and keeps its earlier behavior.
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("sh");
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &link).unwrap();

        // Absolute path, ambient PATH: construction succeeds and the failure
        // surfaces when a command is built.
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some(link.to_string_lossy().into_owned()),
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        assert!(rt.has_shell_access());
        let err = rt
            .build_shell_command("echo hi", &std::env::temp_dir())
            .unwrap_err();
        assert!(
            !err.to_string().contains("no usable shell"),
            "a present-but-broken shell must report the resolver error, got: {err}"
        );

        // Bare name found only as a dangling symlink in an injected PATH:
        // still a construction error, as before.
        let bare = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: Some("sh".into()),
            ..RuntimeConfig::default()
        };
        assert!(create_runtime_with_path(&bare, Some(dir.path().as_os_str())).is_err());
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn factory_native_still_rejects_malformed_shell() {
        for bad in ["", "./sh"] {
            assert!(
                create_runtime(&RuntimeConfig {
                    kind: RuntimeKind::Native,
                    shell: Some(bad.into()),
                    ..RuntimeConfig::default()
                })
                .is_err(),
                "shell {bad:?} is misconfigured, not missing, and must stay an error"
            );
        }
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
    fn factory_native_default_shell_uses_platform_resolver() {
        let cfg = RuntimeConfig {
            kind: RuntimeKind::Native,
            shell: None,
            ..RuntimeConfig::default()
        };
        let rt = create_runtime(&cfg).unwrap();
        let cmd = rt
            .build_shell_command("echo hi", &std::env::temp_dir())
            .unwrap();
        let selected = native::default_shell();
        let expected =
            crate::platform::resolve_executable(std::ffi::OsStr::new(&selected)).unwrap();
        assert_eq!(
            cmd.as_std().get_program(),
            expected.as_os_str(),
            "platform default {selected:?} should use the resolved executable path"
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
