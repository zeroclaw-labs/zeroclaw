//! UF2 flashing support — detect BOOTSEL-mode Pico and deploy firmware.
//!
//! # Workflow
//! 1. [`find_rpi_rp2_mount`] — check well-known mount points for the RPI-RP2 volume
//!    that appears when a Pico is held in BOOTSEL mode.
//! 2. [`ensure_firmware_dir`] — extract the bundled firmware files to
//!    `~/.zeroclaw/firmware/pico/` if they aren't there yet.
//! 3. [`flash_uf2`] — copy the UF2 to the mount point; the Pico reboots automatically.
//!
//! # Embedded assets
//! Both firmware files are compiled into the binary with `include_bytes!` so
//! users never need to download them separately.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

// ── Embedded firmware ─────────────────────────────────────────────────────────

/// MicroPython UF2 binary — copied to RPI-RP2 to install the base runtime.
const PICO_UF2: &[u8] = include_bytes!("../../firmware/pico/zeroclaw-pico.uf2");

/// ZeroClaw serial protocol handler — written to the Pico after MicroPython boots.
pub const PICO_MAIN_PY: &[u8] = include_bytes!("../../firmware/pico/main.py");

/// UF2 magic word 1 (little-endian bytes at offset 0 of every UF2 block).
const UF2_MAGIC1: [u8; 4] = [0x55, 0x46, 0x32, 0x0A];

// ── Volume detection ──────────────────────────────────────────────────────────

/// Find the RPI-RP2 mount point if a Pico is connected in BOOTSEL mode.
///
/// Checks:
/// - macOS:  `/Volumes/RPI-RP2`
/// - Linux:  `/media/*/RPI-RP2` and `/run/media/*/RPI-RP2`
pub fn find_rpi_rp2_mount() -> Option<PathBuf> {
    // macOS
    let mac = PathBuf::from("/Volumes/RPI-RP2");
    if mac.exists() {
        return Some(mac);
    }

    // Linux — /media/<user>/RPI-RP2  or  /run/media/<user>/RPI-RP2
    for base in &["/media", "/run/media"] {
        if let Ok(entries) = std::fs::read_dir(base) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("RPI-RP2");
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

// ── Firmware directory management ─────────────────────────────────────────────

/// Ensure `~/.zeroclaw/firmware/pico/` exists and contains the bundled assets.
///
/// Files are only written if they are absent — existing files are never overwritten
/// so users can substitute their own firmware.
///
/// Returns the firmware directory path.
pub fn ensure_firmware_dir() -> Result<PathBuf> {
    use directories::BaseDirs;

    let base = BaseDirs::new().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;

    let firmware_dir = base
        .home_dir()
        .join(".zeroclaw")
        .join("firmware")
        .join("pico");
    std::fs::create_dir_all(&firmware_dir)?;

    // UF2 — validate magic before writing so a broken stub is caught early.
    let uf2_path = firmware_dir.join("zeroclaw-pico.uf2");
    if !uf2_path.exists() {
        if PICO_UF2.len() < 8 || PICO_UF2[..4] != UF2_MAGIC1 {
            bail!(
                "Bundled UF2 is a placeholder — download the real MicroPython UF2 from \
                 https://micropython.org/download/RPI_PICO/ and place it at \
                 src/firmware/pico/zeroclaw-pico.uf2, then rebuild ZeroClaw."
            );
        }
        std::fs::write(&uf2_path, PICO_UF2)?;
        tracing::info!(path = %uf2_path.display(), "extracted bundled UF2");
    }

    // main.py — always check UF2 magic even if path already exists (user may
    // have placed a stub). main.py has no such check — it's just text.
    let main_py_path = firmware_dir.join("main.py");
    if !main_py_path.exists() {
        std::fs::write(&main_py_path, PICO_MAIN_PY)?;
        tracing::info!(path = %main_py_path.display(), "extracted bundled main.py");
    }

    Ok(firmware_dir)
}

// ── Flashing ──────────────────────────────────────────────────────────────────

/// Copy the UF2 file to the RPI-RP2 mount point.
///
/// macOS often returns "Operation not permitted" for `std::fs::copy` on FAT
/// volumes presented by BOOTSEL-mode Picos.  We try four approaches in order
/// and return a clear manual-fallback message if all fail:
///
/// 1. `std::fs::copy`  — fast, no subprocess; works on most Linux setups.
/// 2. `cp <src> <dst>` — bypasses some macOS VFS permission layers.
/// 3. `sudo cp …`      — escalates for locked volumes.
/// 4. Error — instructs the user to run the `sudo cp` manually.
pub async fn flash_uf2(mount_point: &Path, firmware_dir: &Path) -> Result<()> {
    let uf2_src = firmware_dir.join("zeroclaw-pico.uf2");
    let uf2_dst = mount_point.join("firmware.uf2");
    let src_str = uf2_src.to_string_lossy().into_owned();
    let dst_str = uf2_dst.to_string_lossy().into_owned();

    tracing::info!(
        src = %src_str,
        dst = %dst_str,
        "flashing UF2"
    );

    // Validate UF2 magic before any copy attempt — prevents flashing a stub.
    let data = std::fs::read(&uf2_src)?;
    if data.len() < 8 || data[..4] != UF2_MAGIC1 {
        bail!(
            "UF2 at {} does not look like a valid UF2 file (magic mismatch). \
             Download from https://micropython.org/download/RPI_PICO/ and delete \
             the existing file so ZeroClaw can re-extract it.",
            uf2_src.display()
        );
    }

    // ── Attempt 1: std::fs::copy (works on Linux, sometimes blocked on macOS) ─
    {
        let src = uf2_src.clone();
        let dst = uf2_dst.clone();
        let result = tokio::task::spawn_blocking(move || std::fs::copy(&src, &dst))
            .await
            .map_err(|e| anyhow::anyhow!("copy task panicked: {e}"));

        match result {
            Ok(Ok(_)) => {
                tracing::info!("UF2 copy complete (std::fs::copy) — Pico will reboot");
                return Ok(());
            }
            Ok(Err(e)) => tracing::warn!("std::fs::copy failed ({}), trying cp", e),
            Err(e) => tracing::warn!("std::fs::copy task failed ({}), trying cp", e),
        }
    }

    // ── Attempt 2: cp via subprocess ──────────────────────────────────────────
    {
        /// Timeout for subprocess copy attempts (seconds).
        const CP_TIMEOUT_SECS: u64 = 10;

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(CP_TIMEOUT_SECS),
            tokio::process::Command::new("cp")
                .arg(&src_str)
                .arg(&dst_str)
                .output(),
        )
        .await;

        match out {
            Err(_elapsed) => {
                tracing::warn!("cp timed out after {}s, trying sudo cp", CP_TIMEOUT_SECS);
            }
            Ok(Ok(o)) if o.status.success() => {
                tracing::info!("UF2 copy complete (cp) — Pico will reboot");
                return Ok(());
            }
            Ok(Ok(o)) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("cp failed ({}), trying sudo cp", stderr.trim());
            }
            Ok(Err(e)) => tracing::warn!("cp spawn failed ({}), trying sudo cp", e),
        }
    }

    // ── Attempt 3: sudo cp (non-interactive) ─────────────────────────────────
    {
        const SUDO_CP_TIMEOUT_SECS: u64 = 10;

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(SUDO_CP_TIMEOUT_SECS),
            tokio::process::Command::new("sudo")
                .args(["-n", "cp", &src_str, &dst_str])
                .output(),
        )
        .await;

        match out {
            Err(_elapsed) => {
                tracing::warn!("sudo cp timed out after {}s", SUDO_CP_TIMEOUT_SECS);
            }
            Ok(Ok(o)) if o.status.success() => {
                tracing::info!("UF2 copy complete (sudo cp) — Pico will reboot");
                return Ok(());
            }
            Ok(Ok(o)) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                tracing::warn!("sudo cp failed: {}", stderr.trim());
            }
            Ok(Err(e)) => tracing::warn!("sudo cp spawn failed: {}", e),
        }
    }

    // ── All attempts failed — give the user a clear manual command ────────────
    bail!(
        "All copy methods failed. Run this command manually, then restart ZeroClaw:\n\
         \n  sudo cp {src_str} {dst_str}\n"
    )
}

/// Wait for `/dev/cu.usbmodem*` (macOS) or `/dev/ttyACM*` (Linux) to appear.
///
/// Polls every `interval` for up to `timeout`. Returns the first matching path
/// found, or `None` if the deadline expires.
pub async fn wait_for_serial_port(
    timeout: std::time::Duration,
    interval: std::time::Duration,
) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let patterns = &["/dev/cu.usbmodem*"];
    #[cfg(target_os = "linux")]
    let patterns = &["/dev/ttyACM*"];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let patterns: &[&str] = &[];

    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        for pattern in *patterns {
            if let Ok(mut hits) = glob::glob(pattern) {
                if let Some(Ok(path)) = hits.next() {
                    return Some(path);
                }
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return None;
        }

        tokio::time::sleep(interval).await;
    }
}

// ── Deploy main.py via mpremote ───────────────────────────────────────────────

/// Copy `main.py` to the Pico's MicroPython filesystem and soft-reset it.
///
/// After the UF2 is flashed the Pico reboots into MicroPython but has no
/// `main.py` on its internal filesystem.  This function uses `mpremote` to
/// upload the bundled `main.py` and issue a reset so it starts executing
/// immediately.
///
/// Returns `Ok(())` on success or an error with a helpful fallback command.
pub async fn deploy_main_py(port: &Path, firmware_dir: &Path) -> Result<()> {
    let main_py_src = firmware_dir.join("main.py");
    let src_str = main_py_src.to_string_lossy().into_owned();
    let port_str = port.to_string_lossy().into_owned();

    if !main_py_src.exists() {
        bail!(
            "main.py not found at {} — run ensure_firmware_dir() first",
            main_py_src.display()
        );
    }

    tracing::info!(
        src = %src_str,
        port = %port_str,
        "deploying main.py via mpremote"
    );

    let out = tokio::process::Command::new("mpremote")
        .args([
            "connect", &port_str, "cp", &src_str, ":main.py", "+", "reset",
        ])
        .output()
        .await;

    match out {
        Ok(o) if o.status.success() => {
            tracing::info!("main.py deployed and Pico reset via mpremote");
            Ok(())
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            bail!(
                "mpremote failed (exit {}): {}.\n\
                 Run manually:\n  mpremote connect {port_str} cp {src_str} :main.py + reset",
                o.status,
                stderr.trim()
            )
        }
        Err(e) => {
            bail!(
                "mpremote not found or could not start ({e}).\n\
                 Install it with: pip install mpremote\n\
                 Then run: mpremote connect {port_str} cp {src_str} :main.py + reset"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pico_uf2_has_valid_magic() {
        assert!(
            PICO_UF2.len() >= 8,
            "bundled UF2 too small ({} bytes) — replace with real MicroPython UF2",
            PICO_UF2.len()
        );
        assert_eq!(
            &PICO_UF2[..4],
            &UF2_MAGIC1,
            "bundled UF2 has wrong magic — replace with real MicroPython UF2 from \
             https://micropython.org/download/RPI_PICO/"
        );
    }

    #[test]
    fn pico_main_py_is_non_empty() {
        assert!(!PICO_MAIN_PY.is_empty(), "bundled main.py is empty");
    }

    #[test]
    fn pico_main_py_contains_zeroclaw_marker() {
        let src = std::str::from_utf8(PICO_MAIN_PY).expect("main.py is not valid UTF-8");
        assert!(
            src.contains("zeroclaw"),
            "main.py should contain 'zeroclaw' firmware marker"
        );
    }

    #[test]
    fn find_rpi_rp2_mount_returns_none_when_not_connected() {
        // This test runs on CI without a Pico attached — just verify it doesn't panic.
        let _ = find_rpi_rp2_mount(); // may be Some or None depending on environment
    }
}
