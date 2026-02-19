//! USB device discovery — enumerate devices and enrich with board registry.
//!
//! On Android (Termux) `nusb` does not expose `list_devices()` — it is gated
//! behind `#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]`
//! inside the `nusb` crate.  We guard our call site the same way and return an
//! empty list on Android so the crate compiles cleanly in Termux.

use super::registry;
use anyhow::Result;

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
///
/// Returns an empty `Vec` on Android/Termux where `nusb` does not support
/// USB device enumeration.  All other platforms (Linux, macOS, Windows)
/// perform a real scan.
#[cfg(feature = "hardware")]
pub fn list_usb_devices() -> Result<Vec<UsbDeviceInfo>> {
    #[cfg(not(target_os = "android"))]
    {
        use nusb::MaybeFuture;

        let mut devices = Vec::new();

        let iter = nusb::list_devices()
            .wait()
            .map_err(|e| anyhow::anyhow!("USB enumeration failed: {e}"))?;

        for dev in iter {
            let vid: u16 = dev.vendor_id();
            let pid: u16 = dev.product_id();
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

    // Android/Termux: nusb does not support USB enumeration on this platform.
    #[cfg(target_os = "android")]
    {
        Ok(Vec::new())
    }
}

