//! High-level smart-room device tools for ESP32 boards.
//!
//! Provides `set_device` and `read_device` tools that let the LLM
//! reason in terms of named devices (e.g. "reading_lamp", "fan")
//! instead of raw pin numbers. This eliminates the common failure mode
//! where the model guesses the wrong pin based on training priors.
//!
//! These tools are automatically registered when a board with
//! `board = "esp32"` or `board = "esp32-sim"` is configured.

use super::serial::SerialTransport;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::tool::{Tool, ToolResult};

/// Pin mapping for the smart-room demo board.
///
/// This mapping is intentionally hardcoded for the specific ESP32 demo board
/// used in the hackathon vignette. If the physical wiring on a board changes,
/// both this table and the firmware must be kept in sync.
///
/// For dynamic discovery of named devices, prefer the `pin_devices` map
/// returned by the `hardware_capabilities` tool (see the companion PR that
/// surfaces this field).
fn output_pin(device: &str) -> Option<u8> {
    match device {
        "reading_lamp" | "lamp" | "reading lamp" => Some(12),
        "overhead_light" | "overhead" | "ceiling" | "ceiling_light" => Some(13),
        "heater" | "space_heater" => Some(14),
        "fan" | "status_led" | "fan_led" => Some(2),
        _ => None,
    }
}

fn input_pin(device: &str) -> Option<u8> {
    match device {
        "motion_sensor" | "motion" | "presence" | "pir" => Some(5),
        _ => None,
    }
}

/// Tool: set a smart-room device on or off by name.
pub struct SetDeviceTool {
    pub transport: Arc<SerialTransport>,
}

#[async_trait]
impl Tool for SetDeviceTool {
    fn name(&self) -> &str {
        "set_device"
    }

    fn description(&self) -> &str {
        "Turn a smart-room device on or off by NAME. The hardware pin wiring \
         is handled internally — you do NOT pick pin numbers. \
         Available devices: reading_lamp, overhead_light, heater, fan. \
         For the motion sensor use `read_device` instead. \
         IMPORTANT: ALWAYS call this tool when the user asks to change device \
         state — do NOT skip the call just because conversation history suggests \
         the device is already in the desired state."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "enum": ["reading_lamp", "overhead_light", "heater", "fan"],
                    "description": "Device name. reading_lamp = warm lamp by the chair; overhead_light = bright ceiling; heater = space heater; fan = cooling fan with status LED."
                },
                "state": {
                    "type": "string",
                    "enum": ["on", "off"],
                    "description": "on = energize, off = de-energize"
                }
            },
            "required": ["device", "state"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let device = args
            .get("device")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("missing device"))?;

        let state = args
            .get("state")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("missing state"))?;

        let pin = output_pin(device)
            .ok_or_else(|| anyhow::Error::msg(format!("unknown output device: {}", device)))?;

        let value = match state {
            "on" => 1,
            "off" => 0,
            _ => anyhow::bail!("state must be 'on' or 'off'"),
        };

        let result = self
            .transport
            .request("gpio_write", json!({ "pin": pin, "value": value }))
            .await?;

        Ok(result)
    }
}

/// Tool: read a smart-room input device (currently only motion_sensor).
pub struct ReadDeviceTool {
    pub transport: Arc<SerialTransport>,
}

#[async_trait]
impl Tool for ReadDeviceTool {
    fn name(&self) -> &str {
        "read_device"
    }

    fn description(&self) -> &str {
        "Read the current state of a smart-room input device by NAME. \
         Currently only the motion_sensor is supported as an input device."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "enum": ["motion_sensor"],
                    "description": "Input device name. Only motion_sensor is supported."
                }
            },
            "required": ["device"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let device = args
            .get("device")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("missing device"))?;

        let pin = input_pin(device)
            .ok_or_else(|| anyhow::Error::msg(format!("unknown input device: {}", device)))?;

        let result = self
            .transport
            .request("gpio_read", json!({ "pin": pin }))
            .await?;

        Ok(result)
    }
}

impl Attributable for SetDeviceTool {
    fn role(&self) -> Role {
        Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
    }
    fn alias(&self) -> &str {
        "set_device"
    }
}

impl Attributable for ReadDeviceTool {
    fn role(&self) -> Role {
        Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
    }
    fn alias(&self) -> &str {
        "read_device"
    }
}
