//! AardvarkTransport — implements the Transport trait for Total Phase Aardvark USB adapters.
//!
//! The Aardvark is NOT a microcontroller firmware target; it is a USB bridge
//! that speaks I2C / SPI / GPIO directly.  Unlike [`HardwareSerialTransport`],
//! this transport interprets [`ZcCommand`] locally and calls the Aardvark C
//! library (via [`aardvark_sys`]) rather than forwarding JSON over a serial wire.
//!
//! Lazy-open strategy: a fresh [`aardvark_sys::AardvarkHandle`] is opened at
//! the start of each [`send`](AardvarkTransport::send) call and automatically
//! closed (dropped) before the call returns.  No persistent handle is held,
//! matching the design of [`HardwareSerialTransport`].

use super::protocol::{ZcCommand, ZcResponse};
use super::transport::{Transport, TransportError, TransportKind};
use aardvark_sys::AardvarkHandle;
use async_trait::async_trait;

/// Transport implementation for Total Phase Aardvark USB adapters.
///
/// Supports I2C, SPI, and direct GPIO operations via the Aardvark C library.
pub struct AardvarkTransport {
    /// Aardvark port index (0 = first available adapter).
    port: i32,
    /// Default I2C / SPI bitrate in kHz (e.g. 100 for standard-mode I2C).
    bitrate_khz: u32,
}

impl AardvarkTransport {
    /// Create a new transport for the given port and bitrate.
    ///
    /// The port number matches the index returned by
    /// [`AardvarkHandle::find_devices`].
    pub fn new(port: i32, bitrate_khz: u32) -> Self {
        Self { port, bitrate_khz }
    }

    /// Return `true` when at least one Aardvark adapter is found by the SDK.
    pub fn probe_connected(&self) -> bool {
        AardvarkHandle::find_devices()
            .into_iter()
            .any(|p| i32::from(p) == self.port || self.port == 0)
    }

    /// Open a fresh handle for one transaction.
    fn open_handle(&self) -> Result<AardvarkHandle, TransportError> {
        AardvarkHandle::open_port(self.port)
            .map_err(|e| TransportError::Other(format!("aardvark open: {e}")))
    }
}

#[async_trait]
impl Transport for AardvarkTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Aardvark
    }

    fn is_connected(&self) -> bool {
        !AardvarkHandle::find_devices().is_empty()
    }

    async fn send(&self, cmd: &ZcCommand) -> Result<ZcResponse, TransportError> {
        // Open a fresh handle per command — released when this scope ends.
        let handle = self.open_handle()?;

        let result: serde_json::Value = match cmd.cmd.as_str() {
            // ── I2C ──────────────────────────────────────────────────────────
            "i2c_scan" => {
                handle
                    .i2c_enable(self.bitrate_khz)
                    .map_err(|e| TransportError::Other(e.to_string()))?;
                let devices: Vec<String> = handle
                    .i2c_scan()
                    .into_iter()
                    .map(|a| format!("{a:#04x}"))
                    .collect();
                serde_json::json!({ "ok": true, "data": { "devices": devices } })
            }

            "i2c_read" => {
                let addr = required_u8(&cmd.params, "addr")?;
                let reg = optional_u8(&cmd.params, "register");
                let len: usize = cmd
                    .params
                    .get("len")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .try_into()
                    .unwrap_or(1);

                handle
                    .i2c_enable(self.bitrate_khz)
                    .map_err(|e| TransportError::Other(e.to_string()))?;

                let data = if let Some(r) = reg {
                    handle.i2c_write_read(addr, &[r], len)
                } else {
                    handle.i2c_read(addr, len)
                }
                .map_err(|e| TransportError::Other(e.to_string()))?;

                let hex: Vec<String> = data.iter().map(|b| format!("{b:#04x}")).collect();
                serde_json::json!({
                    "ok": true,
                    "data": { "bytes": data, "hex": hex }
                })
            }

            "i2c_write" => {
                let addr = required_u8(&cmd.params, "addr")?;
                let bytes = required_byte_array(&cmd.params, "bytes")?;

                handle
                    .i2c_enable(self.bitrate_khz)
                    .map_err(|e| TransportError::Other(e.to_string()))?;
                handle
                    .i2c_write(addr, &bytes)
                    .map_err(|e| TransportError::Other(e.to_string()))?;

                serde_json::json!({
                    "ok": true,
                    "data": { "bytes_written": bytes.len() }
                })
            }

            // ── SPI ──────────────────────────────────────────────────────────
            "spi_transfer" => {
                let bytes = required_byte_array(&cmd.params, "bytes")?;

                handle
                    .spi_enable(self.bitrate_khz)
                    .map_err(|e| TransportError::Other(e.to_string()))?;
                let recv = handle
                    .spi_transfer(&bytes)
                    .map_err(|e| TransportError::Other(e.to_string()))?;

                let hex: Vec<String> = recv.iter().map(|b| format!("{b:#04x}")).collect();
                serde_json::json!({
                    "ok": true,
                    "data": { "received": recv, "hex": hex }
                })
            }

            // ── GPIO ─────────────────────────────────────────────────────────
            "gpio_set" => {
                let direction = required_u8(&cmd.params, "direction")?;
                let value = required_u8(&cmd.params, "value")?;

                handle
                    .gpio_set(direction, value)
                    .map_err(|e| TransportError::Other(e.to_string()))?;

                serde_json::json!({
                    "ok": true,
                    "data": { "direction": direction, "value": value }
                })
            }

            "gpio_get" => {
                let val = handle
                    .gpio_get()
                    .map_err(|e| TransportError::Other(e.to_string()))?;

                serde_json::json!({
                    "ok": true,
                    "data": { "value": val }
                })
            }

            unknown => serde_json::json!({
                "ok": false,
                "error": format!("unknown Aardvark command: {unknown}")
            }),
        };

        // Drop handle here (auto-close via Drop).
        Ok(ZcResponse {
            ok: result["ok"].as_bool().unwrap_or(false),
            data: result["data"].clone(),
            error: result["error"].as_str().map(String::from),
        })
    }
}

// ── Parameter helpers ─────────────────────────────────────────────────────────

/// Extract a required `u8` field from JSON params, returning a `TransportError`
/// if missing or out of range.
fn required_u8(params: &serde_json::Value, key: &str) -> Result<u8, TransportError> {
    params
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| {
            TransportError::Protocol(format!("missing or out-of-range u8 parameter: '{key}'"))
        })
}

/// Extract an optional `u8` field — returns `None` if absent or not representable as u8.
fn optional_u8(params: &serde_json::Value, key: &str) -> Option<u8> {
    params
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|n| u8::try_from(n).ok())
}

/// Extract a required JSON array of integers as `Vec<u8>`.
fn required_byte_array(params: &serde_json::Value, key: &str) -> Result<Vec<u8>, TransportError> {
    let arr = params
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| TransportError::Protocol(format!("missing array parameter: '{key}'")))?;

    arr.iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or_else(|| {
                    TransportError::Protocol(format!(
                        "byte at index {i} in '{key}' is not a valid u8"
                    ))
                })
        })
        .collect()
}
