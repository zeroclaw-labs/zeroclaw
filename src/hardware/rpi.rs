//! Raspberry Pi self-discovery and native GPIO tools.
//!
//! Only compiled on Linux with the `peripheral-rpi` feature enabled.
//!
//! Provides two capabilities:
//!
//! 1. **Board detection** — `RpiModel` / `RpiSystemContext` detect which Pi model
//!    is running, its IP address, temperature, and GPIO availability.  The result is
//!    injected into the system prompt so the LLM knows it is running *on* the device.
//!
//! 2. **Tool registration** — Four tools are auto-registered when an RPi board is
//!    detected at boot (no `[[peripherals.boards]]` config entry required):
//!    - `gpio_rpi_write`  — set a GPIO pin HIGH / LOW
//!    - `gpio_rpi_read`   — read a GPIO pin value
//!    - `gpio_rpi_blink`  — blink a GPIO pin N times
//!    - `rpi_system_info` — return board model, RAM, temp, IP

use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;
use std::time::Duration;

// ─── LED sysfs helpers ──────────────────────────────────────────────────────

/// The Linux LED subsystem paths for the onboard ACT LED.
/// On RPi 3B/4B/5/Zero2W the ACT LED is wired through the kernel LED driver,
/// not directly accessible via rppal GPIO.  We must use sysfs instead.
const LED_SYSFS_PATHS: &[&str] = &[
    "/sys/class/leds/ACT/brightness",
    "/sys/class/leds/led0/brightness",
];

const LED_TRIGGER_PATHS: &[&str] = &[
    "/sys/class/leds/ACT/trigger",
    "/sys/class/leds/led0/trigger",
];

/// Returns true if `pin` is the onboard ACT LED for the detected RPi model.
fn is_onboard_led(pin: u8) -> bool {
    RpiModel::detect()
        .and_then(|m| m.onboard_led_gpio())
        .is_some_and(|led| led == pin)
}

/// Find the first existing sysfs brightness path for the ACT LED.
fn led_brightness_path() -> Option<&'static str> {
    LED_SYSFS_PATHS
        .iter()
        .copied()
        .find(|p| std::path::Path::new(p).exists())
}

/// Ensure the ACT LED trigger is set to "none" so we can control it.
fn ensure_led_trigger_none() {
    for path in LED_TRIGGER_PATHS {
        if std::path::Path::new(path).exists() {
            let _ = fs::write(path, "none");
            return;
        }
    }
}

// ─── Board model ────────────────────────────────────────────────────────────

/// Detected Raspberry Pi model variant.
#[derive(Debug, Clone, PartialEq)]
pub enum RpiModel {
    Rpi3B,
    Rpi3BPlus,
    Rpi4B,
    Rpi5,
    RpiZero2W,
    Unknown(String),
}

impl RpiModel {
    /// Detect RPi model from device-tree or /proc/cpuinfo.
    pub fn detect() -> Option<Self> {
        // Device tree model string is the most reliable source.
        if let Ok(raw) = fs::read_to_string("/proc/device-tree/model") {
            let model = raw.trim_end_matches('\0');
            return Some(Self::from_model_string(model));
        }
        // Fallback: scan /proc/cpuinfo for a "Model" line.
        if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
            if cpuinfo.contains("Raspberry Pi") {
                for line in cpuinfo.lines() {
                    if let Some(rest) = line.strip_prefix("Model") {
                        let model = rest.trim_start_matches(':').trim();
                        return Some(Self::from_model_string(model));
                    }
                }
                return Some(Self::Unknown("Raspberry Pi (unknown model)".into()));
            }
        }
        None
    }

    fn from_model_string(s: &str) -> Self {
        let lower = s.to_lowercase();
        if lower.contains("3 model b plus") || lower.contains("3b+") {
            Self::Rpi3BPlus
        } else if lower.contains("3 model b") || lower.contains("3b") {
            Self::Rpi3B
        } else if lower.contains("4 model b") || lower.contains("4b") {
            Self::Rpi4B
        } else if lower.contains("raspberry pi 5") || lower.contains(" 5 ") {
            Self::Rpi5
        } else if lower.contains("zero 2") {
            Self::RpiZero2W
        } else {
            Self::Unknown(s.to_string())
        }
    }

    /// BCM GPIO number of the on-board activity LED, if known.
    pub fn onboard_led_gpio(&self) -> Option<u8> {
        match self {
            Self::Rpi3B | Self::Rpi3BPlus => Some(47),
            Self::Rpi4B => Some(42),
            Self::Rpi5 => Some(9),
            Self::RpiZero2W => Some(29),
            Self::Unknown(_) => None,
        }
    }

    /// Human-readable display name.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Rpi3B => "Raspberry Pi 3 Model B",
            Self::Rpi3BPlus => "Raspberry Pi 3 Model B+",
            Self::Rpi4B => "Raspberry Pi 4 Model B",
            Self::Rpi5 => "Raspberry Pi 5",
            Self::RpiZero2W => "Raspberry Pi Zero 2 W",
            Self::Unknown(s) => s.as_str(),
        }
    }
}

// ─── System context ──────────────────────────────────────────────────────────

/// System information discovered at boot when running on a Raspberry Pi.
#[derive(Debug, Clone)]
pub struct RpiSystemContext {
    pub model: RpiModel,
    pub hostname: String,
    pub ip_address: String,
    pub wifi_interface: Option<String>,
    pub total_ram_mb: u64,
    pub free_ram_mb: u64,
    pub cpu_temp_celsius: Option<f32>,
    pub gpio_available: bool,
}

impl RpiSystemContext {
    /// Attempt to detect the current board and collect system info.
    /// Returns `None` when not running on a Raspberry Pi.
    pub fn discover() -> Option<Self> {
        let model = RpiModel::detect()?;

        let hostname = fs::read_to_string("/etc/hostname")
            .unwrap_or_default()
            .trim()
            .to_string();

        let ip_address = Self::get_ip_address();
        let wifi_interface = Self::get_wifi_interface();
        let (total_ram_mb, free_ram_mb) = Self::get_memory_info();
        let cpu_temp_celsius = Self::get_cpu_temp();
        let gpio_available = std::path::Path::new("/dev/gpiomem").exists();

        Some(Self {
            model,
            hostname,
            ip_address,
            wifi_interface,
            total_ram_mb,
            free_ram_mb,
            cpu_temp_celsius,
            gpio_available,
        })
    }

    /// Determine the primary non-loopback IPv4 address using a UDP routing trick.
    /// No packet is ever sent — we just resolve the outbound route.
    fn get_ip_address() -> String {
        use std::net::UdpSocket;
        UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| {
                s.connect("8.8.8.8:80")?;
                s.local_addr()
            })
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// Returns the first wireless interface name listed in /proc/net/wireless, if any.
    fn get_wifi_interface() -> Option<String> {
        let text = fs::read_to_string("/proc/net/wireless").ok()?;
        text.lines()
            .skip(2) // header rows
            .find(|l| l.contains(':'))
            .map(|l| l.split(':').next().unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Read MemTotal and MemAvailable from /proc/meminfo and return (total_mb, free_mb).
    fn get_memory_info() -> (u64, u64) {
        let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let mut total = 0u64;
        let mut available = 0u64;
        for line in meminfo.lines() {
            if line.starts_with("MemTotal:") {
                total = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
                    / 1024;
            }
            if line.starts_with("MemAvailable:") {
                available = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
                    / 1024;
            }
        }
        (total, available)
    }

    /// Read CPU temperature from the thermal zone sysfs file (millidegrees → °C).
    fn get_cpu_temp() -> Option<f32> {
        fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok())
            .map(|t| t / 1000.0)
    }

    /// Generate the system prompt section that describes this device to the LLM.
    pub fn to_system_prompt(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "## Running On Device (Raspberry Pi)");
        let _ = writeln!(s);
        let _ = writeln!(s, "- Board: {}", self.model.display_name());
        let _ = writeln!(s, "- Hostname: {}", self.hostname);
        let _ = writeln!(s, "- IP Address: {}", self.ip_address);
        if let Some(ref iface) = self.wifi_interface {
            let _ = writeln!(s, "- WiFi interface: {}", iface);
        }
        let _ = writeln!(
            s,
            "- RAM: {}MB total, {}MB available",
            self.total_ram_mb, self.free_ram_mb
        );
        if let Some(temp) = self.cpu_temp_celsius {
            let _ = writeln!(s, "- CPU Temperature: {:.1}°C", temp);
        }
        if let Some(led_pin) = self.model.onboard_led_gpio() {
            let _ = writeln!(s, "- Onboard ACT LED: BCM GPIO {}", led_pin);
        }
        if self.gpio_available {
            let _ = writeln!(s, "- GPIO: available via rppal (/dev/gpiomem)");
            let _ = writeln!(s);
            s.push_str(
                "Use `gpio_rpi_write`, `gpio_rpi_read`, and `gpio_rpi_blink` for all GPIO \
                operations — they access /dev/gpiomem directly, no serial port or mpremote needed.\n",
            );
        }
        s
    }

    /// Write an `rpi0.md` hardware context file to `~/.zeroclaw/hardware/devices/`.
    /// Silently skips on failure so boot is never blocked.
    pub fn write_hardware_context_file(&self) {
        let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) else {
            return;
        };
        let devices_dir = home.join(".zeroclaw").join("hardware").join("devices");
        if let Err(e) = fs::create_dir_all(&devices_dir) {
            tracing::warn!("Failed to create hardware devices dir: {e}");
            return;
        }

        let path = devices_dir.join("rpi0.md");
        let content = self.device_profile_markdown();
        if let Err(e) = fs::write(&path, &content) {
            tracing::warn!("Failed to write rpi0.md: {e}");
        } else {
            tracing::debug!(path = %path.display(), "Wrote rpi0.md hardware context file");
        }
    }

    fn device_profile_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# rpi0 — {}", self.model.display_name());
        let _ = writeln!(s);
        let _ = writeln!(s, "## System");
        let _ = writeln!(s, "- Hostname: {}", self.hostname);
        let _ = writeln!(s, "- IP: {} (at last boot)", self.ip_address);
        let _ = writeln!(s, "- RAM: {}MB total", self.total_ram_mb);
        let _ = writeln!(
            s,
            "- Runtime: ZeroClaw native (rppal — no serial, no mpremote)"
        );
        if let Some(ref iface) = self.wifi_interface {
            let _ = writeln!(s, "- WiFi interface: {}", iface);
        }
        let _ = writeln!(s);
        let _ = writeln!(s, "## GPIO — BCM numbering");
        if let Some(led_pin) = self.model.onboard_led_gpio() {
            let _ = writeln!(
                s,
                "- GPIO {led_pin}: ACT LED (onboard green LED) — use gpio_rpi_write/blink"
            );
        }
        let _ = writeln!(s, "- GPIO 2/3: I2C SDA/SCL");
        let _ = writeln!(s, "- GPIO 7-11: SPI");
        let _ = writeln!(s, "- All other BCM pins: general purpose");
        let _ = writeln!(s);
        let _ = writeln!(s, "## Tool Usage Rules");
        let _ = writeln!(s, "- Single pin on/off → `gpio_rpi_write(pin, value)`");
        let _ = writeln!(
            s,
            "- Blink/repeat → `gpio_rpi_blink(pin, times, on_ms, off_ms)`"
        );
        let _ = writeln!(s, "- Read pin → `gpio_rpi_read(pin)`");
        let _ = writeln!(s, "- System stats → `rpi_system_info()`");
        let _ = writeln!(
            s,
            "- DO NOT use `device_exec` or `mpremote` — not available on this board"
        );
        let _ = writeln!(
            s,
            "- DO NOT use `gpio_write` (serial JSON) — use `gpio_rpi_write` instead"
        );
        s
    }
}

// ─── Tool: gpio_rpi_write ────────────────────────────────────────────────────

/// Set a GPIO pin HIGH or LOW directly on this Raspberry Pi via rppal.
pub struct GpioRpiWriteTool;

#[async_trait]
impl Tool for GpioRpiWriteTool {
    fn name(&self) -> &str {
        "gpio_rpi_write"
    }

    fn description(&self) -> &str {
        "Set a GPIO pin HIGH (1) or LOW (0) directly on this Raspberry Pi. \
        Uses BCM pin numbers (e.g. 47 for the ACT LED on RPi 3B). \
        No serial port needed — accesses /dev/gpiomem via rppal."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO number (e.g. 47 for ACT LED on RPi 3B)"
                },
                "value": {
                    "type": "integer",
                    "description": "1 for HIGH, 0 for LOW"
                }
            },
            "required": ["pin", "value"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin_raw = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let pin = u8::try_from(pin_raw)
            .map_err(|_| anyhow::anyhow!("pin value {pin_raw} out of range for u8"))?;
        let value = args
            .get("value")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'value' parameter"))?;
        let state = if value == 0 { "LOW" } else { "HIGH" };

        // Onboard ACT LED → Linux LED subsystem (sysfs)
        if is_onboard_led(pin) {
            let brightness = if value == 0 { "0" } else { "1" };
            let path = led_brightness_path()
                .ok_or_else(|| anyhow::anyhow!("ACT LED sysfs path not found"))?;
            ensure_led_trigger_none();
            fs::write(path, brightness)?;
            return Ok(ToolResult {
                success: true,
                output: format!("ACT LED (GPIO {}) → {} (via sysfs)", pin, state),
                error: None,
            });
        }

        // Regular GPIO pin → rppal
        let level = if value == 0 {
            rppal::gpio::Level::Low
        } else {
            rppal::gpio::Level::High
        };

        tokio::task::spawn_blocking(move || {
            let gpio = rppal::gpio::Gpio::new()?;
            let mut p = gpio.get(pin)?.into_output();
            p.write(level);
            Ok::<_, anyhow::Error>(())
        })
        .await??;

        Ok(ToolResult {
            success: true,
            output: format!("GPIO {} → {}", pin, state),
            error: None,
        })
    }
}

// ─── Tool: gpio_rpi_read ─────────────────────────────────────────────────────

/// Read a GPIO pin value on this Raspberry Pi via rppal.
pub struct GpioRpiReadTool;

#[async_trait]
impl Tool for GpioRpiReadTool {
    fn name(&self) -> &str {
        "gpio_rpi_read"
    }

    fn description(&self) -> &str {
        "Read the current state (0 or 1) of a GPIO pin on this Raspberry Pi. \
        Uses BCM pin numbers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO number"
                }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin_raw = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let pin = u8::try_from(pin_raw)
            .map_err(|_| anyhow::anyhow!("pin value {pin_raw} out of range for u8"))?;

        // Onboard ACT LED → read from sysfs
        if is_onboard_led(pin) {
            let path = led_brightness_path()
                .ok_or_else(|| anyhow::anyhow!("ACT LED sysfs path not found"))?;
            let raw = fs::read_to_string(path)?.trim().to_string();
            let value: u8 = if raw == "0" { 0 } else { 1 };
            let state = if value == 0 { "LOW" } else { "HIGH" };
            return Ok(ToolResult {
                success: true,
                output: json!({ "pin": pin, "value": value, "state": state, "source": "sysfs" })
                    .to_string(),
                error: None,
            });
        }

        // Regular GPIO pin → rppal
        let value = tokio::task::spawn_blocking(move || {
            let gpio = rppal::gpio::Gpio::new()?;
            let p = gpio.get(pin)?.into_input();
            Ok::<_, anyhow::Error>(match p.read() {
                rppal::gpio::Level::Low => 0u8,
                rppal::gpio::Level::High => 1u8,
            })
        })
        .await??;

        Ok(ToolResult {
            success: true,
            output: json!({ "pin": pin, "value": value, "state": if value == 0 { "LOW" } else { "HIGH" } }).to_string(),
            error: None,
        })
    }
}

// ─── Tool: gpio_rpi_blink ────────────────────────────────────────────────────

/// Blink a GPIO pin N times with configurable on/off timing via rppal.
pub struct GpioRpiBlinkTool;

#[async_trait]
impl Tool for GpioRpiBlinkTool {
    fn name(&self) -> &str {
        "gpio_rpi_blink"
    }

    fn description(&self) -> &str {
        "Blink a GPIO pin N times with configurable on/off durations on this Raspberry Pi. \
        Suitable for LEDs, buzzers, or any repeated toggle. Uses BCM pin numbers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "BCM GPIO number (e.g. 47 for ACT LED on RPi 3B)"
                },
                "times": {
                    "type": "integer",
                    "description": "Number of blink cycles (default 3)"
                },
                "on_ms": {
                    "type": "integer",
                    "description": "Milliseconds pin stays HIGH per cycle (default 500)"
                },
                "off_ms": {
                    "type": "integer",
                    "description": "Milliseconds pin stays LOW between cycles (default 500)"
                }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin_raw = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let pin = u8::try_from(pin_raw)
            .map_err(|_| anyhow::anyhow!("pin value {pin_raw} out of range for u8"))?;
        let times = args
            .get("times")
            .and_then(|v| v.as_u64())
            .unwrap_or(3)
            .min(100); // cap at 100 blinks to prevent runaway
        let on_ms = args
            .get("on_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .min(10_000); // cap at 10s
        let off_ms = args
            .get("off_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .min(10_000);

        // Onboard ACT LED → Linux LED subsystem (async-friendly, no spawn_blocking)
        if is_onboard_led(pin) {
            let path = led_brightness_path()
                .ok_or_else(|| anyhow::anyhow!("ACT LED sysfs path not found"))?;
            ensure_led_trigger_none();
            for _ in 0..times {
                fs::write(path, "1")?;
                tokio::time::sleep(Duration::from_millis(on_ms)).await;
                fs::write(path, "0")?;
                tokio::time::sleep(Duration::from_millis(off_ms)).await;
            }
            return Ok(ToolResult {
                success: true,
                output: format!(
                    "Blinked ACT LED (GPIO {}) × {} ({}/{}ms) via sysfs",
                    pin, times, on_ms, off_ms
                ),
                error: None,
            });
        }

        // Regular GPIO pin → rppal
        tokio::task::spawn_blocking(move || {
            let gpio = rppal::gpio::Gpio::new()?;
            let mut p = gpio.get(pin)?.into_output();
            for _ in 0..times {
                p.set_high();
                std::thread::sleep(Duration::from_millis(on_ms));
                p.set_low();
                std::thread::sleep(Duration::from_millis(off_ms));
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;

        Ok(ToolResult {
            success: true,
            output: format!("Blinked GPIO {} × {} ({}/{}ms)", pin, times, on_ms, off_ms),
            error: None,
        })
    }
}

// ─── Tool: rpi_system_info ───────────────────────────────────────────────────

/// Return current Raspberry Pi system information as JSON.
pub struct RpiSystemInfoTool;

#[async_trait]
impl Tool for RpiSystemInfoTool {
    fn name(&self) -> &str {
        "rpi_system_info"
    }

    fn description(&self) -> &str {
        "Get current system information for this Raspberry Pi: model, RAM, \
        CPU temperature, IP address, and WiFi interface."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let ctx = RpiSystemContext::discover()
            .ok_or_else(|| anyhow::anyhow!("Not running on a Raspberry Pi"))?;

        let info = json!({
            "model": ctx.model.display_name(),
            "hostname": ctx.hostname,
            "ip_address": ctx.ip_address,
            "wifi_interface": ctx.wifi_interface,
            "ram_total_mb": ctx.total_ram_mb,
            "ram_free_mb": ctx.free_ram_mb,
            "cpu_temp_celsius": ctx.cpu_temp_celsius,
            "gpio_available": ctx.gpio_available,
            "onboard_led_gpio": ctx.model.onboard_led_gpio(),
        });

        Ok(ToolResult {
            success: true,
            output: info.to_string(),
            error: None,
        })
    }
}
