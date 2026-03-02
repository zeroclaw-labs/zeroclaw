//! USB and serial device discovery.
//!
//! - `list_usb_devices` — enumerate USB devices via `nusb` (cross-platform).
//! - `scan_serial_devices` — enumerate serial ports (`/dev/ttyACM*`, etc.),
//!   read VID/PID from sysfs (Linux), and return `SerialDeviceInfo` records
//!   ready for `DeviceRegistry` population.

use super::registry;
use anyhow::Result;
use nusb::MaybeFuture;

/// Information about a discovered USB device.
#[derive(Debug, Clone)]
pub struct UsbDeviceInfo {
    pub bus_id: String,
    pub device_address: u8,
    pub vid: u16,
    pub pid: u16,
    pub product_string: Option<String>,
    pub board_name: Option<String>,
    pub architecture: Option<String>,
}

/// Enumerate all connected USB devices and enrich with board registry lookup.
#[cfg(feature = "hardware")]
pub fn list_usb_devices() -> Result<Vec<UsbDeviceInfo>> {
    let mut devices = Vec::new();

    let iter = nusb::list_devices()
        .wait()
        .map_err(|e| anyhow::anyhow!("USB enumeration failed: {e}"))?;

    for dev in iter {
        let vid = dev.vendor_id();
        let pid = dev.product_id();
        let board = registry::lookup_board(vid, pid);

        devices.push(UsbDeviceInfo {
            bus_id: dev.bus_id().to_string(),
            device_address: dev.device_address(),
            vid,
            pid,
            product_string: dev.product_string().map(String::from),
            board_name: board.map(|b| b.name.to_string()),
            architecture: board.and_then(|b| b.architecture.map(String::from)),
        });
    }

    Ok(devices)
}

// ── Serial port discovery ─────────────────────────────────────────────────────

/// A serial device found during port scan, enriched with board registry data.
#[derive(Debug, Clone)]
pub struct SerialDeviceInfo {
    /// Full port path (e.g. `"/dev/ttyACM0"`, `"/dev/tty.usbmodem14101"`).
    pub port_path: String,
    /// USB Vendor ID read from sysfs/IOKit. `0` if unknown.
    pub vid: u16,
    /// USB Product ID read from sysfs/IOKit. `0` if unknown.
    pub pid: u16,
    /// Board name from the registry, if VID/PID was recognised.
    pub board_name: Option<String>,
    /// Architecture description from the registry.
    pub architecture: Option<String>,
}

/// Scan for connected serial-port devices and return their metadata.
///
/// On Linux: globs `/dev/ttyACM*` and `/dev/ttyUSB*`, reads VID/PID via sysfs.
/// On macOS: globs `/dev/tty.usbmodem*`, `/dev/cu.usbmodem*`,
///            `/dev/tty.usbserial*`, `/dev/cu.usbserial*` — VID/PID via nusb heuristic.
/// On other platforms or when the `hardware` feature is off: returns empty `Vec`.
///
/// This function is **synchronous** — it only touches the filesystem (sysfs,
/// glob) and does no I/O to the device. The async ping handshake is done
/// separately in `DeviceRegistry::discover`.
#[cfg(feature = "hardware")]
pub fn scan_serial_devices() -> Vec<SerialDeviceInfo> {
    #[cfg(target_os = "linux")]
    {
        scan_serial_devices_linux()
    }
    #[cfg(target_os = "macos")]
    {
        scan_serial_devices_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

// ── Linux: sysfs-based VID/PID correlation ───────────────────────────────────

#[cfg(all(feature = "hardware", target_os = "linux"))]
fn scan_serial_devices_linux() -> Vec<SerialDeviceInfo> {
    let mut results = Vec::new();

    for pattern in &["/dev/ttyACM*", "/dev/ttyUSB*"] {
        let paths = match glob::glob(pattern) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for path_result in paths.flatten() {
            let port_path = path_result.to_string_lossy().to_string();
            let port_name = path_result
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let (vid, pid) = vid_pid_from_sysfs(&port_name).unwrap_or((0, 0));
            let board = registry::lookup_board(vid, pid);

            results.push(SerialDeviceInfo {
                port_path,
                vid,
                pid,
                board_name: board.map(|b| b.name.to_string()),
                architecture: board.and_then(|b| b.architecture.map(String::from)),
            });
        }
    }

    results
}

/// Read VID and PID for a tty port from Linux sysfs.
///
/// Follows the symlink chain:
/// `/sys/class/tty/<port_name>/device` → canonicalised USB interface directory
/// then climbs to parent (or grandparent) USB device to read `idVendor`/`idProduct`.
#[cfg(all(feature = "hardware", target_os = "linux"))]
fn vid_pid_from_sysfs(port_name: &str) -> Option<(u16, u16)> {
    use std::path::Path;

    let device_link = format!("/sys/class/tty/{}/device", port_name);
    // Resolve the symlink chain to a real absolute path.
    let device_path = std::fs::canonicalize(device_link).ok()?;

    // ttyACM (CDC ACM): device_path = …/2-1:1.0 (interface)
    // idVendor is at the USB device level, one directory up.
    if let Some((v, p)) = try_read_vid_pid(device_path.parent()?) {
        return Some((v, p));
    }

    // ttyUSB (USB-serial chips like CH340, FTDI):
    // device_path = …/usb-serial/ttyUSB0 or …/2-1:1.0/ttyUSB0
    // May need grandparent to reach the USB device.
    device_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(try_read_vid_pid)
}

/// Try to read `idVendor` and `idProduct` files from a directory.
#[cfg(all(feature = "hardware", target_os = "linux"))]
fn try_read_vid_pid(dir: &std::path::Path) -> Option<(u16, u16)> {
    let vid = read_hex_u16(dir.join("idVendor"))?;
    let pid = read_hex_u16(dir.join("idProduct"))?;
    Some((vid, pid))
}

/// Read a hex-formatted u16 from a sysfs file (e.g. `"2e8a\n"` → `0x2E8A`).
#[cfg(all(feature = "hardware", target_os = "linux"))]
fn read_hex_u16(path: impl AsRef<std::path::Path>) -> Option<u16> {
    let s = std::fs::read_to_string(path).ok()?;
    u16::from_str_radix(s.trim(), 16).ok()
}

// ── macOS: glob tty paths, no sysfs ──────────────────────────────────────────

/// On macOS, enumerate common USB CDC and USB-serial tty paths.
/// VID/PID cannot be read from the path alone — they come back as 0/0.
/// Unknown-VID devices will be probed during `DeviceRegistry::discover`.
#[cfg(all(feature = "hardware", target_os = "macos"))]
fn scan_serial_devices_macos() -> Vec<SerialDeviceInfo> {
    let mut results = Vec::new();

    // cu.* variants are preferred on macOS (call-up; tty.* are call-in).
    for pattern in &[
        "/dev/cu.usbmodem*",
        "/dev/cu.usbserial*",
        "/dev/tty.usbmodem*",
        "/dev/tty.usbserial*",
    ] {
        let paths = match glob::glob(pattern) {
            Ok(p) => p,
            Err(_) => continue,
        };

        for path_result in paths.flatten() {
            let port_path = path_result.to_string_lossy().to_string();
            // No sysfs on macOS — VID/PID unknown; will be resolved via ping.
            results.push(SerialDeviceInfo {
                port_path,
                vid: 0,
                pid: 0,
                board_name: None,
                architecture: None,
            });
        }
    }

    results
}
