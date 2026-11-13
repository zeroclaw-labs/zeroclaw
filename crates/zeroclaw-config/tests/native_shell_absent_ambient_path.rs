//! A native runtime built from the process `PATH` (no injected PATH) reports
//! no shell when the shell is absent, even if some `PATH` entries are unusable
//! and would make the executable resolver report an error other than
//! `NotFound`.
//!
//! This is the only test in this binary because it replaces the process-wide
//! `PATH`. Android pins its shell to `/system/bin/sh` and never reports it
//! absent, so the test does not apply there.

#![cfg(all(unix, not(target_os = "android")))]

use std::ffi::OsString;
use zeroclaw_config::platform::{RuntimeAdapter, ShellDialect, create_runtime};
use zeroclaw_config::schema::{RuntimeConfig, RuntimeKind};

struct PathGuard(Option<OsString>);

impl PathGuard {
    fn set(value: &std::ffi::OsStr) -> Self {
        let previous = std::env::var_os("PATH");
        // SAFETY: this is the only test in this binary and it starts no other
        // threads, so nothing reads the environment concurrently.
        unsafe { std::env::set_var("PATH", value) };
        Self(previous)
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`; the test has finished using the runtime.
        unsafe {
            match &self.0 {
                Some(previous) => std::env::set_var("PATH", previous),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

fn runtime_for(shell: &str) -> Box<dyn RuntimeAdapter> {
    create_runtime(&RuntimeConfig {
        kind: RuntimeKind::Native,
        shell: Some(shell.into()),
        ..RuntimeConfig::default()
    })
    .expect("an absent shell must not fail construction")
}

#[test]
fn absent_shell_on_ambient_path_with_unusable_entries_reports_no_shell() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_dir = dir.path().join("file");
    std::fs::write(&not_a_dir, "").unwrap();
    let empty = tempfile::tempdir().unwrap();
    let path = std::env::join_paths([
        OsString::from("relative-decoy"),
        OsString::new(),
        not_a_dir.into_os_string(),
        empty.path().as_os_str().to_os_string(),
    ])
    .unwrap();
    let _guard = PathGuard::set(&path);

    let rt = runtime_for("sh");
    assert!(!rt.has_shell_access());
    assert_eq!(rt.shell_dialect(), ShellDialect::None);
    assert_eq!(rt.shell_profile(), None);
    let err = rt
        .build_shell_command("echo hi", &std::env::temp_dir())
        .unwrap_err();
    assert!(err.to_string().contains("no usable shell"), "{err}");

    // Positive control: the same PATH plus a directory holding an executable
    // `sh` yields a runtime with a shell.
    let bin = tempfile::tempdir().unwrap();
    let shell = bin.path().join("sh");
    std::fs::write(&shell, "#!/bin/sh\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let with_shell = std::env::join_paths([
        std::path::PathBuf::from(&dir.path().join("file")),
        bin.path().to_path_buf(),
    ])
    .unwrap();
    let _guard = PathGuard::set(&with_shell);
    assert!(runtime_for("sh").has_shell_access());
}
