//! Hardware discovery — USB device enumeration and introspection.
//!
//! See `docs/hardware-peripherals-design.md` for the full design.

pub mod registry;

#[cfg(feature = "hardware")]
pub mod discover;

#[cfg(feature = "hardware")]
pub mod introspect;

use crate::config::Config;
use anyhow::Result;

// Re-export config types so wizard can use `hardware::HardwareConfig` etc.
pub use crate::config::{HardwareConfig, HardwareTransport};

/// A hardware device discovered during auto-scan.
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    pub name: String,
    pub detail: Option<String>,
    pub device_path: Option<String>,
    pub transport: HardwareTransport,
}

/// Auto-discover connected hardware devices.
/// Returns an empty vec on platforms without hardware support.
pub fn discover_hardware() -> Vec<DiscoveredDevice> {
    // USB/serial discovery is behind the "hardware" feature gate.
    #[cfg(feature = "hardware")]
    {
        if let Ok(devices) = discover::list_usb_devices() {
            return devices
                .into_iter()
                .map(|d| DiscoveredDevice {
                    name: d
                        .board_name
                        .unwrap_or_else(|| format!("{:04x}:{:04x}", d.vid, d.pid)),
                    detail: d.product_string,
                    device_path: None,
                    transport: if d.architecture.as_deref() == Some("native") {
                        HardwareTransport::Native
                    } else {
                        HardwareTransport::Serial
                    },
                })
                .collect();
        }
    }
    Vec::new()
}

/// Return the recommended default wizard choice index based on discovered devices.
/// 0 = Native, 1 = Tethered/Serial, 2 = Debug Probe, 3 = Software Only
pub fn recommended_wizard_default(devices: &[DiscoveredDevice]) -> usize {
    if devices.is_empty() {
        3 // software only
    } else {
        1 // tethered (most common for detected USB devices)
    }
}

/// Build a `HardwareConfig` from the wizard menu choice (0–3) and discovered devices.
pub fn config_from_wizard_choice(choice: usize, devices: &[DiscoveredDevice]) -> HardwareConfig {
    match choice {
        0 => HardwareConfig {
            enabled: true,
            transport: HardwareTransport::Native,
            ..HardwareConfig::default()
        },
        1 => {
            let serial_port = devices
                .iter()
                .find(|d| d.transport == HardwareTransport::Serial)
                .and_then(|d| d.device_path.clone());
            HardwareConfig {
                enabled: true,
                transport: HardwareTransport::Serial,
                serial_port,
                ..HardwareConfig::default()
            }
        }
        2 => HardwareConfig {
            enabled: true,
            transport: HardwareTransport::Probe,
            ..HardwareConfig::default()
        },
        _ => HardwareConfig::default(), // software only
    }
}

/// Handle `zeroclaw hardware` subcommands.
#[allow(clippy::module_name_repetitions)]
pub fn handle_command(cmd: crate::HardwareCommands, _config: &Config) -> Result<()> {
    #[cfg(not(feature = "hardware"))]
    {
        println!("Hardware discovery requires the 'hardware' feature.");
        println!("Build with: cargo build --features hardware");
        return Ok(());
    }

    #[cfg(feature = "hardware")]
    match cmd {
        crate::HardwareCommands::Discover => run_discover(),
        crate::HardwareCommands::Introspect { path } => run_introspect(&path),
        crate::HardwareCommands::Info { chip } => run_info(&chip),
    }
}

#[cfg(feature = "hardware")]
fn run_discover() -> Result<()> {
    let devices = discover::list_usb_devices()?;

    if devices.is_empty() {
        println!("No USB devices found.");
        println!();
        println!("Connect a board (e.g. Nucleo-F401RE) via USB and try again.");
        return Ok(());
    }

    println!("USB devices:");
    println!();
    for d in &devices {
        let board = d.board_name.as_deref().unwrap_or("(unknown)");
        let arch = d.architecture.as_deref().unwrap_or("—");
        let product = d.product_string.as_deref().unwrap_or("—");
        println!(
            "  {:04x}:{:04x}  {}  {}  {}",
            d.vid, d.pid, board, arch, product
        );
    }
    println!();
    println!("Known boards: nucleo-f401re, nucleo-f411re, arduino-uno, arduino-mega, cp2102");

    Ok(())
}

#[cfg(feature = "hardware")]
fn run_introspect(path: &str) -> Result<()> {
    let result = introspect::introspect_device(path)?;

    println!("Device at {}:", result.path);
    println!();
    if let (Some(vid), Some(pid)) = (result.vid, result.pid) {
        println!("  VID:PID     {:04x}:{:04x}", vid, pid);
    } else {
        println!("  VID:PID     (could not correlate with USB device)");
    }
    if let Some(name) = &result.board_name {
        println!("  Board       {}", name);
    }
    if let Some(arch) = &result.architecture {
        println!("  Architecture {}", arch);
    }
    println!("  Memory map  {}", result.memory_map_note);

    Ok(())
}

#[cfg(feature = "hardware")]
fn run_info(chip: &str) -> Result<()> {
    #[cfg(feature = "probe")]
    {
        match info_via_probe(chip) {
            Ok(()) => return Ok(()),
            Err(e) => {
                println!("probe-rs attach failed: {}", e);
                println!();
                println!(
                    "Ensure Nucleo is connected via USB. The ST-Link is built into the board."
                );
                println!("No firmware needs to be flashed — probe-rs reads chip info over SWD.");
                return Err(e.into());
            }
        }
    }

    #[cfg(not(feature = "probe"))]
    {
        println!("Chip info via USB requires the 'probe' feature.");
        println!();
        println!("Build with: cargo build --features hardware,probe");
        println!();
        println!("Then run: zeroclaw hardware info --chip {}", chip);
        println!();
        println!("This uses probe-rs to attach to the Nucleo's ST-Link over USB");
        println!("and read chip info (memory map, etc.) — no firmware on target needed.");
        Ok(())
    }
}

#[cfg(all(feature = "hardware", feature = "probe"))]
fn info_via_probe(chip: &str) -> anyhow::Result<()> {
    use probe_rs::config::MemoryRegion;
    use probe_rs::{Session, SessionConfig};

    println!("Connecting to {} via USB (ST-Link)...", chip);
    let session = Session::auto_attach(chip, SessionConfig::default())
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let target = session.target();
    println!();
    println!("Chip: {}", target.name);
    println!("Architecture: {:?}", session.architecture());
    println!();
    println!("Memory map:");
    for region in target.memory_map.iter() {
        match region {
            MemoryRegion::Ram(ram) => {
                let start = ram.range.start;
                let end = ram.range.end;
                let size_kb = (end - start) / 1024;
                println!("  RAM: 0x{:08X} - 0x{:08X} ({} KB)", start, end, size_kb);
            }
            MemoryRegion::Nvm(flash) => {
                let start = flash.range.start;
                let end = flash.range.end;
                let size_kb = (end - start) / 1024;
                println!("  Flash: 0x{:08X} - 0x{:08X} ({} KB)", start, end, size_kb);
            }
            _ => {}
        }
    }
    println!();
    println!("Info read via USB (SWD) — no firmware on target needed.");
    Ok(())
}
