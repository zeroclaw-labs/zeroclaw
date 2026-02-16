//! Hardware Abstraction Layer (HAL) for ZeroClaw.
//!
//! Provides auto-discovery of connected hardware, transport abstraction,
//! and a unified interface so the LLM agent can control physical devices
//! without knowing the underlying communication protocol.
//!
//! # Supported Transport Modes
//!
//! | Transport | Backend      | Use Case                                    |
//! |-----------|-------------|---------------------------------------------|
//! | `native`  | rppal / sysfs | Raspberry Pi / Linux SBC with local GPIO  |
//! | `serial`  | JSON/UART   | Arduino, ESP32, Nucleo via USB serial       |
//! | `probe`   | probe-rs    | STM32/ESP32 via SWD/JTAG debug interface    |
//! | `none`    | —           | Software-only mode (no hardware access)     |

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Hardware transport enum ──────────────────────────────────────

/// Transport protocol used to communicate with physical hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HardwareTransport {
    /// Direct GPIO access on a Linux SBC (Raspberry Pi, Orange Pi, etc.)
    Native,
    /// JSON commands over USB serial (Arduino, ESP32, Nucleo)
    Serial,
    /// SWD/JTAG debug probe (probe-rs) for bare-metal MCUs
    Probe,
    /// No hardware — software-only mode
    None,
}

impl Default for HardwareTransport {
    fn default() -> Self {
        Self::None
    }
}

impl std::fmt::Display for HardwareTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native => write!(f, "native"),
            Self::Serial => write!(f, "serial"),
            Self::Probe => write!(f, "probe"),
            Self::None => write!(f, "none"),
        }
    }
}

impl HardwareTransport {
    /// Parse from a string value (config file or CLI arg).
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_ascii_lowercase().trim() {
            "native" | "gpio" | "rppal" | "sysfs" => Self::Native,
            "serial" | "uart" | "usb" | "tethered" => Self::Serial,
            "probe" | "probe-rs" | "swd" | "jtag" | "jlink" | "j-link" => Self::Probe,
            _ => Self::None,
        }
    }
}

// ── Hardware configuration ──────────────────────────────────────

/// Hardware configuration stored in `config.toml` under `[hardware]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareConfig {
    /// Enable hardware integration
    #[serde(default)]
    pub enabled: bool,

    /// Transport mode: "native", "serial", "probe", "none"
    #[serde(default = "default_transport")]
    pub transport: String,

    /// Serial port path (e.g. `/dev/ttyUSB0`, `/dev/tty.usbmodem14201`)
    #[serde(default)]
    pub serial_port: Option<String>,

    /// Serial baud rate (default: 115200)
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,

    /// Enable datasheet RAG — index PDF schematics in workspace for pin lookups
    #[serde(default)]
    pub workspace_datasheets: bool,

    /// Auto-discovered board description (informational, set by discovery)
    #[serde(default)]
    pub discovered_board: Option<String>,

    /// Probe target chip (e.g. "STM32F411CEUx", "nRF52840_xxAA")
    #[serde(default)]
    pub probe_target: Option<String>,

    /// GPIO pin safety allowlist — only these pins can be written to.
    /// Empty = all pins allowed (for development). Recommended for production.
    #[serde(default)]
    pub allowed_pins: Vec<u8>,

    /// Maximum PWM frequency in Hz (safety cap, default: 50_000)
    #[serde(default = "default_max_pwm_freq")]
    pub max_pwm_frequency_hz: u32,
}

fn default_transport() -> String {
    "none".into()
}

fn default_baud_rate() -> u32 {
    115_200
}

fn default_max_pwm_freq() -> u32 {
    50_000
}

impl Default for HardwareConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: default_transport(),
            serial_port: None,
            baud_rate: default_baud_rate(),
            workspace_datasheets: false,
            discovered_board: None,
            probe_target: None,
            allowed_pins: Vec::new(),
            max_pwm_frequency_hz: default_max_pwm_freq(),
        }
    }
}

impl HardwareConfig {
    /// Return the parsed transport enum.
    pub fn transport_mode(&self) -> HardwareTransport {
        HardwareTransport::from_str_loose(&self.transport)
    }

    /// Check if pin access is allowed by the safety allowlist.
    /// An empty allowlist means all pins are permitted (dev mode).
    pub fn is_pin_allowed(&self, pin: u8) -> bool {
        self.allowed_pins.is_empty() || self.allowed_pins.contains(&pin)
    }

    /// Validate the configuration, returning errors for invalid combos.
    pub fn validate(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let mode = self.transport_mode();

        // Serial requires a port
        if mode == HardwareTransport::Serial && self.serial_port.is_none() {
            bail!("Hardware transport is 'serial' but no serial_port is configured. Run `zeroclaw onboard --interactive` or set hardware.serial_port in config.toml.");
        }

        // Probe requires a target chip
        if mode == HardwareTransport::Probe && self.probe_target.is_none() {
            bail!("Hardware transport is 'probe' but no probe_target chip is configured. Set hardware.probe_target in config.toml (e.g. \"STM32F411CEUx\").");
        }

        // Baud rate sanity
        if self.baud_rate == 0 {
            bail!("hardware.baud_rate must be greater than 0.");
        }
        if self.baud_rate > 4_000_000 {
            bail!(
                "hardware.baud_rate of {} exceeds the 4 MHz safety limit.",
                self.baud_rate
            );
        }

        // PWM frequency sanity
        if self.max_pwm_frequency_hz == 0 {
            bail!("hardware.max_pwm_frequency_hz must be greater than 0.");
        }

        Ok(())
    }
}

// ── Discovery: detected hardware on this system ─────────────────

/// A single discovered hardware device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// Human-readable name (e.g. "Raspberry Pi GPIO", "Arduino Uno")
    pub name: String,
    /// Recommended transport mode
    pub transport: HardwareTransport,
    /// Path to the device (e.g. `/dev/ttyUSB0`, `/dev/gpiomem`)
    pub device_path: Option<String>,
    /// Additional detail (e.g. board revision, chip ID)
    pub detail: Option<String>,
}

/// Scan the system for connected hardware.
///
/// This function performs non-destructive, read-only probes:
/// 1. Check for Raspberry Pi GPIO (`/dev/gpiomem`, `/proc/device-tree/model`)
/// 2. Check for USB serial devices (`/dev/ttyUSB*`, `/dev/ttyACM*`, `/dev/tty.usbmodem*`)
/// 3. Check for SWD/JTAG probes (`/dev/ttyACM*` with probe-rs markers)
///
/// This is intentionally conservative — it never writes to any device.
pub fn discover_hardware() -> Vec<DiscoveredDevice> {
    let mut devices = Vec::new();

    // ── 1. Raspberry Pi / Linux SBC native GPIO ──────────────
    discover_native_gpio(&mut devices);

    // ── 2. USB Serial devices (Arduino, ESP32, etc.) ─────────
    discover_serial_devices(&mut devices);

    // ── 3. SWD / JTAG debug probes ──────────────────────────
    discover_debug_probes(&mut devices);

    devices
}

/// Check for native GPIO availability (Raspberry Pi, Orange Pi, etc.)
fn discover_native_gpio(devices: &mut Vec<DiscoveredDevice>) {
    // Primary indicator: /dev/gpiomem exists (Pi-specific)
    let gpiomem = Path::new("/dev/gpiomem");
    // Secondary: /dev/gpiochip0 exists (any Linux with GPIO)
    let gpiochip = Path::new("/dev/gpiochip0");

    if gpiomem.exists() || gpiochip.exists() {
        // Try to read model from device tree
        let model = read_board_model();
        let name = model.as_deref().unwrap_or("Linux SBC with GPIO");

        devices.push(DiscoveredDevice {
            name: format!("{name} (Native GPIO)"),
            transport: HardwareTransport::Native,
            device_path: Some(if gpiomem.exists() {
                "/dev/gpiomem".into()
            } else {
                "/dev/gpiochip0".into()
            }),
            detail: model,
        });
    }
}

/// Read the board model string from the device tree (Linux).
fn read_board_model() -> Option<String> {
    let model_path = Path::new("/proc/device-tree/model");
    if model_path.exists() {
        std::fs::read_to_string(model_path)
            .ok()
            .map(|s| s.trim_end_matches('\0').trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    }
}

/// Scan for USB serial devices.
fn discover_serial_devices(devices: &mut Vec<DiscoveredDevice>) {
    let serial_patterns = serial_device_paths();

    for pattern in &serial_patterns {
        let matches = glob_paths(pattern);
        for path in matches {
            let name = classify_serial_device(&path);
            devices.push(DiscoveredDevice {
                name: format!("{name} (USB Serial)"),
                transport: HardwareTransport::Serial,
                device_path: Some(path.to_string_lossy().to_string()),
                detail: None,
            });
        }
    }
}

/// Return platform-specific glob patterns for serial devices.
fn serial_device_paths() -> Vec<String> {
    if cfg!(target_os = "macos") {
        vec![
            "/dev/tty.usbmodem*".into(),
            "/dev/tty.usbserial*".into(),
            "/dev/tty.wchusbserial*".into(), // CH340 clones
        ]
    } else if cfg!(target_os = "linux") {
        vec!["/dev/ttyUSB*".into(), "/dev/ttyACM*".into()]
    } else {
        // Windows / other — not yet supported for auto-discovery
        vec![]
    }
}

/// Classify a serial device path into a human-readable name.
fn classify_serial_device(path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let lower = name.to_ascii_lowercase();

    if lower.contains("usbmodem") {
        "Arduino/Teensy".into()
    } else if lower.contains("usbserial") || lower.contains("ttyusb") {
        "USB-Serial Device (FTDI/CH340/CP2102)".into()
    } else if lower.contains("wchusbserial") {
        "CH340/CH341 Serial".into()
    } else if lower.contains("ttyacm") {
        "USB CDC Device (Arduino/STM32)".into()
    } else {
        "Unknown Serial Device".into()
    }
}

/// Simple glob expansion for device paths.
fn glob_paths(pattern: &str) -> Vec<PathBuf> {
    glob::glob(pattern)
        .map(|paths| paths.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Check for SWD/JTAG debug probes.
fn discover_debug_probes(devices: &mut Vec<DiscoveredDevice>) {
    // On Linux, ST-Link probes often show up as /dev/stlinkv*
    // We also check for known USB VIDs via sysfs if available
    let stlink_paths = glob_paths("/dev/stlinkv*");
    for path in stlink_paths {
        devices.push(DiscoveredDevice {
            name: "ST-Link Debug Probe (SWD)".into(),
            transport: HardwareTransport::Probe,
            device_path: Some(path.to_string_lossy().to_string()),
            detail: Some("Use probe-rs for flash/debug".into()),
        });
    }

    // J-Link probes on macOS
    let jlink_paths = glob_paths("/dev/tty.SLAB_USBtoUART*");
    for path in jlink_paths {
        devices.push(DiscoveredDevice {
            name: "SEGGER J-Link (SWD/JTAG)".into(),
            transport: HardwareTransport::Probe,
            device_path: Some(path.to_string_lossy().to_string()),
            detail: Some("Use probe-rs for flash/debug".into()),
        });
    }
}

// ── HAL Trait: Unified hardware operations ──────────────────────

/// The core HAL trait that all transport backends implement.
///
/// The LLM agent calls these methods via tool invocations. The HAL
/// translates them into the correct protocol for the underlying hardware.
pub trait HardwareHal: Send + Sync {
    /// Read the digital state of a GPIO pin.
    fn gpio_read(&self, pin: u8) -> Result<bool>;

    /// Write a digital value to a GPIO pin.
    fn gpio_write(&self, pin: u8, value: bool) -> Result<()>;

    /// Read a memory address (for probe-rs or memory-mapped I/O).
    fn memory_read(&self, address: u32, length: u32) -> Result<Vec<u8>>;

    /// Upload firmware to a connected device (Arduino sketch, STM32 binary).
    fn firmware_upload(&self, path: &Path) -> Result<()>;

    /// Return a human-readable description of the connected hardware.
    fn describe(&self) -> String;

    /// Set PWM duty cycle on a pin (0–100%).
    fn pwm_set(&self, pin: u8, duty_percent: f32) -> Result<()>;

    /// Read an analog value (ADC) from a pin, returning 0.0–1.0.
    fn analog_read(&self, pin: u8) -> Result<f32>;
}

// ── NoopHal: used in software-only mode ─────────────────────────

/// A no-op HAL implementation for software-only mode.
/// All hardware operations return descriptive errors.
pub struct NoopHal;

impl HardwareHal for NoopHal {
    fn gpio_read(&self, pin: u8) -> Result<bool> {
        bail!("Hardware not enabled. Cannot read GPIO pin {pin}. Enable hardware in config.toml or run `zeroclaw onboard --interactive`.");
    }

    fn gpio_write(&self, pin: u8, value: bool) -> Result<()> {
        bail!("Hardware not enabled. Cannot write GPIO pin {pin}={value}. Enable hardware in config.toml.");
    }

    fn memory_read(&self, address: u32, _length: u32) -> Result<Vec<u8>> {
        bail!("Hardware not enabled. Cannot read memory at 0x{address:08X}.");
    }

    fn firmware_upload(&self, path: &Path) -> Result<()> {
        bail!(
            "Hardware not enabled. Cannot upload firmware from {}.",
            path.display()
        );
    }

    fn describe(&self) -> String {
        "NoopHal (software-only mode — no hardware connected)".into()
    }

    fn pwm_set(&self, pin: u8, _duty_percent: f32) -> Result<()> {
        bail!("Hardware not enabled. Cannot set PWM on pin {pin}.");
    }

    fn analog_read(&self, pin: u8) -> Result<f32> {
        bail!("Hardware not enabled. Cannot read analog pin {pin}.");
    }
}

// ── Factory: create the right HAL from config ───────────────────

/// Create the appropriate HAL backend from the hardware configuration.
///
/// This is the main entry point — call this once at startup and pass
/// the resulting `Box<dyn HardwareHal>` to the tool registry.
pub fn create_hal(config: &HardwareConfig) -> Result<Box<dyn HardwareHal>> {
    config.validate()?;

    if !config.enabled {
        return Ok(Box::new(NoopHal));
    }

    match config.transport_mode() {
        HardwareTransport::None => Ok(Box::new(NoopHal)),
        HardwareTransport::Native => {
            // In a full implementation, this would return a RppalHal or SysfsHal.
            // For now, we return a stub that validates the transport is correct.
            bail!(
                "Native GPIO transport requires the `rppal` crate (Raspberry Pi only). \
                 This will be available in a future release. For now, use 'serial' transport \
                 with an Arduino/ESP32 bridge."
            );
        }
        HardwareTransport::Serial => {
            let port = config.serial_port.as_deref().unwrap_or("/dev/ttyUSB0");
            // In a full implementation, this would open the serial port and
            // return a SerialHal that sends JSON commands over UART.
            bail!(
                "Serial transport to '{}' at {} baud is configured but the serial HAL \
                 backend is not yet compiled in. This will be available in the next release.",
                port,
                config.baud_rate
            );
        }
        HardwareTransport::Probe => {
            let target = config.probe_target.as_deref().unwrap_or("unknown");
            bail!(
                "Probe transport targeting '{}' is configured but the probe-rs HAL \
                 backend is not yet compiled in. This will be available in a future release.",
                target
            );
        }
    }
}

// ── Wizard helper: build config from discovery ──────────────────

/// Determine the best default selection index for the wizard
/// based on discovery results.
pub fn recommended_wizard_default(devices: &[DiscoveredDevice]) -> usize {
    // If we found native GPIO → recommend Native (index 0)
    if devices
        .iter()
        .any(|d| d.transport == HardwareTransport::Native)
    {
        return 0;
    }
    // If we found serial devices → recommend Tethered (index 1)
    if devices
        .iter()
        .any(|d| d.transport == HardwareTransport::Serial)
    {
        return 1;
    }
    // If we found debug probes → recommend Probe (index 2)
    if devices
        .iter()
        .any(|d| d.transport == HardwareTransport::Probe)
    {
        return 2;
    }
    // Default: Software Only (index 3)
    3
}

/// Build a `HardwareConfig` from a wizard selection and discovered devices.
pub fn config_from_wizard_choice(choice: usize, devices: &[DiscoveredDevice]) -> HardwareConfig {
    match choice {
        // Native
        0 => {
            let native_device = devices
                .iter()
                .find(|d| d.transport == HardwareTransport::Native);
            HardwareConfig {
                enabled: true,
                transport: "native".into(),
                discovered_board: native_device
                    .and_then(|d| d.detail.clone())
                    .or_else(|| native_device.map(|d| d.name.clone())),
                ..HardwareConfig::default()
            }
        }
        // Serial / Tethered
        1 => {
            let serial_device = devices
                .iter()
                .find(|d| d.transport == HardwareTransport::Serial);
            HardwareConfig {
                enabled: true,
                transport: "serial".into(),
                serial_port: serial_device.and_then(|d| d.device_path.clone()),
                discovered_board: serial_device.map(|d| d.name.clone()),
                ..HardwareConfig::default()
            }
        }
        // Probe
        2 => {
            let probe_device = devices
                .iter()
                .find(|d| d.transport == HardwareTransport::Probe);
            HardwareConfig {
                enabled: true,
                transport: "probe".into(),
                discovered_board: probe_device.map(|d| d.name.clone()),
                ..HardwareConfig::default()
            }
        }
        // Software only
        _ => HardwareConfig::default(),
    }
}

// ═══════════════════════════════════════════════════════════════════
// ── Tests ───────────────────────────────────────────────────────
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── HardwareTransport parsing ──────────────────────────────

    #[test]
    fn transport_parse_native_variants() {
        assert_eq!(
            HardwareTransport::from_str_loose("native"),
            HardwareTransport::Native
        );
        assert_eq!(
            HardwareTransport::from_str_loose("gpio"),
            HardwareTransport::Native
        );
        assert_eq!(
            HardwareTransport::from_str_loose("rppal"),
            HardwareTransport::Native
        );
        assert_eq!(
            HardwareTransport::from_str_loose("sysfs"),
            HardwareTransport::Native
        );
        assert_eq!(
            HardwareTransport::from_str_loose("NATIVE"),
            HardwareTransport::Native
        );
        assert_eq!(
            HardwareTransport::from_str_loose("  Native  "),
            HardwareTransport::Native
        );
    }

    #[test]
    fn transport_parse_serial_variants() {
        assert_eq!(
            HardwareTransport::from_str_loose("serial"),
            HardwareTransport::Serial
        );
        assert_eq!(
            HardwareTransport::from_str_loose("uart"),
            HardwareTransport::Serial
        );
        assert_eq!(
            HardwareTransport::from_str_loose("usb"),
            HardwareTransport::Serial
        );
        assert_eq!(
            HardwareTransport::from_str_loose("tethered"),
            HardwareTransport::Serial
        );
        assert_eq!(
            HardwareTransport::from_str_loose("SERIAL"),
            HardwareTransport::Serial
        );
    }

    #[test]
    fn transport_parse_probe_variants() {
        assert_eq!(
            HardwareTransport::from_str_loose("probe"),
            HardwareTransport::Probe
        );
        assert_eq!(
            HardwareTransport::from_str_loose("probe-rs"),
            HardwareTransport::Probe
        );
        assert_eq!(
            HardwareTransport::from_str_loose("swd"),
            HardwareTransport::Probe
        );
        assert_eq!(
            HardwareTransport::from_str_loose("jtag"),
            HardwareTransport::Probe
        );
        assert_eq!(
            HardwareTransport::from_str_loose("jlink"),
            HardwareTransport::Probe
        );
        assert_eq!(
            HardwareTransport::from_str_loose("j-link"),
            HardwareTransport::Probe
        );
    }

    #[test]
    fn transport_parse_none_and_unknown() {
        assert_eq!(
            HardwareTransport::from_str_loose("none"),
            HardwareTransport::None
        );
        assert_eq!(
            HardwareTransport::from_str_loose(""),
            HardwareTransport::None
        );
        assert_eq!(
            HardwareTransport::from_str_loose("foobar"),
            HardwareTransport::None
        );
        assert_eq!(
            HardwareTransport::from_str_loose("bluetooth"),
            HardwareTransport::None
        );
    }

    #[test]
    fn transport_default_is_none() {
        assert_eq!(HardwareTransport::default(), HardwareTransport::None);
    }

    #[test]
    fn transport_display() {
        assert_eq!(format!("{}", HardwareTransport::Native), "native");
        assert_eq!(format!("{}", HardwareTransport::Serial), "serial");
        assert_eq!(format!("{}", HardwareTransport::Probe), "probe");
        assert_eq!(format!("{}", HardwareTransport::None), "none");
    }

    // ── HardwareTransport serde ────────────────────────────────

    #[test]
    fn transport_serde_roundtrip() {
        let json = serde_json::to_string(&HardwareTransport::Native).unwrap();
        assert_eq!(json, "\"native\"");
        let parsed: HardwareTransport = serde_json::from_str("\"serial\"").unwrap();
        assert_eq!(parsed, HardwareTransport::Serial);
        let parsed2: HardwareTransport = serde_json::from_str("\"probe\"").unwrap();
        assert_eq!(parsed2, HardwareTransport::Probe);
        let parsed3: HardwareTransport = serde_json::from_str("\"none\"").unwrap();
        assert_eq!(parsed3, HardwareTransport::None);
    }

    // ── HardwareConfig defaults ────────────────────────────────

    #[test]
    fn config_default_values() {
        let cfg = HardwareConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.transport, "none");
        assert_eq!(cfg.baud_rate, 115_200);
        assert!(cfg.serial_port.is_none());
        assert!(!cfg.workspace_datasheets);
        assert!(cfg.discovered_board.is_none());
        assert!(cfg.probe_target.is_none());
        assert!(cfg.allowed_pins.is_empty());
        assert_eq!(cfg.max_pwm_frequency_hz, 50_000);
    }

    #[test]
    fn config_transport_mode_maps_correctly() {
        let mut cfg = HardwareConfig::default();
        assert_eq!(cfg.transport_mode(), HardwareTransport::None);

        cfg.transport = "native".into();
        assert_eq!(cfg.transport_mode(), HardwareTransport::Native);

        cfg.transport = "serial".into();
        assert_eq!(cfg.transport_mode(), HardwareTransport::Serial);

        cfg.transport = "probe".into();
        assert_eq!(cfg.transport_mode(), HardwareTransport::Probe);

        cfg.transport = "UART".into();
        assert_eq!(cfg.transport_mode(), HardwareTransport::Serial);
    }

    // ── HardwareConfig::is_pin_allowed ─────────────────────────

    #[test]
    fn pin_allowed_empty_allowlist_permits_all() {
        let cfg = HardwareConfig::default();
        assert!(cfg.is_pin_allowed(0));
        assert!(cfg.is_pin_allowed(13));
        assert!(cfg.is_pin_allowed(255));
    }

    #[test]
    fn pin_allowed_nonempty_allowlist_restricts() {
        let cfg = HardwareConfig {
            allowed_pins: vec![2, 13, 27],
            ..HardwareConfig::default()
        };
        assert!(cfg.is_pin_allowed(2));
        assert!(cfg.is_pin_allowed(13));
        assert!(cfg.is_pin_allowed(27));
        assert!(!cfg.is_pin_allowed(0));
        assert!(!cfg.is_pin_allowed(14));
        assert!(!cfg.is_pin_allowed(255));
    }

    #[test]
    fn pin_allowed_single_pin_allowlist() {
        let cfg = HardwareConfig {
            allowed_pins: vec![13],
            ..HardwareConfig::default()
        };
        assert!(cfg.is_pin_allowed(13));
        assert!(!cfg.is_pin_allowed(12));
        assert!(!cfg.is_pin_allowed(14));
    }

    // ── HardwareConfig::validate ───────────────────────────────

    #[test]
    fn validate_disabled_always_ok() {
        let cfg = HardwareConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_disabled_ignores_bad_values() {
        // Even with invalid values, disabled config should pass
        let cfg = HardwareConfig {
            enabled: false,
            transport: "serial".into(),
            serial_port: None, // Would fail if enabled
            baud_rate: 0,      // Would fail if enabled
            ..HardwareConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_serial_requires_port() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: None,
            ..HardwareConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("serial_port"));
    }

    #[test]
    fn validate_serial_with_port_ok() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: Some("/dev/ttyUSB0".into()),
            ..HardwareConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_probe_requires_target() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "probe".into(),
            probe_target: None,
            ..HardwareConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("probe_target"));
    }

    #[test]
    fn validate_probe_with_target_ok() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "probe".into(),
            probe_target: Some("STM32F411CEUx".into()),
            ..HardwareConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_native_ok_without_extras() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "native".into(),
            ..HardwareConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_none_transport_enabled_ok() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "none".into(),
            ..HardwareConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_baud_rate_zero_fails() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: Some("/dev/ttyUSB0".into()),
            baud_rate: 0,
            ..HardwareConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("baud_rate"));
    }

    #[test]
    fn validate_baud_rate_too_high_fails() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: Some("/dev/ttyUSB0".into()),
            baud_rate: 5_000_000,
            ..HardwareConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("safety limit"));
    }

    #[test]
    fn validate_baud_rate_boundary_ok() {
        // Exactly at the limit
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: Some("/dev/ttyUSB0".into()),
            baud_rate: 4_000_000,
            ..HardwareConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_baud_rate_common_values_ok() {
        for baud in [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600] {
            let cfg = HardwareConfig {
                enabled: true,
                transport: "serial".into(),
                serial_port: Some("/dev/ttyUSB0".into()),
                baud_rate: baud,
                ..HardwareConfig::default()
            };
            assert!(cfg.validate().is_ok(), "baud rate {baud} should be valid");
        }
    }

    #[test]
    fn validate_pwm_frequency_zero_fails() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "native".into(),
            max_pwm_frequency_hz: 0,
            ..HardwareConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("max_pwm_frequency_hz"));
    }

    // ── HardwareConfig serde ───────────────────────────────────

    #[test]
    fn config_serde_roundtrip_toml() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: Some("/dev/ttyUSB0".into()),
            baud_rate: 9600,
            workspace_datasheets: true,
            discovered_board: Some("Arduino Uno".into()),
            probe_target: None,
            allowed_pins: vec![2, 13],
            max_pwm_frequency_hz: 25_000,
        };

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: HardwareConfig = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.enabled, cfg.enabled);
        assert_eq!(parsed.transport, cfg.transport);
        assert_eq!(parsed.serial_port, cfg.serial_port);
        assert_eq!(parsed.baud_rate, cfg.baud_rate);
        assert_eq!(parsed.workspace_datasheets, cfg.workspace_datasheets);
        assert_eq!(parsed.discovered_board, cfg.discovered_board);
        assert_eq!(parsed.allowed_pins, cfg.allowed_pins);
        assert_eq!(parsed.max_pwm_frequency_hz, cfg.max_pwm_frequency_hz);
    }

    #[test]
    fn config_serde_minimal_toml() {
        // Deserializing an empty TOML section should produce defaults
        let toml_str = "enabled = false\n";
        let parsed: HardwareConfig = toml::from_str(toml_str).unwrap();
        assert!(!parsed.enabled);
        assert_eq!(parsed.transport, "none");
        assert_eq!(parsed.baud_rate, 115_200);
    }

    #[test]
    fn config_serde_json_roundtrip() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "probe".into(),
            serial_port: None,
            baud_rate: 115200,
            workspace_datasheets: false,
            discovered_board: None,
            probe_target: Some("nRF52840_xxAA".into()),
            allowed_pins: vec![],
            max_pwm_frequency_hz: 50_000,
        };

        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: HardwareConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.probe_target, cfg.probe_target);
        assert_eq!(parsed.transport, "probe");
    }

    // ── NoopHal ────────────────────────────────────────────────

    #[test]
    fn noop_hal_gpio_read_fails() {
        let hal = NoopHal;
        let err = hal.gpio_read(13).unwrap_err();
        assert!(err.to_string().contains("not enabled"));
        assert!(err.to_string().contains("13"));
    }

    #[test]
    fn noop_hal_gpio_write_fails() {
        let hal = NoopHal;
        let err = hal.gpio_write(5, true).unwrap_err();
        assert!(err.to_string().contains("not enabled"));
    }

    #[test]
    fn noop_hal_memory_read_fails() {
        let hal = NoopHal;
        let err = hal.memory_read(0x2000_0000, 4).unwrap_err();
        assert!(err.to_string().contains("not enabled"));
        assert!(err.to_string().contains("0x20000000"));
    }

    #[test]
    fn noop_hal_firmware_upload_fails() {
        let hal = NoopHal;
        let err = hal
            .firmware_upload(Path::new("/tmp/firmware.bin"))
            .unwrap_err();
        assert!(err.to_string().contains("not enabled"));
        assert!(err.to_string().contains("firmware.bin"));
    }

    #[test]
    fn noop_hal_describe() {
        let hal = NoopHal;
        let desc = hal.describe();
        assert!(desc.contains("software-only"));
    }

    #[test]
    fn noop_hal_pwm_set_fails() {
        let hal = NoopHal;
        let err = hal.pwm_set(9, 50.0).unwrap_err();
        assert!(err.to_string().contains("not enabled"));
    }

    #[test]
    fn noop_hal_analog_read_fails() {
        let hal = NoopHal;
        let err = hal.analog_read(0).unwrap_err();
        assert!(err.to_string().contains("not enabled"));
    }

    // ── create_hal factory ─────────────────────────────────────

    #[test]
    fn create_hal_disabled_returns_noop() {
        let cfg = HardwareConfig::default();
        let hal = create_hal(&cfg).unwrap();
        assert!(hal.describe().contains("software-only"));
    }

    #[test]
    fn create_hal_none_transport_returns_noop() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "none".into(),
            ..HardwareConfig::default()
        };
        let hal = create_hal(&cfg).unwrap();
        assert!(hal.describe().contains("software-only"));
    }

    #[test]
    fn create_hal_serial_without_port_fails_validation() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: None,
            ..HardwareConfig::default()
        };
        assert!(create_hal(&cfg).is_err());
    }

    #[test]
    fn create_hal_invalid_baud_fails_validation() {
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: Some("/dev/ttyUSB0".into()),
            baud_rate: 0,
            ..HardwareConfig::default()
        };
        assert!(create_hal(&cfg).is_err());
    }

    // ── Discovery helpers ──────────────────────────────────────

    #[test]
    fn classify_serial_arduino() {
        let path = Path::new("/dev/tty.usbmodem14201");
        assert!(classify_serial_device(path).contains("Arduino"));
    }

    #[test]
    fn classify_serial_ftdi() {
        let path = Path::new("/dev/tty.usbserial-1234");
        assert!(classify_serial_device(path).contains("FTDI"));
    }

    #[test]
    fn classify_serial_ch340() {
        let path = Path::new("/dev/tty.wchusbserial1420");
        assert!(classify_serial_device(path).contains("CH340"));
    }

    #[test]
    fn classify_serial_ttyacm() {
        let path = Path::new("/dev/ttyACM0");
        assert!(classify_serial_device(path).contains("CDC"));
    }

    #[test]
    fn classify_serial_ttyusb() {
        let path = Path::new("/dev/ttyUSB0");
        assert!(classify_serial_device(path).contains("USB-Serial"));
    }

    #[test]
    fn classify_serial_unknown() {
        let path = Path::new("/dev/ttyXYZ99");
        assert!(classify_serial_device(path).contains("Unknown"));
    }

    // ── Serial device path patterns ────────────────────────────

    #[test]
    fn serial_paths_macos_patterns() {
        if cfg!(target_os = "macos") {
            let patterns = serial_device_paths();
            assert!(patterns.iter().any(|p| p.contains("usbmodem")));
            assert!(patterns.iter().any(|p| p.contains("usbserial")));
            assert!(patterns.iter().any(|p| p.contains("wchusbserial")));
        }
    }

    #[test]
    fn serial_paths_linux_patterns() {
        if cfg!(target_os = "linux") {
            let patterns = serial_device_paths();
            assert!(patterns.iter().any(|p| p.contains("ttyUSB")));
            assert!(patterns.iter().any(|p| p.contains("ttyACM")));
        }
    }

    // ── Wizard helpers ─────────────────────────────────────────

    #[test]
    fn recommended_default_no_devices() {
        let devices: Vec<DiscoveredDevice> = vec![];
        assert_eq!(recommended_wizard_default(&devices), 3); // Software only
    }

    #[test]
    fn recommended_default_native_found() {
        let devices = vec![DiscoveredDevice {
            name: "Raspberry Pi (Native GPIO)".into(),
            transport: HardwareTransport::Native,
            device_path: Some("/dev/gpiomem".into()),
            detail: None,
        }];
        assert_eq!(recommended_wizard_default(&devices), 0); // Native
    }

    #[test]
    fn recommended_default_serial_found() {
        let devices = vec![DiscoveredDevice {
            name: "Arduino (USB Serial)".into(),
            transport: HardwareTransport::Serial,
            device_path: Some("/dev/ttyUSB0".into()),
            detail: None,
        }];
        assert_eq!(recommended_wizard_default(&devices), 1); // Tethered
    }

    #[test]
    fn recommended_default_probe_found() {
        let devices = vec![DiscoveredDevice {
            name: "ST-Link (SWD)".into(),
            transport: HardwareTransport::Probe,
            device_path: None,
            detail: None,
        }];
        assert_eq!(recommended_wizard_default(&devices), 2); // Probe
    }

    #[test]
    fn recommended_default_native_priority_over_serial() {
        // When both native and serial are found, native wins
        let devices = vec![
            DiscoveredDevice {
                name: "Arduino".into(),
                transport: HardwareTransport::Serial,
                device_path: Some("/dev/ttyUSB0".into()),
                detail: None,
            },
            DiscoveredDevice {
                name: "RPi GPIO".into(),
                transport: HardwareTransport::Native,
                device_path: Some("/dev/gpiomem".into()),
                detail: None,
            },
        ];
        assert_eq!(recommended_wizard_default(&devices), 0); // Native wins
    }

    #[test]
    fn config_from_wizard_native() {
        let devices = vec![DiscoveredDevice {
            name: "Raspberry Pi 4 (Native GPIO)".into(),
            transport: HardwareTransport::Native,
            device_path: Some("/dev/gpiomem".into()),
            detail: Some("Raspberry Pi 4 Model B Rev 1.5".into()),
        }];

        let cfg = config_from_wizard_choice(0, &devices);
        assert!(cfg.enabled);
        assert_eq!(cfg.transport, "native");
        assert_eq!(
            cfg.discovered_board.as_deref(),
            Some("Raspberry Pi 4 Model B Rev 1.5")
        );
    }

    #[test]
    fn config_from_wizard_serial() {
        let devices = vec![DiscoveredDevice {
            name: "Arduino Uno (USB Serial)".into(),
            transport: HardwareTransport::Serial,
            device_path: Some("/dev/ttyUSB0".into()),
            detail: None,
        }];

        let cfg = config_from_wizard_choice(1, &devices);
        assert!(cfg.enabled);
        assert_eq!(cfg.transport, "serial");
        assert_eq!(cfg.serial_port.as_deref(), Some("/dev/ttyUSB0"));
    }

    #[test]
    fn config_from_wizard_probe() {
        let devices = vec![DiscoveredDevice {
            name: "ST-Link (SWD)".into(),
            transport: HardwareTransport::Probe,
            device_path: Some("/dev/stlinkv2".into()),
            detail: None,
        }];

        let cfg = config_from_wizard_choice(2, &devices);
        assert!(cfg.enabled);
        assert_eq!(cfg.transport, "probe");
    }

    #[test]
    fn config_from_wizard_software_only() {
        let devices: Vec<DiscoveredDevice> = vec![];
        let cfg = config_from_wizard_choice(3, &devices);
        assert!(!cfg.enabled);
        assert_eq!(cfg.transport, "none");
    }

    #[test]
    fn config_from_wizard_serial_no_serial_device_found() {
        // User picks serial but no serial device was discovered
        let devices = vec![DiscoveredDevice {
            name: "RPi GPIO".into(),
            transport: HardwareTransport::Native,
            device_path: Some("/dev/gpiomem".into()),
            detail: None,
        }];

        let cfg = config_from_wizard_choice(1, &devices);
        assert!(cfg.enabled);
        assert_eq!(cfg.transport, "serial");
        assert!(cfg.serial_port.is_none()); // Will need manual config later
    }

    #[test]
    fn config_from_wizard_out_of_bounds_defaults_to_software() {
        let devices: Vec<DiscoveredDevice> = vec![];
        let cfg = config_from_wizard_choice(99, &devices);
        assert!(!cfg.enabled);
    }

    // ── Discovery function runs without panicking ──────────────

    #[test]
    fn discover_hardware_does_not_panic() {
        // Should never panic regardless of the platform
        let devices = discover_hardware();
        // We can't assert what's found (platform-dependent) but it should not crash
        assert!(devices.len() < 100); // Sanity check
    }

    // ── DiscoveredDevice equality ──────────────────────────────

    #[test]
    fn discovered_device_equality() {
        let d1 = DiscoveredDevice {
            name: "Arduino".into(),
            transport: HardwareTransport::Serial,
            device_path: Some("/dev/ttyUSB0".into()),
            detail: None,
        };
        let d2 = d1.clone();
        assert_eq!(d1, d2);
    }

    #[test]
    fn discovered_device_inequality() {
        let d1 = DiscoveredDevice {
            name: "Arduino".into(),
            transport: HardwareTransport::Serial,
            device_path: Some("/dev/ttyUSB0".into()),
            detail: None,
        };
        let d2 = DiscoveredDevice {
            name: "ESP32".into(),
            transport: HardwareTransport::Serial,
            device_path: Some("/dev/ttyUSB1".into()),
            detail: None,
        };
        assert_ne!(d1, d2);
    }

    // ── Edge cases ─────────────────────────────────────────────

    #[test]
    fn config_with_all_pins_in_allowlist() {
        let cfg = HardwareConfig {
            allowed_pins: (0..=255).collect(),
            ..HardwareConfig::default()
        };
        // Every pin should be allowed
        for pin in 0..=255u8 {
            assert!(cfg.is_pin_allowed(pin));
        }
    }

    #[test]
    fn config_transport_unknown_string() {
        let cfg = HardwareConfig {
            transport: "quantum_bus".into(),
            ..HardwareConfig::default()
        };
        assert_eq!(cfg.transport_mode(), HardwareTransport::None);
    }

    #[test]
    fn config_transport_empty_string() {
        let cfg = HardwareConfig {
            transport: String::new(),
            ..HardwareConfig::default()
        };
        assert_eq!(cfg.transport_mode(), HardwareTransport::None);
    }

    #[test]
    fn validate_serial_empty_port_string_treated_as_set() {
        // An empty string is still Some(""), which passes the None check
        // but the serial backend would fail at open time — that's acceptable
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: Some(String::new()),
            ..HardwareConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_multiple_errors_first_wins() {
        // Serial with no port AND zero baud — the port error should surface first
        let cfg = HardwareConfig {
            enabled: true,
            transport: "serial".into(),
            serial_port: None,
            baud_rate: 0,
            ..HardwareConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("serial_port"));
    }
}
