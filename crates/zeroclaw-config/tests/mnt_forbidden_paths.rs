//! Regression coverage for default forbidden_paths covering WSL `/mnt` trees.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use zeroclaw_config::policy::SecurityPolicy;

#[test]
fn unix_defaults_forbid_mnt_media_and_run_media() {
    let p = SecurityPolicy {
        workspace_only: false,
        ..SecurityPolicy::default()
    };
    assert!(!p.is_path_allowed("/home/alice/.ssh/id_rsa"));
    assert!(!p.is_resolved_path_readable(Path::new("/home/alice/.ssh/id_rsa")));
    assert!(
        p.forbidden_path_argument("cat /home/alice/.ssh/id_rsa")
            .is_some()
    );

    assert!(!p.is_path_allowed("/mnt/c/Users/alice/.ssh/id_rsa"));
    assert!(!p.is_resolved_path_readable(Path::new("/mnt/c/Users/alice/.ssh/id_rsa")));
    assert!(
        p.forbidden_path_argument("cat /mnt/c/Users/alice/.ssh/id_rsa")
            .is_some()
    );
    assert!(!p.is_resolved_path_allowed(Path::new(
        "/mnt/c/Users/alice/AppData/Roaming/Microsoft/Windows/Start Menu/Programs/Startup/run.bat"
    )));
    assert!(!p.is_path_allowed("/media/usb/secret"));
    assert!(!p.is_path_allowed("/run/media/alice/disk/secret"));
}

#[test]
fn adding_mnt_keeps_a_windows_side_workspace_usable() {
    let mut p = SecurityPolicy {
        workspace_only: false,
        workspace_dir: PathBuf::from("/mnt/c/Users/alice/src/proj"),
        ..SecurityPolicy::default()
    };
    // Defaults already include /mnt; workspace depth beats the broad entry.
    assert!(p.is_path_allowed("/mnt/c/Users/alice/src/proj/main.rs"));
    assert!(!p.is_path_allowed("/mnt/c/Users/alice/.ssh/id_rsa"));
    assert!(
        p.forbidden_path_argument("cat /mnt/c/Users/alice/.ssh/id_rsa")
            .is_some()
    );
    // An operator allowlist under /mnt still wins by depth.
    p.allowed_roots.push(PathBuf::from("/mnt/share/inbox"));
    assert!(p.is_path_allowed("/mnt/share/inbox/report.csv"));
    assert!(!p.is_path_allowed("/mnt/share/dept/secrets.txt"));
}
