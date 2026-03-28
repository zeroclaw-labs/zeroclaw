//! Hardware serial transport — newline-delimited JSON over USB CDC.
//!
//! Implements the [`Transport`] trait with **lazy port opening**: the port is
//! opened for each `send()` call and closed immediately after the response is
//! received. This means multiple tools can use the same device path without
//! one holding the port exclusively.
//!
//! Wire protocol (ZeroClaw serial JSON):
//! ```text
//! Host → Device:  {"cmd":"gpio_write","params":{"pin":25,"value":1}}\n
//! Device → Host:  {"ok":true,"data":{"pin":25,"value":1,"state":"HIGH"}}\n
//! ```
//!
//! All I/O is wrapped in `tokio::time::timeout` — no blocking reads.

use super::{
    protocol::{ZcCommand, ZcResponse},
    transport::{Transport, TransportError, TransportKind},
};
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_serial::SerialPortBuilderExt;

/// Default timeout for a single send→receive round-trip (seconds).
const SEND_TIMEOUT_SECS: u64 = 5;

/// Default baud rate for ZeroClaw serial devices.
pub const DEFAULT_BAUD: u32 = 115_200;

/// Timeout for the ping handshake during device discovery (milliseconds).
const PING_TIMEOUT_MS: u64 = 300;

/// Allowed serial device path prefixes — reject arbitrary paths for security.
/// Uses the shared allowlist from `crate::util`.
use crate::util::is_serial_path_allowed as is_path_allowed;

/// Serial transport for ZeroClaw hardware devices.
///
/// The port is **opened lazily** on each `send()` call and released immediately
/// after the response is read. This avoids exclusive-hold conflicts between
/// multiple tools or processes.
pub struct HardwareSerialTransport {
    port_path: String,
    baud_rate: u32,
}

impl HardwareSerialTransport {
    /// Create a new lazy-open serial transport.
    ///
    /// Does NOT open the port — that happens on the first `send()` call.
    pub fn new(port_path: impl Into<String>, baud_rate: u32) -> Self {
        Self {
            port_path: port_path.into(),
            baud_rate,
        }
    }

    /// Create with the default baud rate (115 200).
    pub fn with_default_baud(port_path: impl Into<String>) -> Self {
        Self::new(port_path, DEFAULT_BAUD)
    }

    /// Port path this transport is bound to.
    pub fn port_path(&self) -> &str {
        &self.port_path
    }

    /// Attempt a ping handshake to verify ZeroClaw firmware is running.
    ///
    /// Opens the port, sends `{"cmd":"ping","params":{}}`, waits up to
    /// `PING_TIMEOUT_MS` for a response with `data.firmware == "zeroclaw"`.
    ///
    /// Returns `true` if a ZeroClaw device responds, `false` otherwise.
    /// This method never returns an error — discovery must not hang on failure.
    pub async fn ping_handshake(&self) -> bool {
        let ping = ZcCommand::simple("ping");
        let json = match serde_json::to_string(&ping) {
            Ok(j) => j,
            Err(_) => return false,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(PING_TIMEOUT_MS),
            do_send(&self.port_path, self.baud_rate, &json),
        )
        .await;

        match result {
            Ok(Ok(resp)) => {
                // Accept if firmware field is "zeroclaw" (in data or top-level)
                resp.ok
                    && resp
                        .data
                        .get("firmware")
                        .and_then(|v| v.as_str())
                        .map(|s| s == "zeroclaw")
                        .unwrap_or(false)
            }
            _ => false,
        }
    }
}

#[async_trait]
impl Transport for HardwareSerialTransport {
    async fn send(&self, cmd: &ZcCommand) -> Result<ZcResponse, TransportError> {
        if !is_path_allowed(&self.port_path) {
            return Err(TransportError::Other(format!(
                "serial path not allowed: {}",
                self.port_path
            )));
        }

        let json = serde_json::to_string(cmd)
            .map_err(|e| TransportError::Protocol(format!("failed to serialize command: {e}")))?;
        // Log command name only — never log the full payload (may contain large or sensitive data).
        tracing::info!(port = %self.port_path, cmd = %cmd.cmd, "serial send");

        tokio::time::timeout(
            std::time::Duration::from_secs(SEND_TIMEOUT_SECS),
            do_send(&self.port_path, self.baud_rate, &json),
        )
        .await
        .map_err(|_| TransportError::Timeout(SEND_TIMEOUT_SECS))?
    }

    fn kind(&self) -> TransportKind {
        TransportKind::Serial
    }

    fn is_connected(&self) -> bool {
        // Lightweight connectivity check: the device file must exist.
        std::path::Path::new(&self.port_path).exists()
    }
}

/// Open the port, write the command, read one response line, return the parsed response.
///
/// This is the inner function wrapped with `tokio::time::timeout` by the caller.
/// Do NOT add a timeout here — the outer caller owns the deadline.
async fn do_send(path: &str, baud: u32, json: &str) -> Result<ZcResponse, TransportError> {
    // Open port lazily — released when this function returns
    let mut port = tokio_serial::new(path, baud)
        .open_native_async()
        .map_err(|e| {
            // Match on the error kind for robust cross-platform disconnect detection.
            match e.kind {
                tokio_serial::ErrorKind::NoDevice => TransportError::Disconnected,
                tokio_serial::ErrorKind::Io(io_kind) if io_kind == std::io::ErrorKind::NotFound => {
                    TransportError::Disconnected
                }
                _ => TransportError::Other(format!("failed to open {path}: {e}")),
            }
        })?;

    // Write command line
    port.write_all(format!("{json}\n").as_bytes())
        .await
        .map_err(TransportError::Io)?;
    port.flush().await.map_err(TransportError::Io)?;

    // Read response line — port is moved into BufReader; write phase complete
    let mut reader = BufReader::new(port);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .await
        .map_err(|e: std::io::Error| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                TransportError::Disconnected
            } else {
                TransportError::Io(e)
            }
        })?;

    let trimmed = response_line.trim();
    if trimmed.is_empty() {
        return Err(TransportError::Protocol(
            "empty response from device".to_string(),
        ));
    }

    serde_json::from_str(trimmed).map_err(|e| {
        TransportError::Protocol(format!("invalid JSON response: {e} — got: {trimmed:?}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_transport_new_stores_path_and_baud() {
        let t = HardwareSerialTransport::new("/dev/ttyACM0", 115_200);
        assert_eq!(t.port_path(), "/dev/ttyACM0");
        assert_eq!(t.baud_rate, 115_200);
    }

    #[test]
    fn serial_transport_default_baud() {
        let t = HardwareSerialTransport::with_default_baud("/dev/ttyACM0");
        assert_eq!(t.baud_rate, DEFAULT_BAUD);
    }

    #[test]
    fn serial_transport_kind_is_serial() {
        let t = HardwareSerialTransport::with_default_baud("/dev/ttyACM0");
        assert_eq!(t.kind(), TransportKind::Serial);
    }

    #[test]
    fn is_connected_false_for_nonexistent_path() {
        let t = HardwareSerialTransport::with_default_baud("/dev/ttyACM_does_not_exist_99");
        assert!(!t.is_connected());
    }

    #[test]
    fn allowed_paths_accept_valid_prefixes() {
        // Linux-only paths
        #[cfg(target_os = "linux")]
        {
            assert!(is_path_allowed("/dev/ttyACM0"));
            assert!(is_path_allowed("/dev/ttyUSB1"));
        }
        // macOS-only paths
        #[cfg(target_os = "macos")]
        {
            assert!(is_path_allowed("/dev/tty.usbmodem14101"));
            assert!(is_path_allowed("/dev/cu.usbmodem14201"));
            assert!(is_path_allowed("/dev/tty.usbserial-1410"));
            assert!(is_path_allowed("/dev/cu.usbserial-1410"));
        }
        // Windows-only paths
        #[cfg(target_os = "windows")]
        assert!(is_path_allowed("COM3"));
        // Cross-platform: macOS paths always work on macOS, Linux paths on Linux
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            assert!(is_path_allowed("/dev/ttyACM0"));
            assert!(is_path_allowed("/dev/tty.usbmodem14101"));
            assert!(is_path_allowed("COM3"));
        }
    }

    #[test]
    fn allowed_paths_reject_invalid_prefixes() {
        assert!(!is_path_allowed("/dev/sda"));
        assert!(!is_path_allowed("/etc/passwd"));
        assert!(!is_path_allowed("/tmp/evil"));
        assert!(!is_path_allowed(""));
    }

    #[tokio::test]
    async fn send_rejects_disallowed_path() {
        let t = HardwareSerialTransport::new("/dev/sda", 115_200);
        let result = t.send(&ZcCommand::simple("ping")).await;
        assert!(matches!(result, Err(TransportError::Other(_))));
    }

    #[tokio::test]
    async fn send_returns_disconnected_for_missing_device() {
        // Use a platform-appropriate path that passes the serialpath allowlist
        // but refers to a device that doesn't actually exist.
        #[cfg(target_os = "linux")]
        let path = "/dev/ttyACM_phase2_test_99";
        #[cfg(target_os = "macos")]
        let path = "/dev/tty.usbmodemfake9900";
        #[cfg(target_os = "windows")]
        let path = "COM99";
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let path = "/dev/ttyACM_phase2_test_99";

        let t = HardwareSerialTransport::new(path, 115_200);
        let result = t.send(&ZcCommand::simple("ping")).await;
        // Missing device → Disconnected or Timeout (system-dependent)
        assert!(
            matches!(
                result,
                Err(TransportError::Disconnected | TransportError::Timeout(_))
            ),
            "expected Disconnected or Timeout, got {result:?}"
        );
    }

    #[tokio::test]
    async fn ping_handshake_returns_false_for_missing_device() {
        #[cfg(target_os = "linux")]
        let path = "/dev/ttyACM_phase2_test_99";
        #[cfg(target_os = "macos")]
        let path = "/dev/tty.usbmodemfake9900";
        #[cfg(target_os = "windows")]
        let path = "COM99";
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let path = "/dev/ttyACM_phase2_test_99";

        let t = HardwareSerialTransport::new(path, 115_200);
        assert!(!t.ping_handshake().await);
    }
}
