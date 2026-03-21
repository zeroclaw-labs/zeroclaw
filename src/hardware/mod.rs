//! Hardware discovery — USB device enumeration and introspection.
//!
//! See `docs/hardware-peripherals-design.md` for the full design.

pub mod device;
pub mod gpio;
pub mod protocol;
pub mod registry;
pub mod transport;

#[cfg(all(
    feature = "hardware",
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
pub mod discover;

#[cfg(all(
    feature = "hardware",
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
pub mod introspect;

#[cfg(feature = "hardware")]
pub mod serial;

#[cfg(feature = "hardware")]
pub mod uf2;

#[cfg(feature = "hardware")]
pub mod pico_flash;

#[cfg(feature = "hardware")]
pub mod pico_code;

/// Aardvark USB adapter transport (I2C / SPI / GPIO via aardvark-sys).
#[cfg(feature = "hardware")]
pub mod aardvark;

/// Tools backed by the Aardvark transport (i2c_scan, i2c_read, i2c_write,
/// spi_transfer, gpio_aardvark).
#[cfg(feature = "hardware")]
pub mod aardvark_tools;

/// Datasheet management — search, download, and manage device datasheets.
/// Used by DatasheetTool when an Aardvark is connected.
#[cfg(feature = "hardware")]
pub mod datasheet;

/// Raspberry Pi self-discovery and native GPIO tools.
/// Only compiled on Linux with the `peripheral-rpi` feature.
#[cfg(all(feature = "peripheral-rpi", target_os = "linux"))]
pub mod rpi;

// ── Phase 4: ToolRegistry + plugin system ─────────────────────────────────────
pub mod loader;
pub mod manifest;
pub mod subprocess;
pub mod tool_registry;

#[cfg(feature = "hardware")]
#[allow(unused_imports)]
pub use aardvark::AardvarkTransport;

use crate::config::Config;
use crate::hardware::device::DeviceRegistry;
use anyhow::Result;
#[allow(unused_imports)]
pub use tool_registry::{ToolError, ToolRegistry};

// Re-export config types so wizard can use `hardware::HardwareConfig` etc.
pub use crate::config::{HardwareConfig, HardwareTransport};

// ── Phase 5: boot() — hardware tool integration into agent loop ───────────────

/// Merge hardware tools from a [`HardwareBootResult`] into an existing tool
/// registry, deduplicating by name.
///
/// Returns a tuple of `(device_summary, added_tool_names)`.
pub fn merge_hardware_tools(
    tools: &mut Vec<Box<dyn crate::tools::Tool>>,
    hw_boot: HardwareBootResult,
) -> (String, Vec<String>) {
    let device_summary = hw_boot.device_summary.clone();
    let mut added_tool_names: Vec<String> = Vec::new();
    if !hw_boot.tools.is_empty() {
        let existing: std::collections::HashSet<String> =
            tools.iter().map(|t| t.name().to_string()).collect();
        let new_hw_tools: Vec<Box<dyn crate::tools::Tool>> = hw_boot
            .tools
            .into_iter()
            .filter(|t| !existing.contains(t.name()))
            .collect();
        if !new_hw_tools.is_empty() {
            added_tool_names = new_hw_tools.iter().map(|t| t.name().to_string()).collect();
            tracing::info!(count = new_hw_tools.len(), "Hardware registry tools added");
            tools.extend(new_hw_tools);
        }
    }
    (device_summary, added_tool_names)
}

/// Result of [`boot`]: tools to merge into the agent + device summary for the
/// system prompt.
pub struct HardwareBootResult {
    /// Tools to extend into the agent's `tools_registry`.
    pub tools: Vec<Box<dyn crate::tools::Tool>>,
    /// Human-readable device summary for the LLM system prompt.
    pub device_summary: String,
    /// Content of `~/.zeroclaw/hardware/` context files (HARDWARE.md, device
    /// profiles, and skills) for injection into the system prompt.
    pub context_files_prompt: String,
}

/// Load hardware context files from `~/.zeroclaw/hardware/` and return them
/// concatenated as a single markdown string ready for system-prompt injection.
///
/// Reads (if they exist):
/// 1. `~/.zeroclaw/hardware/HARDWARE.md`
/// 2. `~/.zeroclaw/hardware/devices/<alias>.md` for each discovered alias
/// 3. All `~/.zeroclaw/hardware/skills/*.md` files (sorted by name)
///
/// Missing files are silently skipped. Returns an empty string when no files
/// are found.
pub fn load_hardware_context_prompt(aliases: &[&str]) -> String {
    let home = match directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
        Some(h) => h,
        None => return String::new(),
    };
    load_hardware_context_from_dir(&home.join(".zeroclaw").join("hardware"), aliases)
}

/// Inner helper that reads hardware context from an explicit base directory.
/// Separated from [`load_hardware_context_prompt`] to allow unit-testing with
/// a temporary directory.
fn load_hardware_context_from_dir(hw_dir: &std::path::Path, aliases: &[&str]) -> String {
    let mut sections: Vec<String> = Vec::new();

    // 1. Global HARDWARE.md
    let global = hw_dir.join("HARDWARE.md");
    if let Ok(content) = std::fs::read_to_string(&global) {
        if !content.trim().is_empty() {
            sections.push(content.trim().to_string());
        }
    }

    // 2. Per-device profile
    let devices_dir = hw_dir.join("devices");
    for alias in aliases {
        let path = devices_dir.join(format!("{alias}.md"));
        tracing::info!("loading device file: {:?}", path);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if !content.trim().is_empty() {
                sections.push(content.trim().to_string());
            }
        }
    }

    // 3. Skills directory (*.md files, sorted)
    let skills_dir = hw_dir.join("skills");
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        let mut skill_paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .collect();
        skill_paths.sort();
        for path in skill_paths {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    sections.push(content.trim().to_string());
                }
            }
        }
    }

    if sections.is_empty() {
        return String::new();
    }
    sections.join("\n\n")
}

/// Inject RPi self-discovery tools and system prompt context into the boot result.
///
/// Called from both `boot()` variants when the `peripheral-rpi` feature is active
/// and the binary is running on Linux. If `/proc/device-tree/model` (or
/// `/proc/cpuinfo`) identifies a Raspberry Pi, the four built-in GPIO/info
/// tools are added to `tools` and the board description is appended to
/// `context_files_prompt` so the LLM knows it is running on the device.
#[cfg(all(feature = "peripheral-rpi", target_os = "linux"))]
fn inject_rpi_context(
    tools: &mut Vec<Box<dyn crate::tools::Tool>>,
    context_files_prompt: &mut String,
) {
    if let Some(ctx) = rpi::RpiSystemContext::discover() {
        tracing::info!(board = %ctx.model.display_name(), ip = %ctx.ip_address, "RPi self-discovery complete");
        if let Some(led) = ctx.model.onboard_led_gpio() {
            tracing::info!(gpio = led, "Onboard ACT LED");
        }
        println!("[registry] rpi0 ready \u{2192} /dev/gpiomem");
        if ctx.gpio_available {
            tools.push(Box::new(rpi::GpioRpiWriteTool));
            tools.push(Box::new(rpi::GpioRpiReadTool));
            tools.push(Box::new(rpi::GpioRpiBlinkTool));
            println!("[registry] loaded built-in: gpio_rpi_write");
            println!("[registry] loaded built-in: gpio_rpi_read");
            println!("[registry] loaded built-in: gpio_rpi_blink");
        }
        tools.push(Box::new(rpi::RpiSystemInfoTool));
        println!("[registry] loaded built-in: rpi_system_info");
        ctx.write_hardware_context_file();
        // Load the device profile (rpi0.md) that was just written so its full
        // GPIO reference and tool-usage rules appear in the system prompt.
        let device_ctx = load_hardware_context_prompt(&["rpi0"]);
        if !device_ctx.is_empty() {
            if !context_files_prompt.is_empty() {
                context_files_prompt.push_str("\n\n");
            }
            context_files_prompt.push_str("## Connected Hardware Devices\n\n");
            context_files_prompt.push_str(&device_ctx);
        }
        let rpi_prompt = ctx.to_system_prompt();
        if !context_files_prompt.is_empty() {
            context_files_prompt.push_str("\n\n");
        }
        context_files_prompt.push_str(&rpi_prompt);
    }
}

/// Boot the hardware subsystem: discover devices + load tool registry.
///
/// With the `hardware` feature: enumerates USB-serial devices, then
/// pre-registers any config-specified serial boards not already found by
/// discovery. [`HardwareSerialTransport`] opens the port lazily per-send,
/// so this succeeds even when the port doesn't exist at startup.
///
/// Without the feature: loads plugin tools from `~/.zeroclaw/tools/` only,
/// with an empty device registry (GPIO tools will report "no device found"
/// if called, which is correct).
#[cfg(feature = "hardware")]
#[allow(unused_mut)] // tools and context_files_prompt are mutated on Linux+peripheral-rpi
pub async fn boot(
    peripherals: &crate::config::PeripheralsConfig,
) -> anyhow::Result<HardwareBootResult> {
    use self::serial::HardwareSerialTransport;
    use device::DeviceCapabilities;

    let mut registry_inner = DeviceRegistry::discover().await;

    // Pre-register config-specified serial boards not already found by USB
    // discovery. Transport opens lazily, so the port need not exist at boot.
    if peripherals.enabled {
        let mut discovered_paths: std::collections::HashSet<String> = registry_inner
            .all()
            .iter()
            .filter_map(|d| d.device_path.clone())
            .collect();

        for board in &peripherals.boards {
            if board.transport != "serial" {
                continue;
            }
            let path = match &board.path {
                Some(p) if !p.is_empty() => p.clone(),
                _ => continue,
            };
            if discovered_paths.contains(&path) {
                continue; // already registered by USB discovery or a previous config entry
            }
            let alias = registry_inner.register(&board.board, None, None, Some(path.clone()), None);
            let transport = std::sync::Arc::new(HardwareSerialTransport::new(&path, board.baud))
                as std::sync::Arc<dyn transport::Transport>;
            let caps = DeviceCapabilities {
                gpio: true,
                ..DeviceCapabilities::default()
            };
            registry_inner.attach_transport(&alias, transport, caps)
                .unwrap_or_else(|e| tracing::warn!(alias = %alias, err = %e, "attach_transport: unexpected unknown alias"));
            // Mark path as registered so duplicate config entries are skipped.
            discovered_paths.insert(path.clone());
            tracing::info!(
                board = %board.board,
                path = %path,
                alias = %alias,
                "pre-registered config board with lazy serial transport"
            );
        }
    }

    // BOOTSEL auto-detect: warn the user if a Pico is in BOOTSEL mode at startup.
    if uf2::find_rpi_rp2_mount().is_some() {
        tracing::info!("Pico detected in BOOTSEL mode (RPI-RP2 drive found)");
        tracing::info!("Say \"flash my pico\" to install ZeroClaw firmware automatically");
    }

    // Aardvark discovery: scan for Total Phase Aardvark USB adapters and
    // register each one with AardvarkTransport + full I2C/SPI/GPIO capabilities.
    {
        use aardvark::AardvarkTransport;
        use device::DeviceCapabilities;

        let aardvark_ports = aardvark_sys::AardvarkHandle::find_devices();
        for (i, &port) in aardvark_ports.iter().enumerate() {
            let alias = registry_inner.register(
                "aardvark",
                Some(0x2b76),
                None,
                None,
                Some("Total Phase Aardvark".to_string()),
            );
            let transport = std::sync::Arc::new(AardvarkTransport::new(i32::from(port), 100))
                as std::sync::Arc<dyn transport::Transport>;
            let caps = DeviceCapabilities {
                gpio: true,
                i2c: true,
                spi: true,
                ..DeviceCapabilities::default()
            };
            registry_inner
                .attach_transport(&alias, transport, caps)
                .unwrap_or_else(|e| {
                    tracing::warn!(alias = %alias, err = %e, "aardvark attach_transport failed")
                });
            tracing::info!(
                alias = %alias,
                port_index = %i,
                "aardvark adapter registered"
            );
            println!("[registry] {alias} ready \u{2192} Total Phase port {i}");
        }
    }

    let devices = std::sync::Arc::new(tokio::sync::RwLock::new(registry_inner));
    let registry = ToolRegistry::load(devices.clone()).await?;
    let device_summary = {
        let reg = devices.read().await;
        reg.prompt_summary()
    };
    let mut tools = registry.into_tools();
    if !tools.is_empty() {
        tracing::info!(count = tools.len(), "Hardware registry tools loaded");
    }
    let alias_strings: Vec<String> = {
        let reg = devices.read().await;
        reg.aliases()
            .into_iter()
            .map(|s: &str| s.to_string())
            .collect()
    };
    let alias_refs: Vec<&str> = alias_strings.iter().map(|s: &String| s.as_str()).collect();
    let mut context_files_prompt = load_hardware_context_prompt(&alias_refs);
    if !context_files_prompt.is_empty() {
        tracing::info!("Hardware context files loaded");
    }
    // RPi self-discovery: detect board model and inject GPIO tools + prompt context.
    #[cfg(all(feature = "peripheral-rpi", target_os = "linux"))]
    inject_rpi_context(&mut tools, &mut context_files_prompt);
    Ok(HardwareBootResult {
        tools,
        device_summary,
        context_files_prompt,
    })
}

/// Fallback when the `hardware` feature is disabled — plugins only.
#[cfg(not(feature = "hardware"))]
#[allow(unused_mut)] // tools and context_files_prompt are mutated on Linux+peripheral-rpi
pub async fn boot(
    _peripherals: &crate::config::PeripheralsConfig,
) -> anyhow::Result<HardwareBootResult> {
    let devices = std::sync::Arc::new(tokio::sync::RwLock::new(DeviceRegistry::new()));
    let registry = ToolRegistry::load(devices.clone()).await?;
    let device_summary = {
        let reg = devices.read().await;
        reg.prompt_summary()
    };
    let mut tools = registry.into_tools();
    if !tools.is_empty() {
        tracing::info!(
            count = tools.len(),
            "Hardware registry tools loaded (plugins only)"
        );
    }
    // No discovered devices in no-hardware fallback; still load global files.
    let mut context_files_prompt = load_hardware_context_prompt(&[]);
    // RPi self-discovery: detect board model and inject GPIO tools + prompt context.
    #[cfg(all(feature = "peripheral-rpi", target_os = "linux"))]
    inject_rpi_context(&mut tools, &mut context_files_prompt);
    Ok(HardwareBootResult {
        tools,
        device_summary,
        context_files_prompt,
    })
}

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
    // USB/serial discovery is behind the "hardware" feature gate and only
    // available on platforms where nusb supports device enumeration.
    #[cfg(all(
        feature = "hardware",
        any(target_os = "linux", target_os = "macos", target_os = "windows")
    ))]
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
        let _ = &cmd;
        println!("Hardware discovery requires the 'hardware' feature.");
        println!("Build with: cargo build --features hardware");
        Ok(())
    }

    #[cfg(all(
        feature = "hardware",
        not(any(target_os = "linux", target_os = "macos", target_os = "windows"))
    ))]
    {
        let _ = &cmd;
        println!("Hardware USB discovery is not supported on this platform.");
        println!("Supported platforms: Linux, macOS, Windows.");
        return Ok(());
    }

    #[cfg(all(
        feature = "hardware",
        any(target_os = "linux", target_os = "macos", target_os = "windows")
    ))]
    match cmd {
        crate::HardwareCommands::Discover => run_discover(),
        crate::HardwareCommands::Introspect { path } => run_introspect(&path),
        crate::HardwareCommands::Info { chip } => run_info(&chip),
    }
}

#[cfg(all(
    feature = "hardware",
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
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

#[cfg(all(
    feature = "hardware",
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
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

#[cfg(all(
    feature = "hardware",
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
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

#[cfg(all(
    feature = "hardware",
    feature = "probe",
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
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

#[cfg(test)]
mod tests {
    use super::load_hardware_context_from_dir;
    use std::fs;

    fn write(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn empty_dir_returns_empty_string() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load_hardware_context_from_dir(tmp.path(), &[]), "");
    }

    #[test]
    fn hardware_md_only_returns_its_content() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("HARDWARE.md"), "# Global HW\npin 25 = LED");
        let result = load_hardware_context_from_dir(tmp.path(), &[]);
        assert!(result.contains("pin 25 = LED"), "got: {result}");
    }

    #[test]
    fn device_profile_loaded_for_matching_alias() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("devices").join("pico0.md"),
            "# pico0\nPort: /dev/cu.usbmodem1101",
        );
        let result = load_hardware_context_from_dir(tmp.path(), &["pico0"]);
        assert!(result.contains("/dev/cu.usbmodem1101"), "got: {result}");
    }

    #[test]
    fn device_profile_skipped_for_non_matching_alias() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("devices").join("pico0.md"),
            "# pico0\nPort: /dev/cu.usbmodem1101",
        );
        // No alias provided — device profile must not appear
        let result = load_hardware_context_from_dir(tmp.path(), &[]);
        assert!(!result.contains("pico0"), "got: {result}");
    }

    #[test]
    fn skills_loaded_and_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("skills").join("blink.md"),
            "# Skill: Blink\nuse device_exec",
        );
        write(
            &tmp.path().join("skills").join("gpio.md"),
            "# Skill: GPIO\ngpio_write",
        );
        let result = load_hardware_context_from_dir(tmp.path(), &[]);
        // blink.md sorts before gpio.md
        let blink_pos = result.find("device_exec").unwrap();
        let gpio_pos = result.find("gpio_write").unwrap();
        assert!(blink_pos < gpio_pos, "skills not sorted; got: {result}");
    }

    #[test]
    fn sections_joined_with_double_newline() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("HARDWARE.md"), "global");
        write(&tmp.path().join("devices").join("pico0.md"), "device");
        let result = load_hardware_context_from_dir(tmp.path(), &["pico0"]);
        assert!(result.contains("global\n\ndevice"), "got: {result}");
    }

    #[test]
    fn hardware_context_contains_device_exec_rule() {
        // Verify that the installed HARDWARE.md (from Section 3) contains
        // the device_exec rule so the LLM knows to use it for blink/loops.
        // This acts as the Section 5 BUG-2 behavioral gate.
        if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
            let hw_md = home.join(".zeroclaw").join("hardware").join("HARDWARE.md");
            if hw_md.exists() {
                let content = fs::read_to_string(&hw_md).unwrap_or_default();
                assert!(
                    content.contains("device_exec"),
                    "HARDWARE.md must mention device_exec for blink/loop operations; got: {content}"
                );
            }
        }
    }
}
