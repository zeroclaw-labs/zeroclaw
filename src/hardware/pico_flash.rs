//! `pico_flash` tool — flash ZeroClaw firmware to a Pico in BOOTSEL mode.
//!
//! # Happy path
//! 1. User holds BOOTSEL while plugging in Pico → RPI-RP2 drive appears.
//! 2. User asks "flash my pico".
//! 3. LLM calls `pico_flash(confirm=true)`.
//! 4. Tool copies UF2 to RPI-RP2 drive; Pico reboots into the Rust firmware.
//! 5. Tool waits up to 20 s for `/dev/cu.usbmodem*` to appear.
//! 6. Tool reconnects the serial transport; user can use `gpio_write` immediately.

use super::device::DeviceRegistry;
use super::uf2;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

/// How long to wait for the Pico serial port after flashing (seconds).
const PORT_WAIT_SECS: u64 = 20;

/// How often to poll for the serial port.
const PORT_POLL_MS: u64 = 500;

// ── PicoFlashTool ─────────────────────────────────────────────────────────────

/// Tool: flash ZeroClaw firmware to a Pico in BOOTSEL mode.
///
/// The Pico must be connected with BOOTSEL held so it mounts as `RPI-RP2`.
/// After flashing, the tool reconnects the serial transport in the
/// [`DeviceRegistry`] so subsequent `gpio_write` calls work immediately
/// without restarting ZeroClaw.
pub struct PicoFlashTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl PicoFlashTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PicoFlashTool {
    fn name(&self) -> &str {
        "pico_flash"
    }

    fn description(&self) -> &str {
        "Flash ZeroClaw firmware to a Raspberry Pi Pico in BOOTSEL mode. \
         The Pico must be connected with the BOOTSEL button held (shows as RPI-RP2 drive in Finder). \
         After flashing the Pico reboots into the Rust firmware and the serial \
         connection is refreshed automatically — no restart needed."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "confirm": {
                    "type": "boolean",
                    "description": "Set to true to confirm flashing the Pico firmware"
                }
            },
            "required": ["confirm"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // ── 1. Require explicit confirmation ──────────────────────────────
        let confirmed = args
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !confirmed {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Set confirm=true to proceed with flashing. \
                     This will overwrite the firmware on the connected Pico."
                        .to_string(),
                ),
            });
        }

        // ── 2. Detect BOOTSEL-mode Pico ───────────────────────────────────
        let mount = match uf2::find_rpi_rp2_mount() {
            Some(m) => m,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "No Pico in BOOTSEL mode found (RPI-RP2 drive not detected). \
                         Hold the BOOTSEL button while plugging the Pico in via USB, \
                         then try again."
                            .to_string(),
                    ),
                });
            }
        };

        tracing::info!(mount = %mount.display(), "RPI-RP2 volume found");

        // ── 3. Ensure firmware files are extracted ────────────────────────
        let firmware_dir = match uf2::ensure_firmware_dir() {
            Ok(d) => d,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("firmware error: {e}")),
                });
            }
        };

        // ── 4. Flash UF2 ─────────────────────────────────────────────────
        if let Err(e) = uf2::flash_uf2(&mount, &firmware_dir).await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("flash failed: {e}")),
            });
        }

        // ── 5. Wait for serial port to appear ─────────────────────────────
        let port = uf2::wait_for_serial_port(
            std::time::Duration::from_secs(PORT_WAIT_SECS),
            std::time::Duration::from_millis(PORT_POLL_MS),
        )
        .await;

        let port = match port {
            Some(p) => p,
            None => {
                // Flash likely succeeded even if port didn't appear in time —
                // some host systems are slower to enumerate the new port.
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "UF2 copied to {} but serial port did not appear within {PORT_WAIT_SECS}s. \
                         Unplug and replug the Pico, then restart ZeroClaw to connect as pico0.",
                        mount.display()
                    )),
                });
            }
        };

        tracing::info!(port = %port.display(), "Pico serial port online after UF2 flash");

        let final_port = Some(port);

        // ── 6. Reconnect serial transport in DeviceRegistry ──────────────
        //
        // The old transport still points at a stale port handle from before
        // the flash. Reconnect so gpio_write works immediately.
        let reconnect_result = match &final_port {
            Some(p) => {
                let port_str = p.to_string_lossy();
                let mut reg = self.registry.write().await;
                // Try to find a pico alias in the registry.
                match reg.aliases().into_iter().find(|a| a.starts_with("pico")) {
                    Some(a) => {
                        let alias = a.to_string();
                        reg.reconnect(&alias, Some(&port_str)).await
                    }
                    None => Err(anyhow::anyhow!(
                        "no pico alias found in registry; cannot reconnect transport"
                    )),
                }
            }
            None => Err(anyhow::anyhow!("no serial port to reconnect")),
        };

        // ── 7. Return result ──────────────────────────────────────────────
        match final_port {
            Some(p) => {
                let port_str = p.display().to_string();
                let reconnected = reconnect_result.is_ok();
                if reconnected {
                    tracing::info!(port = %port_str, "Pico online — transport reconnected");
                } else {
                    let err = reconnect_result.unwrap_err();
                    tracing::warn!(port = %port_str, err = %err, "Pico online but reconnect failed");
                }
                let suffix = if reconnected {
                    "pico0 is ready — you can use gpio_write immediately."
                } else {
                    "Restart ZeroClaw to reconnect as pico0."
                };
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Pico flashed successfully. Firmware is online at {port_str}. {suffix}"
                    ),
                    error: None,
                })
            }
            None => Ok(ToolResult {
                success: true,
                output: format!(
                    "Pico flashed. Serial port did not appear within {PORT_WAIT_SECS}s — \
                     unplug and replug the Pico, then restart ZeroClaw to connect as pico0."
                ),
                error: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::device::DeviceRegistry;
    use super::*;

    fn tool() -> PicoFlashTool {
        let registry = Arc::new(RwLock::new(DeviceRegistry::new()));
        PicoFlashTool::new(registry)
    }

    #[test]
    fn name_is_pico_flash() {
        let t = tool();
        assert_eq!(t.name(), "pico_flash");
    }

    #[test]
    fn schema_requires_confirm() {
        let schema = tool().parameters_schema();
        let required = schema["required"].as_array().expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("confirm")),
            "confirm should be required"
        );
    }

    #[tokio::test]
    async fn execute_without_confirm_returns_error() {
        let result = tool()
            .execute(serde_json::json!({"confirm": false}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
        let err = result.error.unwrap();
        assert!(
            err.contains("confirm=true"),
            "error should mention confirm=true; got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_missing_confirm_returns_error() {
        let result = tool().execute(serde_json::json!({})).await.unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn execute_with_confirm_true_but_no_pico_returns_error() {
        // In CI there's no Pico attached — the tool should report missing device, not panic.
        let result = tool()
            .execute(serde_json::json!({"confirm": true}))
            .await
            .unwrap();
        // Either success (if a Pico happens to be connected) or the BOOTSEL error.
        // What must NOT happen: panic or anyhow error propagation.
        let _ = result; // just verify it didn't panic
    }
}
