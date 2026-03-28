//! Aardvark hardware tools — I2C, SPI, and GPIO operations via the Total Phase
//! Aardvark USB adapter.
//!
//! All tools follow the same pattern as the built-in GPIO tools:
//! 1. Accept an optional `device` alias parameter.
//! 2. Resolve the Aardvark device from the [`DeviceRegistry`].
//! 3. Build a [`ZcCommand`] and send it through the registered transport.
//! 4. Return a [`ToolResult`] with human-readable output.
//!
//! These tools are only registered when at least one Aardvark adapter is
//! detected at startup (see [`DeviceRegistry::has_aardvark`]).

use super::device::DeviceRegistry;
use super::protocol::ZcCommand;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Factory ───────────────────────────────────────────────────────────────────

/// Build the five Aardvark hardware tools.
///
/// Called from [`ToolRegistry::load`] when an Aardvark adapter is present.
pub fn aardvark_tools(devices: Arc<RwLock<DeviceRegistry>>) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(I2cScanTool::new(devices.clone())),
        Box::new(I2cReadTool::new(devices.clone())),
        Box::new(I2cWriteTool::new(devices.clone())),
        Box::new(SpiTransferTool::new(devices.clone())),
        Box::new(GpioAardvarkTool::new(devices.clone())),
    ]
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Resolve the Aardvark device from args and return an owned `DeviceContext`.
///
/// Thin wrapper so individual tool `execute` methods don't duplicate the logic.
async fn resolve(
    registry: &Arc<RwLock<DeviceRegistry>>,
    args: &serde_json::Value,
) -> Result<(String, super::device::DeviceContext), ToolResult> {
    let reg = registry.read().await;
    reg.resolve_aardvark_device(args).map_err(|msg| ToolResult {
        success: false,
        output: String::new(),
        error: Some(msg),
    })
}

// ── I2cScanTool ───────────────────────────────────────────────────────────────

/// Tool: scan the I2C bus for responding device addresses.
pub struct I2cScanTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl I2cScanTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for I2cScanTool {
    fn name(&self) -> &str {
        "i2c_scan"
    }

    fn description(&self) -> &str {
        "Scan the I2C bus via the Aardvark USB adapter and return all responding \
         device addresses in hex (e.g. [0x48, 0x68])"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Aardvark device alias (e.g. aardvark0). Omit to auto-select."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let (_alias, ctx) = match resolve(&self.registry, &args).await {
            Ok(v) => v,
            Err(result) => return Ok(result),
        };

        let cmd = ZcCommand::simple("i2c_scan");
        match ctx.transport.send(&cmd).await {
            Ok(resp) if resp.ok => {
                let devices = resp
                    .data
                    .get("devices")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let output = if devices.is_empty() {
                    "I2C scan complete — no devices found on the bus.".to_string()
                } else {
                    let addrs: Vec<&str> = devices.iter().filter_map(|v| v.as_str()).collect();
                    format!(
                        "I2C scan found {} device(s): {}",
                        addrs.len(),
                        addrs.join(", ")
                    )
                };
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Ok(resp) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    resp.error
                        .unwrap_or_else(|| "i2c_scan: device returned ok:false".to_string()),
                ),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("transport error: {e}")),
            }),
        }
    }
}

// ── I2cReadTool ───────────────────────────────────────────────────────────────

/// Tool: read bytes from an I2C device register.
pub struct I2cReadTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl I2cReadTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for I2cReadTool {
    fn name(&self) -> &str {
        "i2c_read"
    }

    fn description(&self) -> &str {
        "Read bytes from an I2C device via the Aardvark USB adapter. \
         Provide the I2C address and optionally a register to read from."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Aardvark device alias (e.g. aardvark0). Omit to auto-select."
                },
                "addr": {
                    "type": "integer",
                    "description": "I2C device address (e.g. 72 for 0x48)"
                },
                "register": {
                    "type": "integer",
                    "description": "Register address to read from (optional)"
                },
                "len": {
                    "type": "integer",
                    "description": "Number of bytes to read",
                    "default": 1
                }
            },
            "required": ["addr"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let addr = match args.get("addr").and_then(|v| v.as_u64()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: addr".to_string()),
                });
            }
        };
        let len = args.get("len").and_then(|v| v.as_u64()).unwrap_or(1);

        let (_alias, ctx) = match resolve(&self.registry, &args).await {
            Ok(v) => v,
            Err(result) => return Ok(result),
        };

        let mut params = json!({ "addr": addr, "len": len });
        if let Some(reg) = args.get("register").and_then(|v| v.as_u64()) {
            params["register"] = json!(reg);
        }
        let cmd = ZcCommand::new("i2c_read", params);

        match ctx.transport.send(&cmd).await {
            Ok(resp) if resp.ok => {
                let hex = resp
                    .data
                    .get("hex")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_else(|| "?".to_string());
                Ok(ToolResult {
                    success: true,
                    output: format!("I2C read from addr {addr:#04x}: [{hex}]"),
                    error: None,
                })
            }
            Ok(resp) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    resp.error
                        .unwrap_or_else(|| "i2c_read: device returned ok:false".to_string()),
                ),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("transport error: {e}")),
            }),
        }
    }
}

// ── I2cWriteTool ──────────────────────────────────────────────────────────────

/// Tool: write bytes to an I2C device.
pub struct I2cWriteTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl I2cWriteTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for I2cWriteTool {
    fn name(&self) -> &str {
        "i2c_write"
    }

    fn description(&self) -> &str {
        "Write bytes to an I2C device via the Aardvark USB adapter"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Aardvark device alias (e.g. aardvark0). Omit to auto-select."
                },
                "addr": {
                    "type": "integer",
                    "description": "I2C device address (e.g. 72 for 0x48)"
                },
                "bytes": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Bytes to write (e.g. [1, 96] for register 0x01 config 0x60)"
                }
            },
            "required": ["addr", "bytes"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let addr = match args.get("addr").and_then(|v| v.as_u64()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: addr".to_string()),
                });
            }
        };
        let bytes = match args.get("bytes").and_then(|v| v.as_array()) {
            Some(b) => b.clone(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: bytes".to_string()),
                });
            }
        };

        let (_alias, ctx) = match resolve(&self.registry, &args).await {
            Ok(v) => v,
            Err(result) => return Ok(result),
        };

        let cmd = ZcCommand::new("i2c_write", json!({ "addr": addr, "bytes": bytes }));

        match ctx.transport.send(&cmd).await {
            Ok(resp) if resp.ok => {
                let n = resp
                    .data
                    .get("bytes_written")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(bytes.len() as u64);
                Ok(ToolResult {
                    success: true,
                    output: format!("I2C write to addr {addr:#04x}: {n} byte(s) written"),
                    error: None,
                })
            }
            Ok(resp) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    resp.error
                        .unwrap_or_else(|| "i2c_write: device returned ok:false".to_string()),
                ),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("transport error: {e}")),
            }),
        }
    }
}

// ── SpiTransferTool ───────────────────────────────────────────────────────────

/// Tool: full-duplex SPI transfer.
pub struct SpiTransferTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl SpiTransferTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SpiTransferTool {
    fn name(&self) -> &str {
        "spi_transfer"
    }

    fn description(&self) -> &str {
        "Perform a full-duplex SPI transfer via the Aardvark USB adapter. \
         Sends the given bytes and returns the received bytes (same length)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Aardvark device alias (e.g. aardvark0). Omit to auto-select."
                },
                "bytes": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Bytes to send (received bytes have the same length)"
                }
            },
            "required": ["bytes"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let bytes = match args.get("bytes").and_then(|v| v.as_array()) {
            Some(b) => b.clone(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: bytes".to_string()),
                });
            }
        };

        let (_alias, ctx) = match resolve(&self.registry, &args).await {
            Ok(v) => v,
            Err(result) => return Ok(result),
        };

        let cmd = ZcCommand::new("spi_transfer", json!({ "bytes": bytes }));

        match ctx.transport.send(&cmd).await {
            Ok(resp) if resp.ok => {
                let hex = resp
                    .data
                    .get("hex")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_else(|| "?".to_string());
                Ok(ToolResult {
                    success: true,
                    output: format!("SPI transfer complete. Received: [{hex}]"),
                    error: None,
                })
            }
            Ok(resp) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    resp.error
                        .unwrap_or_else(|| "spi_transfer: device returned ok:false".to_string()),
                ),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("transport error: {e}")),
            }),
        }
    }
}

// ── GpioAardvarkTool ──────────────────────────────────────────────────────────

/// Tool: set or read the Aardvark adapter's GPIO pins.
///
/// The Aardvark has 8 GPIO pins accessible via the 10-pin expansion header.
/// Each pin can be configured as input or output via bitmasks.
pub struct GpioAardvarkTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl GpioAardvarkTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for GpioAardvarkTool {
    fn name(&self) -> &str {
        "gpio_aardvark"
    }

    fn description(&self) -> &str {
        "Set or read the Aardvark USB adapter GPIO pins via bitmasks. \
         Use action='set' with direction and value bitmasks to drive output pins, \
         or action='get' to read current pin states."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Aardvark device alias (e.g. aardvark0). Omit to auto-select."
                },
                "action": {
                    "type": "string",
                    "enum": ["set", "get"],
                    "description": "'set' to write GPIO pins, 'get' to read pin states"
                },
                "direction": {
                    "type": "integer",
                    "description": "For action='set': bitmask of output pins (1=output, 0=input)"
                },
                "value": {
                    "type": "integer",
                    "description": "For action='set': bitmask of output pin levels (1=high, 0=low)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: action".to_string()),
                });
            }
        };

        let (_alias, ctx) = match resolve(&self.registry, &args).await {
            Ok(v) => v,
            Err(result) => return Ok(result),
        };

        let cmd = match action.as_str() {
            "set" => {
                let direction = args.get("direction").and_then(|v| v.as_u64()).unwrap_or(0);
                let value = args.get("value").and_then(|v| v.as_u64()).unwrap_or(0);
                ZcCommand::new(
                    "gpio_set",
                    json!({ "direction": direction, "value": value }),
                )
            }
            "get" => ZcCommand::simple("gpio_get"),
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("unknown action '{other}'; use 'set' or 'get'")),
                });
            }
        };

        match ctx.transport.send(&cmd).await {
            Ok(resp) if resp.ok => {
                let output = if action == "get" {
                    let val = resp.data.get("value").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("Aardvark GPIO pins: {val:#010b} (0x{val:02x})")
                } else {
                    let dir = resp
                        .data
                        .get("direction")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let val = resp.data.get("value").and_then(|v| v.as_u64()).unwrap_or(0);
                    format!("Aardvark GPIO set — direction: {dir:#010b}, value: {val:#010b}")
                };
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Ok(resp) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    resp.error
                        .unwrap_or_else(|| "gpio_aardvark: device returned ok:false".to_string()),
                ),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("transport error: {e}")),
            }),
        }
    }
}
