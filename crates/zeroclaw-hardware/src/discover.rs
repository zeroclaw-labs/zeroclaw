//! USB device discovery — enumerate devices and enrich with board registry.
//!
//! USB enumeration via `nusb` is only supported on Linux, macOS, and Windows.
//! On Android (Termux) and other unsupported platforms this module is excluded
//! from compilation; callers in `hardware/mod.rs` fall back to an empty result.

#![cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]

use super::registry;
use anyhow::Result;
use nusb::MaybeFuture;

/// Serial port with USB VID/PID for device registration.
#[derive(Debug, Clone)]
pub struct SerialDeviceInfo {
    pub port_path: String,
    pub vid: u16,
    pub pid: u16,
    pub board_name: Option<String>,
    pub architecture: Option<String>,
}

/// Enumerate serial ports that correspond to known USB devices.
/// Returns empty when hardware feature is disabled or enumeration fails.
#[cfg(feature = "hardware")]
pub fn scan_serial_devices() -> Vec<SerialDeviceInfo> {
    let mut result = Vec::new();
    let Ok(ports) = tokio_serial::available_ports() else {
        return result;
    };
    for port in ports {
        let port_name = port.port_name.as_str();
        if !crate::util::is_serial_path_allowed(port_name) {
            continue;
        }
        let (vid, pid) = match &port.port_type {
            tokio_serial::SerialPortType::UsbPort(usb) => (usb.vid, usb.pid),
            _ => (0, 0),
        };
        let board = if vid != 0 {
            registry::lookup_board(vid, pid)
        } else {
            None
        };
        result.push(SerialDeviceInfo {
            port_path: port_name.to_string(),
            vid,
            pid,
            board_name: board.map(|b| b.name.to_string()),
            architecture: board.and_then(|b| b.architecture.map(String::from)),
        });
    }
    result
}

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
