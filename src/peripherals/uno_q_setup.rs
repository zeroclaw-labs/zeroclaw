//! Deploy ZeroClaw Bridge app to Arduino Uno Q.

use anyhow::{Context, Result};
use std::process::Command;

const BRIDGE_APP_NAME: &str = "uno-q-bridge";

/// Deploy the Bridge app. If host is Some, scp from repo and ssh to start.
/// If host is None, assume we're ON the Uno Q — use embedded files and start.
pub fn setup_uno_q_bridge(host: Option<&str>) -> Result<()> {
    let bridge_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("firmware")
        .join("uno-q-bridge");

    if let Some(h) = host {
        if bridge_dir.exists() {
            deploy_remote(h, &bridge_dir)?;
        } else {
            anyhow::bail!(
                "Bridge app not found at {}. Run from zeroclaw repo root.",
                bridge_dir.display()
            );
        }
    } else {
        deploy_local(if bridge_dir.exists() {
            Some(&bridge_dir)
        } else {
            None
        })?;
    }
    Ok(())
}

fn deploy_remote(host: &str, bridge_dir: &std::path::Path) -> Result<()> {
    let ssh_target = if host.contains('@') {
        host.to_string()
    } else {
        format!("arduino@{}", host)
    };

    println!("Copying Bridge app to {}...", host);
    let status = Command::new("ssh")
        .args([&ssh_target, "mkdir", "-p", "~/ArduinoApps"])
        .status()
        .context("ssh mkdir failed")?;
    if !status.success() {
        anyhow::bail!("Failed to create ArduinoApps dir on Uno Q");
    }

    let status = Command::new("scp")
        .args([
            "-r",
            bridge_dir.to_str().unwrap(),
            &format!("{}:~/ArduinoApps/", ssh_target),
        ])
        .status()
        .context("scp failed")?;
    if !status.success() {
        anyhow::bail!("Failed to copy Bridge app");
    }

    println!("Starting Bridge app on Uno Q...");
    let status = Command::new("ssh")
        .args([
            &ssh_target,
            "arduino-app-cli",
            "app",
            "start",
            "~/ArduinoApps/uno-q-bridge",
        ])
        .status()
        .context("arduino-app-cli start failed")?;
    if !status.success() {
        anyhow::bail!("Failed to start Bridge app. Ensure arduino-app-cli is installed on Uno Q.");
    }

    println!("ZeroClaw Bridge app started. Add to config.toml:");
    println!("  [[peripherals.boards]]");
    println!("  board = \"arduino-uno-q\"");
    println!("  transport = \"bridge\"");
    Ok(())
}

fn deploy_local(bridge_dir: Option<&std::path::Path>) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/arduino".into());
    let apps_dir = std::path::Path::new(&home).join("ArduinoApps");
    let dest_dir = apps_dir.join(BRIDGE_APP_NAME);

    std::fs::create_dir_all(&dest_dir).context("create dest dir")?;

    if let Some(src) = bridge_dir {
        println!("Copying Bridge app from repo...");
        copy_dir(src, &dest_dir)?;
    } else {
        println!("Writing embedded Bridge app...");
        write_embedded_bridge(&dest_dir)?;
    }

    println!("Starting Bridge app...");
    let status = Command::new("arduino-app-cli")
        .args(["app", "start", dest_dir.to_str().unwrap()])
        .status()
        .context("arduino-app-cli start failed")?;
    if !status.success() {
        anyhow::bail!("Failed to start Bridge app. Ensure arduino-app-cli is installed on Uno Q.");
    }

    println!("ZeroClaw Bridge app started.");
    Ok(())
}

fn write_embedded_bridge(dest: &std::path::Path) -> Result<()> {
    let app_yaml = include_str!("../../firmware/uno-q-bridge/app.yaml");
    let sketch_ino = include_str!("../../firmware/uno-q-bridge/sketch/sketch.ino");
    let sketch_yaml = include_str!("../../firmware/uno-q-bridge/sketch/sketch.yaml");
    let main_py = include_str!("../../firmware/uno-q-bridge/python/main.py");
    let requirements = include_str!("../../firmware/uno-q-bridge/python/requirements.txt");

    std::fs::write(dest.join("app.yaml"), app_yaml)?;
    std::fs::create_dir_all(dest.join("sketch"))?;
    std::fs::write(dest.join("sketch").join("sketch.ino"), sketch_ino)?;
    std::fs::write(dest.join("sketch").join("sketch.yaml"), sketch_yaml)?;
    std::fs::create_dir_all(dest.join("python"))?;
    std::fs::write(dest.join("python").join("main.py"), main_py)?;
    std::fs::write(dest.join("python").join("requirements.txt"), requirements)?;
    Ok(())
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let e = entry?;
        let name = e.file_name();
        let src_path = src.join(&name);
        let dst_path = dst.join(&name);
        if e.file_type()?.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_app_name_is_correct() {
        assert_eq!(BRIDGE_APP_NAME, "uno-q-bridge");
    }

    #[test]
    fn write_embedded_bridge_creates_all_files() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dest = tmp.path();

        write_embedded_bridge(dest).expect("write_embedded_bridge failed");

        // Verify all expected files exist
        assert!(dest.join("app.yaml").exists(), "app.yaml should exist");
        assert!(
            dest.join("sketch").join("sketch.ino").exists(),
            "sketch/sketch.ino should exist"
        );
        assert!(
            dest.join("sketch").join("sketch.yaml").exists(),
            "sketch/sketch.yaml should exist"
        );
        assert!(
            dest.join("python").join("main.py").exists(),
            "python/main.py should exist"
        );
        assert!(
            dest.join("python").join("requirements.txt").exists(),
            "python/requirements.txt should exist"
        );
    }

    #[test]
    fn write_embedded_bridge_files_are_non_empty() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dest = tmp.path();

        write_embedded_bridge(dest).expect("write_embedded_bridge failed");

        let app_yaml = std::fs::read_to_string(dest.join("app.yaml")).unwrap();
        assert!(!app_yaml.trim().is_empty(), "app.yaml should not be empty");

        let sketch = std::fs::read_to_string(dest.join("sketch").join("sketch.ino")).unwrap();
        assert!(!sketch.trim().is_empty(), "sketch.ino should not be empty");

        let main_py = std::fs::read_to_string(dest.join("python").join("main.py")).unwrap();
        assert!(!main_py.trim().is_empty(), "main.py should not be empty");
    }

    #[test]
    fn write_embedded_bridge_main_py_contains_zeroclaw() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dest = tmp.path();

        write_embedded_bridge(dest).expect("write_embedded_bridge failed");

        let main_py = std::fs::read_to_string(dest.join("python").join("main.py")).unwrap();
        assert!(
            main_py.contains("ZeroClaw") || main_py.contains("zeroclaw"),
            "main.py should contain ZeroClaw marker"
        );
    }

    #[test]
    fn write_embedded_bridge_idempotent() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        let dest = tmp.path();

        // Write twice — should not fail
        write_embedded_bridge(dest).expect("first write failed");
        write_embedded_bridge(dest).expect("second write should overwrite without error");

        assert!(dest.join("app.yaml").exists());
    }

    #[test]
    fn copy_dir_copies_files_and_subdirs() {
        let src_tmp = tempfile::tempdir().expect("create src dir");
        let dst_tmp = tempfile::tempdir().expect("create dst dir");

        // Create a source tree: file.txt and sub/nested.txt
        std::fs::write(src_tmp.path().join("file.txt"), "hello").unwrap();
        std::fs::create_dir(src_tmp.path().join("sub")).unwrap();
        std::fs::write(src_tmp.path().join("sub").join("nested.txt"), "world").unwrap();

        copy_dir(src_tmp.path(), dst_tmp.path()).expect("copy_dir failed");

        assert_eq!(
            std::fs::read_to_string(dst_tmp.path().join("file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dst_tmp.path().join("sub").join("nested.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn bridge_dir_resolves_from_cargo_manifest() {
        // Verify that the bridge directory path is correctly derived from CARGO_MANIFEST_DIR.
        let bridge_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("firmware")
            .join("uno-q-bridge");
        assert!(
            bridge_dir.exists(),
            "firmware/uno-q-bridge should exist at {:?}",
            bridge_dir
        );
        assert!(
            bridge_dir.join("app.yaml").exists(),
            "app.yaml should exist in bridge dir"
        );
    }
}
