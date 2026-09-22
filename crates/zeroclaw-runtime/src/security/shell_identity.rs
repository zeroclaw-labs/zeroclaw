//! Preserve a native shell's argv[0] through a replacing host sandbox.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
#[cfg(any(test, not(target_os = "macos")))]
use std::path::PathBuf;
use std::process::Command;
#[cfg(any(test, not(target_os = "macos")))]
use std::process::Stdio;
#[cfg(any(test, not(target_os = "macos")))]
use std::time::{Duration, Instant};

pub(super) fn invocation(cmd: &Command, identity: Option<&OsStr>) -> io::Result<Vec<OsString>> {
    #[cfg(target_os = "macos")]
    {
        macos_invocation(cmd, identity)
    }
    #[cfg(not(target_os = "macos"))]
    {
        invocation_with(cmd, identity, resolve_helper)
    }
}

#[cfg(target_os = "macos")]
fn macos_invocation(cmd: &Command, identity: Option<&OsStr>) -> io::Result<Vec<OsString>> {
    let mut argv = Vec::new();
    if let Some(identity) = identity.filter(|identity| *identity != cmd.get_program()) {
        validate_operand(Path::new(cmd.get_program()))?;
        // macOS ships Bash but not GNU env --argv0. -p ignores inherited
        // startup hooks; all variable data remains positional, never source.
        let bash = zeroclaw_config::platform::resolve_executable(OsStr::new("/bin/bash"))?;
        argv.extend([
            bash.into_os_string(),
            OsString::from("-p"),
            OsString::from("-c"),
            OsString::from("builtin exec -a \"$1\" \"$2\" \"${@:3}\""),
            OsString::from("--"),
            identity.to_owned(),
        ]);
    }
    argv.push(cmd.get_program().to_owned());
    argv.extend(cmd.get_args().map(OsStr::to_owned));
    Ok(argv)
}

#[cfg(any(test, not(target_os = "macos")))]
pub(super) fn invocation_with(
    cmd: &Command,
    identity: Option<&OsStr>,
    resolve: impl FnOnce() -> io::Result<PathBuf>,
) -> io::Result<Vec<OsString>> {
    let mut argv = Vec::new();
    if let Some(identity) = identity.filter(|identity| *identity != cmd.get_program()) {
        validate_operand(Path::new(cmd.get_program()))?;
        let helper = resolve()?;
        validate_operand(&helper)?;
        argv.push(helper.into_os_string());
        let mut arg0 = OsString::from("--argv0=");
        arg0.push(identity);
        argv.push(arg0);
        argv.push(OsString::from("--"));
    }
    argv.push(cmd.get_program().to_owned());
    argv.extend(cmd.get_args().map(OsStr::to_owned));
    Ok(argv)
}

fn validate_operand(path: &Path) -> io::Result<()> {
    // env parses '=' operands as assignments even after '--'. Never let an
    // executable path become an assignment and move the next argument into
    // the executable position.
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&b'=') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox shell identity requires an absolute executable path without '='",
        ));
    }
    Ok(())
}

#[cfg(any(test, not(target_os = "macos")))]
fn resolve_helper() -> io::Result<PathBuf> {
    let mut last_error = None;
    for name in ["env", "genv"] {
        let result =
            zeroclaw_config::platform::resolve_executable(OsStr::new(name)).and_then(|helper| {
                probe_helper(&helper, Duration::from_secs(2))?;
                Ok(helper)
            });
        match result {
            Ok(helper) => return Ok(helper),
            Err(error) => last_error = Some(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "sandbox shell identity requires a GNU-compatible env or genv with --argv0 on the host PATH and accessible inside the sandbox: {}",
            last_error.map_or_else(|| "no helper found".to_owned(), |error| error.to_string())
        ),
    ))
}

#[cfg(any(test, not(target_os = "macos")))]
fn probe_helper(helper: &Path, timeout: Duration) -> io::Result<()> {
    validate_operand(helper)?;
    // Probe the exact helper without executing any user payload. Old env
    // implementations reject --argv0; a supporting one executes itself.
    let mut child = Command::new(helper)
        .arg("--argv0=env")
        .arg("--")
        .arg(helper)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "env helper does not support --argv0",
                ));
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(result.err().unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "env --argv0 probe timed out")
                }));
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn shell_identity_invocation_preserves_program_and_argument_bytes() {
        let mut cmd = Command::new("/usr/bin/busybox");
        let argument = OsString::from_vec(b"quotes ' \" ; $() \xff".to_vec());
        cmd.arg("-c").arg(&argument);
        let argv = invocation_with(&cmd, Some(OsStr::new("sh")), || {
            Ok(PathBuf::from("/usr/bin/env"))
        })
        .unwrap();
        assert_eq!(argv[0], "/usr/bin/env");
        assert_eq!(argv[1], "--argv0=sh");
        assert_eq!(argv[2], "--");
        assert_eq!(argv[3], cmd.get_program());
        assert_eq!(argv[4], "-c");
        assert_eq!(argv[5], argument);
    }

    #[test]
    fn shell_identity_invalid_target_never_resolves_a_helper() {
        for target in ["sh", "/opt/shells=v1/sh"] {
            let cmd = Command::new(target);
            let error = invocation_with(&cmd, Some(OsStr::new("alias")), || {
                panic!("invalid target must fail before helper lookup")
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn shell_identity_missing_helper_fails_without_mutating_payload() {
        let mut cmd = Command::new("/usr/bin/busybox");
        cmd.args(["-c", "payload"]);
        let error = invocation_with(&cmd, Some(OsStr::new("sh")), || {
            Err(io::Error::new(io::ErrorKind::NotFound, "missing helper"))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(cmd.get_program(), "/usr/bin/busybox");
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), ["-c", "payload"]);
    }

    #[test]
    fn shell_identity_no_override_needs_no_helper() {
        let cmd = Command::new("/bin/sh");
        for identity in [None, Some(OsStr::new("/bin/sh"))] {
            assert_eq!(
                invocation_with(&cmd, identity, || panic!("no helper required")).unwrap(),
                [OsString::from("/bin/sh")]
            );
        }
    }

    #[test]
    fn shell_identity_rejects_ambiguous_helper_paths() {
        let cmd = Command::new("/usr/bin/busybox");
        for helper in ["env", "/opt/coreutils=v1/env"] {
            let error = invocation_with(&cmd, Some(OsStr::new("sh")), || Ok(PathBuf::from(helper)))
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn shell_identity_probe_rejects_unsupported_and_stalled_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("env");
        for (script, timeout, expected) in [
            (
                "#!/bin/sh\nexit 1\n",
                Duration::from_secs(2),
                io::ErrorKind::Unsupported,
            ),
            (
                "#!/bin/sh\nwhile :; do :; done\n",
                Duration::from_millis(100),
                io::ErrorKind::TimedOut,
            ),
        ] {
            std::fs::write(&helper, script).unwrap();
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
            let error = probe_helper(&helper, timeout).unwrap_err();
            assert_eq!(error.kind(), expected);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_identity_trampoline_ignores_inherited_bash_startup_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let startup = dir.path().join("startup.sh");
        std::fs::write(&startup, "exit 73\n").unwrap();
        let mut cmd = Command::new("/usr/bin/printf");
        cmd.args(["%s", "safe"]);
        let argv = invocation(&cmd, Some(OsStr::new("printf-alias"))).unwrap();
        assert_eq!(argv[1], "-p");
        assert_eq!(argv[2], "-c");
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .env("BASH_ENV", &startup)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"safe");
    }

    #[test]
    #[ignore = "requires GNU-compatible env/genv; run explicitly on a provisioned host"]
    fn shell_identity_real_env_executes_requested_alias() {
        let helper = resolve_helper().expect("GNU-compatible env/genv required");
        let shell = std::fs::canonicalize("/bin/sh").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let alias = dir.path().join("sh");
        std::os::unix::fs::symlink(&shell, &alias).unwrap();
        for identity in [OsStr::new("sh"), alias.as_os_str()] {
            let mut cmd = Command::new(&shell);
            cmd.args(["-c", "printf '%s' \"$0\""]);
            let argv = invocation_with(&cmd, Some(identity), || Ok(helper.clone())).unwrap();
            let output = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, identity.as_encoded_bytes());
        }
    }
}
