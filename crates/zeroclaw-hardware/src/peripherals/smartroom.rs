//! High-level smart-room device tools — the LLM picks device NAMES; the wiring
//! (pin numbers) is hidden inside the tool. Eliminates "model guessed the wrong
//! pin from training priors" failures.
//!
//! Wired up by [`super::create_peripheral_tools`] when a board with name
//! `esp32-sim` (or alias `smartroom`) is configured. The pin map is hard-coded
//! to match the demo's `esp32_sim` example simulator. For a real board you'd
//! either swap this implementation or read the map from the firmware's
//! `capabilities` response and key on `pin_devices`.

use super::serial::SerialTransport;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Pin mapping for the smart-room demo board. Output devices only — the
/// motion sensor uses `read_device`.
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

/// Tool: set a smart-room device on or off. Hides pin numbers from the model.
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
         the device is already in the desired state. Each user request that \
         names an on/off action MUST result in a fresh `set_device` call."
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
            .ok_or_else(|| anyhow::anyhow!("Missing 'device' parameter"))?;
        let state = args
            .get("state")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'state' parameter"))?;

        let pin = output_pin(device).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown device '{}'. Available: reading_lamp, overhead_light, heater, fan",
                device
            )
        })?;
        let value: u64 = match state {
            "on" => 1,
            "off" => 0,
            other => anyhow::bail!("state must be 'on' or 'off', got '{}'", other),
        };

        let result = self
            .transport
            .request("gpio_write", json!({ "pin": pin, "value": value }))
            .await?;
        if result.success {
            Ok(ToolResult {
                success: true,
                output: format!("{} → {}", device, state),
                error: None,
            })
        } else {
            Ok(result)
        }
    }
}

/// Tool: read a smart-room sensor device by NAME (currently just motion_sensor).
pub struct ReadDeviceTool {
    pub transport: Arc<SerialTransport>,
}

#[async_trait]
impl Tool for ReadDeviceTool {
    fn name(&self) -> &str {
        "read_device"
    }

    fn description(&self) -> &str {
        "Read a smart-room input device by NAME. Currently the only readable \
         device is `motion_sensor` (returns 1 when presence detected, 0 otherwise)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "enum": ["motion_sensor"],
                    "description": "Sensor name."
                }
            },
            "required": ["device"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let device = args
            .get("device")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'device' parameter"))?;
        let pin = input_pin(device).ok_or_else(|| {
            anyhow::anyhow!("Unknown sensor '{}'. Available: motion_sensor", device)
        })?;
        self.transport
            .request("gpio_read", json!({ "pin": pin }))
            .await
    }
}
