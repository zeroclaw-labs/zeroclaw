//! Linux process-boundary proof for `zeroclaw desktop`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const DOWNLOAD_URL: &str = "https://github.com/zeroclaw-labs/zeroclaw/releases/latest";

fn make_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn desktop_command(config: &Path, home: &Path, xdg_data: &Path, path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zeroclaw"));
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_DATA_HOME", xdg_data)
        .env("XDG_DATA_DIRS", xdg_data.join("system"))
        .env("PATH", path)
        .env("LANG", "C.UTF-8")
        .arg("--config-dir")
        .arg(config)
        .arg("desktop");
    command
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !path.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(path.is_file(), "timed out waiting for {}", path.display());
}

#[test]
fn desktop_launch_and_install_reach_the_linux_process_boundary() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let config = root.path().join("config");
    let xdg_data = root.path().join("xdg");
    let applications = xdg_data.join("applications");
    let path_dir = root.path().join("bin");
    for dir in [&home, &config, &applications, &path_dir] {
        std::fs::create_dir_all(dir).unwrap();
    }

    let appimage = root.path().join("ZeroClaw-smoke.AppImage");
    let launch_sentinel = PathBuf::from(format!("{}.launched", appimage.display()));
    make_executable(&appimage, "#!/bin/sh\n: > \"$0.launched\"\n");
    std::fs::write(
        applications.join("ZeroClaw.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=ZeroClaw\nExec={}\n",
            appimage.display()
        ),
    )
    .unwrap();

    let output = desktop_command(&config, &home, &xdg_data, &path_dir)
        .output()
        .expect("failed to run `zeroclaw desktop`");
    assert_success(&output, "desktop launch");
    wait_for_file(&launch_sentinel);

    let xdg_open = path_dir.join("xdg-open");
    let url_sentinel = PathBuf::from(format!("{}.url", xdg_open.display()));
    make_executable(&xdg_open, "#!/bin/sh\nprintf '%s' \"$1\" > \"$0.url\"\n");
    let output = desktop_command(&config, &home, &xdg_data, &path_dir)
        .arg("--install")
        .output()
        .expect("failed to run `zeroclaw desktop --install`");
    assert_success(&output, "desktop install-page open");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Opening the ZeroClaw companion app download page"),
        "desktop --install output must say that it opens the page:\n{stdout}"
    );
    wait_for_file(&url_sentinel);
    assert_eq!(
        std::fs::read_to_string(&url_sentinel).unwrap(),
        DOWNLOAD_URL
    );

    let output = desktop_command(&config, &home, &xdg_data, &path_dir)
        .arg("--help")
        .output()
        .expect("failed to run `zeroclaw desktop --help`");
    assert_success(&output, "desktop help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("open the download page")
            && stdout.contains("does not install anything itself"),
        "desktop help must describe --install truthfully:\n{stdout}"
    );
}
