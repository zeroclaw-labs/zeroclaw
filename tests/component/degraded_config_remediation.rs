use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn path_with_fixture_first(dir: &Path) -> OsString {
    let mut paths = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).expect("test PATH must be joinable")
}

#[cfg(unix)]
fn write_fake_zeroclaw(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("zeroclaw");
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf '%s\\n' '{\"error\":null,\"migrated\":false,\"schema_version\":3,\"valid\":true}'\n",
    )
    .expect("write fake zeroclaw");
    let mut permissions = std::fs::metadata(&path)
        .expect("stat fake zeroclaw")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("make fake zeroclaw executable");
    path
}

#[cfg(windows)]
fn write_fake_zeroclaw(dir: &Path) -> PathBuf {
    let path = dir.join("zeroclaw.cmd");
    std::fs::write(
        &path,
        "@echo off\r\necho {\"error\":null,\"migrated\":false,\"schema_version\":3,\"valid\":true}\r\n",
    )
    .expect("write fake zeroclaw.cmd");
    path
}

#[cfg(unix)]
fn run_path_zeroclaw(path: &OsString, config_dir: &Path) -> Output {
    Command::new("sh")
        .args(["-c", "zeroclaw config migrate --json"])
        .env("PATH", path)
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .output()
        .expect("run PATH zeroclaw fixture through sh")
}

#[cfg(windows)]
fn run_path_zeroclaw(path: &OsString, config_dir: &Path) -> Output {
    Command::new("cmd.exe")
        .args(["/D", "/S", "/C", "zeroclaw config migrate --json"])
        .env("PATH", path)
        .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
        .env("ZEROCLAW_CONFIG_DIR", config_dir)
        .output()
        .expect("run PATH zeroclaw fixture through cmd.exe")
}

#[test]
fn degraded_config_guidance_is_bound_to_running_executable_when_path_disagrees() {
    let config_dir = tempfile::tempdir().expect("temp config dir");
    std::fs::write(
        config_dir.path().join("config.toml"),
        r#"schema_version = 3

[gateway]
require_pairing = false

[risk_profiles.example]
level = "autonomous"
"#,
    )
    .expect("write degraded config");

    let fake_bin_dir = tempfile::tempdir().expect("temp fake-bin dir");
    let fake_zeroclaw = write_fake_zeroclaw(fake_bin_dir.path());
    assert!(fake_zeroclaw.exists(), "fake PATH zeroclaw must exist");

    let test_path = path_with_fixture_first(fake_bin_dir.path());
    let path_result = run_path_zeroclaw(&test_path, config_dir.path());
    let path_stdout = String::from_utf8_lossy(&path_result.stdout);
    let path_stderr = String::from_utf8_lossy(&path_result.stderr);
    assert!(
        path_result.status.success() && path_stdout.contains("\"valid\":true"),
        "PATH zeroclaw must accept the fixture config\nstdout:\n{path_stdout}\nstderr:\n{path_stderr}"
    );

    let daemon_bin = Path::new(env!("CARGO_BIN_EXE_zeroclaw"));
    let output = Command::new(daemon_bin)
        .arg("--config-dir")
        .arg(config_dir.path())
        .arg("daemon")
        .env("PATH", &test_path)
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .output()
        .expect("run real zeroclaw daemon");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "daemon must reject the security-critical config without --allow-degraded-security\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("risk_profiles"),
        "daemon rejection must identify the degraded security section: {stderr}"
    );

    let daemon_path = daemon_bin.display().to_string();
    assert!(
        stderr.contains(&daemon_path) && stderr.contains("config migrate"),
        "remediation must bind config migration to the running executable {daemon_path}: {stderr}"
    );
    assert!(
        !stderr.contains("Run `zeroclaw config migrate`")
            && !stderr.contains("run `zeroclaw config migrate`"),
        "daemon startup must not direct the operator through PATH: {stderr}"
    );
}
