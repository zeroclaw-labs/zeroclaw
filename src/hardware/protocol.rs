//! ZeroClaw serial JSON protocol — the firmware contract.
//!
//! These types define the newline-delimited JSON wire format shared between
//! the ZeroClaw host and device firmware (Pico, Arduino, ESP32, Nucleo).
//!
//! Wire format:
//!   Host → Device:  `{"cmd":"gpio_write","params":{"pin":25,"value":1}}\n`
//!   Device → Host:  `{"ok":true,"data":{"pin":25,"value":1,"state":"HIGH"}}\n`
//!
//! Both sides MUST agree on these struct definitions. Any change here is a
//! breaking firmware contract change.

use serde::{Deserialize, Serialize};

/// Host-to-device command.
///
/// Serialized as one JSON line terminated by `\n`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZcCommand {
    /// Command name (e.g. `"gpio_read"`, `"ping"`, `"reboot_bootsel"`).
    pub cmd: String,
    /// Command parameters — schema depends on the command.
    #[serde(default)]
    pub params: serde_json::Value,
}

impl ZcCommand {
    /// Create a new command with the given name and parameters.
    pub fn new(cmd: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            cmd: cmd.into(),
            params,
        }
    }

    /// Create a parameterless command (e.g. `ping`, `capabilities`).
    pub fn simple(cmd: impl Into<String>) -> Self {
        Self {
            cmd: cmd.into(),
            params: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Device-to-host response.
///
/// Serialized as one JSON line terminated by `\n`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZcResponse {
    /// Whether the command succeeded.
    pub ok: bool,
    /// Response payload — schema depends on the command executed.
    #[serde(default)]
    pub data: serde_json::Value,
    /// Human-readable error message when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ZcResponse {
    /// Create a success response with data.
    pub fn success(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data,
            error: None,
        }
    }

    /// Create an error response.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: serde_json::Value::Null,
            error: Some(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn zc_command_serialization_roundtrip() {
        let cmd = ZcCommand::new("gpio_write", json!({"pin": 25, "value": 1}));
        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: ZcCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.cmd, "gpio_write");
        assert_eq!(parsed.params["pin"], 25);
        assert_eq!(parsed.params["value"], 1);
    }

    #[test]
    fn zc_command_simple_has_empty_params() {
        let cmd = ZcCommand::simple("ping");
        assert_eq!(cmd.cmd, "ping");
        assert!(cmd.params.is_object());
    }

    #[test]
    fn zc_response_success_roundtrip() {
        let resp = ZcResponse::success(json!({"value": 1}));
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: ZcResponse = serde_json::from_str(&json).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.data["value"], 1);
        assert!(parsed.error.is_none());
    }

    #[test]
    fn zc_response_error_roundtrip() {
        let resp = ZcResponse::error("pin not available");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: ZcResponse = serde_json::from_str(&json).unwrap();
        assert!(!parsed.ok);
        assert_eq!(parsed.error.as_deref(), Some("pin not available"));
    }

    #[test]
    fn zc_command_wire_format_matches_spec() {
        // Verify the exact JSON shape the firmware expects.
        let cmd = ZcCommand::new("gpio_write", json!({"pin": 25, "value": 1}));
        let v: serde_json::Value = serde_json::to_value(&cmd).unwrap();
        assert!(v.get("cmd").is_some());
        assert!(v.get("params").is_some());
    }

    #[test]
    fn zc_response_from_firmware_json() {
        // Simulate a raw firmware response line.
        let raw = r#"{"ok":true,"data":{"pin":25,"value":1,"state":"HIGH"}}"#;
        let resp: ZcResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.data["state"], "HIGH");
    }

    #[test]
    fn zc_response_missing_optional_fields() {
        // Firmware may omit `data` and `error` on success.
        let raw = r#"{"ok":true}"#;
        let resp: ZcResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.ok);
        assert!(resp.data.is_null());
        assert!(resp.error.is_none());
    }
}
