//! Transport trait — decouples hardware tools from wire protocol.

use super::protocol::{ZcCommand, ZcResponse};
use async_trait::async_trait;
use thiserror::Error;

/// Transport layer error.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Operation timed out.
    #[error("transport timeout after {secs}s: {detail}")]
    Timeout { secs: u64, detail: String },

    /// Transport is disconnected or device was removed.
    #[error("transport disconnected")]
    Disconnected,

    /// Protocol-level error (malformed JSON, id mismatch, etc.).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Underlying I/O error.
    #[error("transport I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Catch-all for transport-specific errors.
    #[error("{0}")]
    Other(String),
}

/// Transport kind discriminator.
/// Used for capability matching — some tools require a specific transport
/// (e.g. `pico_flash` requires UF2, `memory_read` prefers SWD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportKind {
    /// Newline-delimited JSON over USB CDC serial.
    Serial,
    /// SWD debug probe (probe-rs).
    Swd,
    /// UF2 mass storage firmware flashing.
    Uf2,
    /// Direct Linux GPIO/I2C/SPI (rppal, sysfs).
    Native,
    /// Total Phase Aardvark USB adapter (I2C/SPI/GPIO via C library).
    Aardvark,
}

impl std::fmt::Display for TransportKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serial => write!(f, "serial"),
            Self::Swd => write!(f, "swd"),
            Self::Uf2 => write!(f, "uf2"),
            Self::Native => write!(f, "native"),
            Self::Aardvark => write!(f, "aardvark"),
        }
    }
}

/// Transport trait — sends commands to a hardware device and receives responses.
/// All implementations MUST use explicit `tokio::time::timeout` on I/O operations.
/// Callers should never assume success; always handle `TransportError`.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send a command to the device and receive the response.
    async fn send(&self, cmd: &ZcCommand) -> Result<ZcResponse, TransportError>;

    /// What kind of transport this is.
    fn kind(&self) -> TransportKind;

    /// Whether the transport is currently connected to a device.
    fn is_connected(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_kind_display() {
        assert_eq!(TransportKind::Serial.to_string(), "serial");
        assert_eq!(TransportKind::Swd.to_string(), "swd");
        assert_eq!(TransportKind::Uf2.to_string(), "uf2");
        assert_eq!(TransportKind::Native.to_string(), "native");
    }

    #[test]
    fn transport_error_display() {
        let err = TransportError::Timeout {
            secs: 5,
            detail: "deadline has elapsed".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "transport timeout after 5s: deadline has elapsed"
        );

        let err = TransportError::Disconnected;
        assert_eq!(err.to_string(), "transport disconnected");

        let err = TransportError::Protocol("bad json".into());
        assert_eq!(err.to_string(), "protocol error: bad json");

        let err = TransportError::Other("custom".into());
        assert_eq!(err.to_string(), "custom");
    }

    #[test]
    fn transport_kind_equality() {
        assert_eq!(TransportKind::Serial, TransportKind::Serial);
        assert_ne!(TransportKind::Serial, TransportKind::Swd);
    }

    /// Regression: a serial deadline must keep the `Timeout` classification
    /// so callers can still match on it. Evolving the variant to carry detail
    /// must not turn it into catch-all `Other`.
    #[test]
    fn timeout_variant_preserves_typed_classification() {
        let err = TransportError::Timeout {
            secs: 3,
            detail: "test detail".into(),
        };
        assert!(
            matches!(err, TransportError::Timeout { .. }),
            "Timeout must remain matchable as Timeout, got {err:?}"
        );
    }
}
