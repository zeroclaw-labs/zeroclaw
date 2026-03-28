//! Device types and registry — stable aliases for discovered hardware.
//!
//! The LLM always refers to devices by alias (`"pico0"`, `"arduino0"`), never
//! by raw `/dev/` paths. The `DeviceRegistry` assigns these aliases at startup
//! and provides lookup + context building for tool execution.

use super::transport::Transport;
use std::collections::HashMap;
use std::sync::Arc;

// ── DeviceRuntime ─────────────────────────────────────────────────────────────

/// The software runtime / execution environment of a device.
///
/// Determines which host-side tooling is used for code deployment and execution.
/// Currently only [`MicroPython`](DeviceRuntime::MicroPython) is implemented;
/// other variants return a clear "not yet supported" error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRuntime {
    /// MicroPython — uses `mpremote` for code read/write/exec.
    MicroPython,
    /// CircuitPython — `mpremote`-compatible (future).
    CircuitPython,
    /// Arduino — `arduino-cli` for sketch upload (future).
    Arduino,
    /// STM32 / probe-rs based flashing and debugging (future).
    Nucleus,
    /// Linux / Raspberry Pi — ssh/shell execution (future).
    Linux,
}

impl DeviceRuntime {
    /// Derive the default runtime from a [`DeviceKind`].
    pub fn from_kind(kind: &DeviceKind) -> Self {
        match kind {
            DeviceKind::Pico | DeviceKind::Esp32 | DeviceKind::Generic => Self::MicroPython,
            DeviceKind::Arduino => Self::Arduino,
            DeviceKind::Nucleo => Self::Nucleus,
        }
    }
}

impl std::fmt::Display for DeviceRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MicroPython => write!(f, "MicroPython"),
            Self::CircuitPython => write!(f, "CircuitPython"),
            Self::Arduino => write!(f, "Arduino"),
            Self::Nucleus => write!(f, "Nucleus"),
            Self::Linux => write!(f, "Linux"),
        }
    }
}

// ── DeviceKind ────────────────────────────────────────────────────────────────

/// The category of a discovered hardware device.
///
/// Derived from USB Vendor ID or, for unknown VIDs, from a successful
/// ping handshake (which yields `Generic`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceKind {
    /// Raspberry Pi Pico / Pico W (VID `0x2E8A`).
    Pico,
    /// Arduino Uno, Mega, etc. (VID `0x2341`).
    Arduino,
    /// ESP32 via CP2102 bridge (VID `0x10C4`).
    Esp32,
    /// STM32 Nucleo (VID `0x0483`).
    Nucleo,
    /// Unknown VID that passed the ZeroClaw firmware ping handshake.
    Generic,
}

impl DeviceKind {
    /// Derive the device kind from a USB Vendor ID.
    /// Returns `None` if the VID is unknown (0 or unrecognised).
    pub fn from_vid(vid: u16) -> Option<Self> {
        match vid {
            0x2e8a => Some(Self::Pico),
            0x2341 => Some(Self::Arduino),
            0x10c4 => Some(Self::Esp32),
            0x0483 => Some(Self::Nucleo),
            _ => None,
        }
    }
}

impl std::fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pico => write!(f, "pico"),
            Self::Arduino => write!(f, "arduino"),
            Self::Esp32 => write!(f, "esp32"),
            Self::Nucleo => write!(f, "nucleo"),
            Self::Generic => write!(f, "generic"),
        }
    }
}

/// Capability flags for a connected device.
///
/// Populated from device handshake or static board metadata.
/// Tools can check capabilities before attempting unsupported operations.
#[derive(Debug, Clone, Default)]
pub struct DeviceCapabilities {
    pub gpio: bool,
    pub i2c: bool,
    pub spi: bool,
    pub swd: bool,
    pub uart: bool,
    pub adc: bool,
    pub pwm: bool,
}

/// A discovered and registered hardware device.
#[derive(Debug, Clone)]
pub struct Device {
    /// Stable session alias (e.g. `"pico0"`, `"arduino0"`, `"nucleo0"`).
    pub alias: String,
    /// Board name from registry (e.g. `"raspberry-pi-pico"`, `"arduino-uno"`).
    pub board_name: String,
    /// Device category derived from VID or ping handshake.
    pub kind: DeviceKind,
    /// Software runtime that determines how code is deployed/executed.
    pub runtime: DeviceRuntime,
    /// USB Vendor ID (if USB-connected).
    pub vid: Option<u16>,
    /// USB Product ID (if USB-connected).
    pub pid: Option<u16>,
    /// Raw device path (e.g. `"/dev/ttyACM0"`) — internal use only.
    /// Tools MUST NOT use this directly; always go through Transport.
    pub device_path: Option<String>,
    /// Architecture description (e.g. `"ARM Cortex-M0+"`).
    pub architecture: Option<String>,
    /// Firmware identifier reported by device during ping handshake.
    pub firmware: Option<String>,
}

impl Device {
    /// Convenience accessor — same as `device_path` (matches the Phase 2 spec naming).
    pub fn port(&self) -> Option<&str> {
        self.device_path.as_deref()
    }
}

/// Context passed to hardware tools during execution.
///
/// Provides the tool with access to the device identity, transport layer,
/// and capability flags without the tool managing connections itself.
pub struct DeviceContext {
    /// The device this tool is operating on.
    pub device: Arc<Device>,
    /// Transport for sending commands to the device.
    pub transport: Arc<dyn Transport>,
    /// Device capabilities (gpio, i2c, spi, etc.).
    pub capabilities: DeviceCapabilities,
}

/// A registered device entry with its transport and capabilities.
struct RegisteredDevice {
    device: Arc<Device>,
    transport: Option<Arc<dyn Transport>>,
    capabilities: DeviceCapabilities,
}

/// Summary string returned by [`DeviceRegistry::prompt_summary`] when no
/// devices are registered.  Exported so callers can compare against it without
/// duplicating the literal.
pub const NO_HW_DEVICES_SUMMARY: &str = "No hardware devices connected.";

/// Registry of discovered devices with stable session aliases.
///
/// - Scans at startup (via `hardware::discover`)
/// - Assigns aliases: `pico0`, `pico1`, `arduino0`, `nucleo0`, `device0`, etc.
/// - Provides alias-based lookup for tool dispatch
/// - Generates prompt summaries for LLM context
pub struct DeviceRegistry {
    devices: HashMap<String, RegisteredDevice>,
    alias_counters: HashMap<String, u32>,
}

impl DeviceRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
            alias_counters: HashMap::new(),
        }
    }

    /// Register a discovered device and assign a stable alias.
    ///
    /// Returns the assigned alias (e.g. `"pico0"`).
    pub fn register(
        &mut self,
        board_name: &str,
        vid: Option<u16>,
        pid: Option<u16>,
        device_path: Option<String>,
        architecture: Option<String>,
    ) -> String {
        let prefix = alias_prefix(board_name);
        let counter = self.alias_counters.entry(prefix.clone()).or_insert(0);
        let alias = format!("{}{}", prefix, counter);
        *counter += 1;

        let kind = vid
            .and_then(DeviceKind::from_vid)
            .unwrap_or(DeviceKind::Generic);
        let runtime = DeviceRuntime::from_kind(&kind);

        let device = Arc::new(Device {
            alias: alias.clone(),
            board_name: board_name.to_string(),
            kind,
            runtime,
            vid,
            pid,
            device_path,
            architecture,
            firmware: None,
        });

        self.devices.insert(
            alias.clone(),
            RegisteredDevice {
                device,
                transport: None,
                capabilities: DeviceCapabilities::default(),
            },
        );

        alias
    }

    /// Attach a transport and capabilities to a previously registered device.
    ///
    /// Returns `Err` when `alias` is not found in the registry (should not
    /// happen in normal usage because callers pass aliases from `register`).
    pub fn attach_transport(
        &mut self,
        alias: &str,
        transport: Arc<dyn Transport>,
        capabilities: DeviceCapabilities,
    ) -> anyhow::Result<()> {
        if let Some(entry) = self.devices.get_mut(alias) {
            entry.transport = Some(transport);
            entry.capabilities = capabilities;
            Ok(())
        } else {
            Err(anyhow::anyhow!("unknown device alias: {}", alias))
        }
    }

    /// Look up a device by alias.
    pub fn get_device(&self, alias: &str) -> Option<Arc<Device>> {
        self.devices.get(alias).map(|e| e.device.clone())
    }

    /// Build a `DeviceContext` for a device by alias.
    ///
    /// Returns `None` if the alias is unknown or no transport is attached.
    pub fn context(&self, alias: &str) -> Option<DeviceContext> {
        self.devices.get(alias).and_then(|e| {
            e.transport.as_ref().map(|t| DeviceContext {
                device: e.device.clone(),
                transport: t.clone(),
                capabilities: e.capabilities.clone(),
            })
        })
    }

    /// List all registered device aliases.
    pub fn aliases(&self) -> Vec<&str> {
        self.devices.keys().map(|s| s.as_str()).collect()
    }

    /// Return a summary of connected devices for the LLM system prompt.
    pub fn prompt_summary(&self) -> String {
        if self.devices.is_empty() {
            return NO_HW_DEVICES_SUMMARY.to_string();
        }

        let mut lines = vec!["Connected devices:".to_string()];
        let mut sorted_aliases: Vec<&String> = self.devices.keys().collect();
        sorted_aliases.sort();
        for alias in sorted_aliases {
            let entry = &self.devices[alias];
            let status = entry
                .transport
                .as_ref()
                .map(|t| {
                    if t.is_connected() {
                        "connected"
                    } else {
                        "disconnected"
                    }
                })
                .unwrap_or("no transport");
            let arch = entry
                .device
                .architecture
                .as_deref()
                .unwrap_or("unknown arch");
            lines.push(format!(
                "  {} — {} ({}) [{}]",
                alias, entry.device.board_name, arch, status
            ));
        }
        lines.join("\n")
    }

    /// Resolve a GPIO-capable device alias from tool arguments.
    ///
    /// If `args["device"]` is provided, uses that alias directly.
    /// Otherwise, auto-selects the single GPIO-capable device, returning an
    /// error description if zero or multiple GPIO devices are available.
    ///
    /// On success returns `(alias, DeviceContext)` — both are owned / Arc-based
    /// so the caller can drop the registry lock before doing async I/O.
    pub fn resolve_gpio_device(
        &self,
        args: &serde_json::Value,
    ) -> Result<(String, DeviceContext), String> {
        let device_alias: String = match args.get("device").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => {
                let gpio_aliases: Vec<String> = self
                    .aliases()
                    .into_iter()
                    .filter(|a| {
                        self.context(a)
                            .map(|c| c.capabilities.gpio)
                            .unwrap_or(false)
                    })
                    .map(|a| a.to_string())
                    .collect();
                match gpio_aliases.as_slice() {
                    [single] => single.clone(),
                    [] => {
                        return Err("no GPIO-capable device found; specify \"device\" parameter"
                            .to_string());
                    }
                    _ => {
                        return Err(format!(
                            "multiple devices available ({}); specify \"device\" parameter",
                            gpio_aliases.join(", ")
                        ));
                    }
                }
            }
        };

        let ctx = self.context(&device_alias).ok_or_else(|| {
            format!(
                "device '{}' not found or has no transport attached",
                device_alias
            )
        })?;

        // Verify the device advertises GPIO capability.
        if !ctx.capabilities.gpio {
            return Err(format!(
                "device '{}' does not support GPIO; specify a GPIO-capable device",
                device_alias
            ));
        }

        Ok((device_alias, ctx))
    }

    /// Number of registered devices.
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// Look up a device by alias (alias for `get_device` matching the Phase 2 spec).
    pub fn get(&self, alias: &str) -> Option<Arc<Device>> {
        self.get_device(alias)
    }

    /// Return all registered devices.
    pub fn all(&self) -> Vec<Arc<Device>> {
        self.devices.values().map(|e| e.device.clone()).collect()
    }

    /// One-line summary per device: `"pico0: raspberry-pi-pico /dev/ttyACM0"`.
    ///
    /// Suitable for CLI output and debug logging.
    pub fn summary(&self) -> String {
        if self.devices.is_empty() {
            return String::new();
        }
        let mut lines: Vec<String> = self
            .devices
            .values()
            .map(|e| {
                let path = e.device.port().unwrap_or("(native)");
                format!("{}: {} {}", e.device.alias, e.device.board_name, path)
            })
            .collect();
        lines.sort(); // deterministic for tests
        lines.join("\n")
    }

    /// Discover all connected serial devices and populate the registry.
    ///
    /// Steps:
    /// 1. Call `discover::scan_serial_devices()` to enumerate port paths + VID/PID.
    /// 2. For each device with a recognised VID: register and attach a transport.
    /// 3. For unknown VID (`0`): attempt a 300 ms ping handshake; register only
    ///    if the device responds with ZeroClaw firmware.
    /// 4. Return the populated registry.
    ///
    /// Returns an empty registry when no devices are found or the `hardware`
    /// feature is disabled.
    #[cfg(feature = "hardware")]
    pub async fn discover() -> Self {
        use super::{
            discover::scan_serial_devices,
            serial::{HardwareSerialTransport, DEFAULT_BAUD},
        };

        let mut registry = Self::new();

        for info in scan_serial_devices() {
            let is_known_vid = info.vid != 0;

            // For unknown VIDs, run the ping handshake before registering.
            // This avoids registering random USB-serial adapters.
            // If the probe succeeds we reuse the same transport instance below.
            let probe_transport = if !is_known_vid {
                let probe = HardwareSerialTransport::new(&info.port_path, DEFAULT_BAUD);
                if !probe.ping_handshake().await {
                    tracing::debug!(
                        port = %info.port_path,
                        "skipping unknown device: no ZeroClaw firmware response"
                    );
                    continue;
                }
                Some(probe)
            } else {
                None
            };

            let board_name = info.board_name.as_deref().unwrap_or("unknown").to_string();

            let alias = registry.register(
                &board_name,
                if info.vid != 0 { Some(info.vid) } else { None },
                if info.pid != 0 { Some(info.pid) } else { None },
                Some(info.port_path.clone()),
                info.architecture,
            );

            // For unknown-VID devices that passed ping: mark as Generic.
            // (register() will have already set kind = Generic for vid=None)

            let transport: Arc<dyn super::transport::Transport> =
                if let Some(probe) = probe_transport {
                    Arc::new(probe)
                } else {
                    Arc::new(HardwareSerialTransport::new(&info.port_path, DEFAULT_BAUD))
                };
            let caps = DeviceCapabilities {
                gpio: true, // assume GPIO; Phase 3 will populate via capabilities handshake
                ..DeviceCapabilities::default()
            };
            registry.attach_transport(&alias, transport, caps)
                .unwrap_or_else(|e| tracing::warn!(alias = %alias, err = %e, "attach_transport: unexpected unknown alias"));

            tracing::info!(
                alias = %alias,
                port  = %info.port_path,
                vid   = %info.vid,
                "device registered"
            );
        }

        registry
    }
}

impl DeviceRegistry {
    /// Reconnect a device after reboot/reflash.
    ///
    /// Drops the old transport, creates a fresh [`HardwareSerialTransport`] for
    /// the given (or existing) port path, runs the ping handshake to confirm
    /// ZeroClaw firmware is alive, and re-attaches the transport.
    ///
    /// Pass `new_port` when the OS assigned a different path after reboot;
    /// pass `None` to reuse the device's current path.
    #[cfg(feature = "hardware")]
    pub async fn reconnect(&mut self, alias: &str, new_port: Option<&str>) -> anyhow::Result<()> {
        use super::serial::{HardwareSerialTransport, DEFAULT_BAUD};

        let entry = self
            .devices
            .get_mut(alias)
            .ok_or_else(|| anyhow::anyhow!("unknown device alias: {alias}"))?;

        // Determine the port path — prefer the caller's override.
        let port_path = match new_port {
            Some(p) => {
                // Update the device record with the new path.
                let mut updated = (*entry.device).clone();
                updated.device_path = Some(p.to_string());
                entry.device = Arc::new(updated);
                p.to_string()
            }
            None => entry
                .device
                .device_path
                .clone()
                .ok_or_else(|| anyhow::anyhow!("device {alias} has no port path"))?,
        };

        // Drop the stale transport.
        entry.transport = None;

        // Create a fresh transport and verify firmware is alive.
        let transport = HardwareSerialTransport::new(&port_path, DEFAULT_BAUD);
        if !transport.ping_handshake().await {
            anyhow::bail!(
                "ping handshake failed after reconnect on {port_path} — \
                 firmware may not be running"
            );
        }

        entry.transport = Some(Arc::new(transport) as Arc<dyn super::transport::Transport>);
        entry.capabilities.gpio = true;

        tracing::info!(alias = %alias, port = %port_path, "device reconnected");
        Ok(())
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive alias prefix from board name.
fn alias_prefix(board_name: &str) -> String {
    match board_name {
        s if s.starts_with("raspberry-pi-pico") || s.starts_with("pico") => "pico".to_string(),
        s if s.starts_with("arduino") => "arduino".to_string(),
        s if s.starts_with("esp32") || s.starts_with("esp") => "esp".to_string(),
        s if s.starts_with("nucleo") || s.starts_with("stm32") => "nucleo".to_string(),
        s if s.starts_with("rpi") || s == "raspberry-pi" => "rpi".to_string(),
        _ => "device".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_prefix_pico_variants() {
        assert_eq!(alias_prefix("raspberry-pi-pico"), "pico");
        assert_eq!(alias_prefix("pico-w"), "pico");
        assert_eq!(alias_prefix("pico"), "pico");
    }

    #[test]
    fn alias_prefix_arduino() {
        assert_eq!(alias_prefix("arduino-uno"), "arduino");
        assert_eq!(alias_prefix("arduino-mega"), "arduino");
    }

    #[test]
    fn alias_prefix_esp() {
        assert_eq!(alias_prefix("esp32"), "esp");
        assert_eq!(alias_prefix("esp32-s3"), "esp");
    }

    #[test]
    fn alias_prefix_nucleo() {
        assert_eq!(alias_prefix("nucleo-f401re"), "nucleo");
        assert_eq!(alias_prefix("stm32-discovery"), "nucleo");
    }

    #[test]
    fn alias_prefix_rpi() {
        assert_eq!(alias_prefix("rpi-gpio"), "rpi");
        assert_eq!(alias_prefix("raspberry-pi"), "rpi");
    }

    #[test]
    fn alias_prefix_unknown() {
        assert_eq!(alias_prefix("custom-board"), "device");
    }

    #[test]
    fn registry_assigns_sequential_aliases() {
        let mut reg = DeviceRegistry::new();
        let a1 = reg.register("raspberry-pi-pico", Some(0x2E8A), Some(0x000A), None, None);
        let a2 = reg.register("raspberry-pi-pico", Some(0x2E8A), Some(0x000A), None, None);
        let a3 = reg.register("arduino-uno", Some(0x2341), Some(0x0043), None, None);

        assert_eq!(a1, "pico0");
        assert_eq!(a2, "pico1");
        assert_eq!(a3, "arduino0");
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn registry_get_device_by_alias() {
        let mut reg = DeviceRegistry::new();
        let alias = reg.register(
            "nucleo-f401re",
            Some(0x0483),
            Some(0x374B),
            Some("/dev/ttyACM0".to_string()),
            Some("ARM Cortex-M4".to_string()),
        );

        let device = reg.get_device(&alias).unwrap();
        assert_eq!(device.alias, "nucleo0");
        assert_eq!(device.board_name, "nucleo-f401re");
        assert_eq!(device.vid, Some(0x0483));
        assert_eq!(device.architecture.as_deref(), Some("ARM Cortex-M4"));
    }

    #[test]
    fn registry_unknown_alias_returns_none() {
        let reg = DeviceRegistry::new();
        assert!(reg.get_device("nonexistent").is_none());
        assert!(reg.context("nonexistent").is_none());
    }

    #[test]
    fn registry_context_none_without_transport() {
        let mut reg = DeviceRegistry::new();
        let alias = reg.register("pico", None, None, None, None);
        // No transport attached → context returns None.
        assert!(reg.context(&alias).is_none());
    }

    #[test]
    fn registry_prompt_summary_empty() {
        let reg = DeviceRegistry::new();
        assert_eq!(reg.prompt_summary(), NO_HW_DEVICES_SUMMARY);
    }

    #[test]
    fn registry_prompt_summary_with_devices() {
        let mut reg = DeviceRegistry::new();
        reg.register(
            "raspberry-pi-pico",
            Some(0x2E8A),
            None,
            None,
            Some("ARM Cortex-M0+".to_string()),
        );
        let summary = reg.prompt_summary();
        assert!(summary.contains("pico0"));
        assert!(summary.contains("raspberry-pi-pico"));
        assert!(summary.contains("ARM Cortex-M0+"));
        assert!(summary.contains("no transport"));
    }

    #[test]
    fn device_capabilities_default_all_false() {
        let caps = DeviceCapabilities::default();
        assert!(!caps.gpio);
        assert!(!caps.i2c);
        assert!(!caps.spi);
        assert!(!caps.swd);
        assert!(!caps.uart);
        assert!(!caps.adc);
        assert!(!caps.pwm);
    }

    #[test]
    fn registry_default_is_empty() {
        let reg = DeviceRegistry::default();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn registry_aliases_returns_all() {
        let mut reg = DeviceRegistry::new();
        reg.register("pico", None, None, None, None);
        reg.register("arduino-uno", None, None, None, None);
        let mut aliases = reg.aliases();
        aliases.sort();
        assert_eq!(aliases, vec!["arduino0", "pico0"]);
    }

    // ── Phase 2 new tests ────────────────────────────────────────────────────

    #[test]
    fn device_kind_from_vid_known() {
        assert_eq!(DeviceKind::from_vid(0x2e8a), Some(DeviceKind::Pico));
        assert_eq!(DeviceKind::from_vid(0x2341), Some(DeviceKind::Arduino));
        assert_eq!(DeviceKind::from_vid(0x10c4), Some(DeviceKind::Esp32));
        assert_eq!(DeviceKind::from_vid(0x0483), Some(DeviceKind::Nucleo));
    }

    #[test]
    fn device_kind_from_vid_unknown() {
        assert_eq!(DeviceKind::from_vid(0x0000), None);
        assert_eq!(DeviceKind::from_vid(0xffff), None);
    }

    #[test]
    fn device_kind_display() {
        assert_eq!(DeviceKind::Pico.to_string(), "pico");
        assert_eq!(DeviceKind::Arduino.to_string(), "arduino");
        assert_eq!(DeviceKind::Esp32.to_string(), "esp32");
        assert_eq!(DeviceKind::Nucleo.to_string(), "nucleo");
        assert_eq!(DeviceKind::Generic.to_string(), "generic");
    }

    #[test]
    fn register_sets_kind_from_vid() {
        let mut reg = DeviceRegistry::new();
        let a = reg.register("raspberry-pi-pico", Some(0x2e8a), Some(0x000a), None, None);
        assert_eq!(reg.get(&a).unwrap().kind, DeviceKind::Pico);

        let b = reg.register("arduino-uno", Some(0x2341), Some(0x0043), None, None);
        assert_eq!(reg.get(&b).unwrap().kind, DeviceKind::Arduino);

        let c = reg.register("unknown-device", None, None, None, None);
        assert_eq!(reg.get(&c).unwrap().kind, DeviceKind::Generic);
    }

    #[test]
    fn device_port_returns_device_path() {
        let mut reg = DeviceRegistry::new();
        let alias = reg.register(
            "raspberry-pi-pico",
            Some(0x2e8a),
            None,
            Some("/dev/ttyACM0".to_string()),
            None,
        );
        let device = reg.get(&alias).unwrap();
        assert_eq!(device.port(), Some("/dev/ttyACM0"));
    }

    #[test]
    fn device_port_none_without_path() {
        let mut reg = DeviceRegistry::new();
        let alias = reg.register("pico", None, None, None, None);
        assert!(reg.get(&alias).unwrap().port().is_none());
    }

    #[test]
    fn registry_get_is_alias_for_get_device() {
        let mut reg = DeviceRegistry::new();
        let alias = reg.register("raspberry-pi-pico", Some(0x2e8a), None, None, None);
        let via_get = reg.get(&alias);
        let via_get_device = reg.get_device(&alias);
        assert!(via_get.is_some());
        assert!(via_get_device.is_some());
        assert_eq!(via_get.unwrap().alias, via_get_device.unwrap().alias);
    }

    #[test]
    fn registry_all_returns_every_device() {
        let mut reg = DeviceRegistry::new();
        reg.register("raspberry-pi-pico", Some(0x2e8a), None, None, None);
        reg.register("arduino-uno", Some(0x2341), None, None, None);
        assert_eq!(reg.all().len(), 2);
    }

    #[test]
    fn registry_summary_one_liner_per_device() {
        let mut reg = DeviceRegistry::new();
        reg.register(
            "raspberry-pi-pico",
            Some(0x2e8a),
            None,
            Some("/dev/ttyACM0".to_string()),
            None,
        );
        let s = reg.summary();
        assert!(s.contains("pico0"));
        assert!(s.contains("raspberry-pi-pico"));
        assert!(s.contains("/dev/ttyACM0"));
    }

    #[test]
    fn registry_summary_empty_when_no_devices() {
        let reg = DeviceRegistry::new();
        assert_eq!(reg.summary(), "");
    }
}
