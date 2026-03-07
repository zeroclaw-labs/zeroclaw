//! GPIO tools — `gpio_read` and `gpio_write` for LLM-driven hardware control.
//!
//! These are the first built-in hardware tools. They implement the standard
//! [`Tool`](crate::tools::Tool) trait so the LLM can call them via function
//! calling, and dispatch commands to physical devices via the
//! [`Transport`](super::Transport) layer.
//!
//! Wire protocol (ZeroClaw serial JSON):
//! ```text
//! gpio_write:
//!   Host → Device:  {"cmd":"gpio_write","params":{"pin":25,"value":1}}\n
//!   Device → Host:  {"ok":true,"data":{"pin":25,"value":1,"state":"HIGH"}}\n
//!
//! gpio_read:
//!   Host → Device:  {"cmd":"gpio_read","params":{"pin":25}}\n
//!   Device → Host:  {"ok":true,"data":{"pin":25,"value":1,"state":"HIGH"}}\n
//! ```

use super::device::DeviceRegistry;
use super::protocol::ZcCommand;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── GpioWriteTool ─────────────────────────────────────────────────────────────

/// Tool: set a GPIO pin HIGH or LOW on a connected hardware device.
///
/// The LLM provides `device` (alias), `pin`, and `value` (0 or 1).
/// The tool builds a `ZcCommand`, sends it via the device's transport,
/// and returns a human-readable result.
pub struct GpioWriteTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl GpioWriteTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for GpioWriteTool {
    fn name(&self) -> &str {
        "gpio_write"
    }

    fn description(&self) -> &str {
        "Set a GPIO pin HIGH (1) or LOW (0) on a connected hardware device"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Device alias e.g. pico0, arduino0"
                },
                "pin": {
                    "type": "integer",
                    "description": "GPIO pin number"
                },
                "value": {
                    "type": "integer",
                    "enum": [0, 1],
                    "description": "1 = HIGH (on), 0 = LOW (off)"
                }
            },
            "required": ["pin", "value"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let pin = match args.get("pin").and_then(|v| v.as_u64()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: pin".to_string()),
                })
            }
        };
        let value = match args.get("value").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: value".to_string()),
                })
            }
        };

        if value > 1 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("value must be 0 or 1".to_string()),
            });
        }

        // Resolve device alias and obtain an owned context (Arc-based) before
        // dropping the registry read guard — avoids holding the lock across async I/O.
        let (device_alias, ctx) = {
            let registry = self.registry.read().await;
            match registry.resolve_gpio_device(&args) {
                Ok(resolved) => resolved,
                Err(msg) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(msg),
                    });
                }
            }
            // registry read guard dropped here
        };

        let cmd = ZcCommand::new("gpio_write", json!({ "pin": pin, "value": value }));

        match ctx.transport.send(&cmd).await {
            Ok(resp) if resp.ok => {
                let state = resp
                    .data
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or(if value == 1 { "HIGH" } else { "LOW" });
                Ok(ToolResult {
                    success: true,
                    output: format!("GPIO {} set {} on {}", pin, state, device_alias),
                    error: None,
                })
            }
            Ok(resp) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    resp.error
                        .unwrap_or_else(|| "device returned ok:false".to_string()),
                ),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("transport error: {}", e)),
            }),
        }
    }
}

// ── GpioReadTool ──────────────────────────────────────────────────────────────

/// Tool: read the current HIGH/LOW state of a GPIO pin on a connected device.
///
/// The LLM provides `device` (alias) and `pin`. The tool builds a `ZcCommand`,
/// sends it via the device's transport, and returns the pin state.
pub struct GpioReadTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl GpioReadTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for GpioReadTool {
    fn name(&self) -> &str {
        "gpio_read"
    }

    fn description(&self) -> &str {
        "Read the current HIGH/LOW state of a GPIO pin on a connected device"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Device alias e.g. pico0, arduino0"
                },
                "pin": {
                    "type": "integer",
                    "description": "GPIO pin number to read"
                }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let pin = match args.get("pin").and_then(|v| v.as_u64()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: pin".to_string()),
                })
            }
        };

        // Resolve device alias and obtain an owned context (Arc-based) before
        // dropping the registry read guard — avoids holding the lock across async I/O.
        let (device_alias, ctx) = {
            let registry = self.registry.read().await;
            match registry.resolve_gpio_device(&args) {
                Ok(resolved) => resolved,
                Err(msg) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(msg),
                    });
                }
            }
            // registry read guard dropped here
        };

        let cmd = ZcCommand::new("gpio_read", json!({ "pin": pin }));

        match ctx.transport.send(&cmd).await {
            Ok(resp) if resp.ok => {
                let value = resp.data.get("value").and_then(|v| v.as_u64()).unwrap_or(0);
                let state = resp
                    .data
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or(if value == 1 { "HIGH" } else { "LOW" });
                Ok(ToolResult {
                    success: true,
                    output: format!("GPIO {} is {} ({}) on {}", pin, state, value, device_alias),
                    error: None,
                })
            }
            Ok(resp) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    resp.error
                        .unwrap_or_else(|| "device returned ok:false".to_string()),
                ),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("transport error: {}", e)),
            }),
        }
    }
}

// ── Factory ───────────────────────────────────────────────────────────────────

/// Create the built-in GPIO tools for a given device registry.
///
/// Returns `[GpioWriteTool, GpioReadTool]` ready for registration in the
/// agent's tool list or a future `ToolRegistry`.
pub fn gpio_tools(registry: Arc<RwLock<DeviceRegistry>>) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(GpioWriteTool::new(registry.clone())),
        Box::new(GpioReadTool::new(registry)),
    ]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{
        device::{DeviceCapabilities, DeviceRegistry},
        protocol::ZcResponse,
        transport::{Transport, TransportError, TransportKind},
    };
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Mock transport that returns configurable responses.
    struct MockTransport {
        response: tokio::sync::Mutex<ZcResponse>,
        connected: AtomicBool,
        last_cmd: tokio::sync::Mutex<Option<ZcCommand>>,
    }

    impl MockTransport {
        fn new(response: ZcResponse) -> Self {
            Self {
                response: tokio::sync::Mutex::new(response),
                connected: AtomicBool::new(true),
                last_cmd: tokio::sync::Mutex::new(None),
            }
        }

        fn disconnected() -> Self {
            let t = Self::new(ZcResponse::error("mock: disconnected"));
            t.connected.store(false, Ordering::SeqCst);
            t
        }

        async fn last_command(&self) -> Option<ZcCommand> {
            self.last_cmd.lock().await.clone()
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn send(&self, cmd: &ZcCommand) -> Result<ZcResponse, TransportError> {
            if !self.connected.load(Ordering::SeqCst) {
                return Err(TransportError::Disconnected);
            }
            *self.last_cmd.lock().await = Some(cmd.clone());
            Ok(self.response.lock().await.clone())
        }

        fn kind(&self) -> TransportKind {
            TransportKind::Serial
        }

        fn is_connected(&self) -> bool {
            self.connected.load(Ordering::SeqCst)
        }
    }

    /// Helper: build a registry with one device + mock transport.
    fn registry_with_mock(transport: Arc<MockTransport>) -> Arc<RwLock<DeviceRegistry>> {
        let mut reg = DeviceRegistry::new();
        let alias = reg.register(
            "raspberry-pi-pico",
            Some(0x2e8a),
            Some(0x000a),
            Some("/dev/ttyACM0".to_string()),
            Some("ARM Cortex-M0+".to_string()),
        );
        reg.attach_transport(
            &alias,
            transport as Arc<dyn Transport>,
            DeviceCapabilities {
                gpio: true,
                ..Default::default()
            },
        )
        .expect("alias was just registered");
        Arc::new(RwLock::new(reg))
    }

    // ── GpioWriteTool tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn gpio_write_success() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(
            json!({"pin": 25, "value": 1, "state": "HIGH"}),
        )));
        let reg = registry_with_mock(mock.clone());
        let tool = GpioWriteTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 25, "value": 1}))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "GPIO 25 set HIGH on pico0");
        assert!(result.error.is_none());

        // Verify the command sent to the device
        let cmd = mock.last_command().await.unwrap();
        assert_eq!(cmd.cmd, "gpio_write");
        assert_eq!(cmd.params["pin"], 25);
        assert_eq!(cmd.params["value"], 1);
    }

    #[tokio::test]
    async fn gpio_write_low() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(
            json!({"pin": 13, "value": 0, "state": "LOW"}),
        )));
        let reg = registry_with_mock(mock.clone());
        let tool = GpioWriteTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 13, "value": 0}))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "GPIO 13 set LOW on pico0");
    }

    #[tokio::test]
    async fn gpio_write_device_error() {
        let mock = Arc::new(MockTransport::new(ZcResponse::error(
            "pin 99 not available",
        )));
        let reg = registry_with_mock(mock);
        let tool = GpioWriteTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 99, "value": 1}))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("pin 99 not available"));
    }

    #[tokio::test]
    async fn gpio_write_transport_disconnected() {
        let mock = Arc::new(MockTransport::disconnected());
        let reg = registry_with_mock(mock);
        let tool = GpioWriteTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 25, "value": 1}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("transport"));
    }

    #[tokio::test]
    async fn gpio_write_unknown_device() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(json!({}))));
        let reg = registry_with_mock(mock);
        let tool = GpioWriteTool::new(reg);

        let result = tool
            .execute(json!({"device": "nonexistent", "pin": 25, "value": 1}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn gpio_write_invalid_value() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(json!({}))));
        let reg = registry_with_mock(mock);
        let tool = GpioWriteTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 25, "value": 5}))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("value must be 0 or 1"));
    }

    #[tokio::test]
    async fn gpio_write_missing_params() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(json!({}))));
        let reg = registry_with_mock(mock);
        let tool = GpioWriteTool::new(reg);

        // Missing pin
        let result = tool
            .execute(json!({"device": "pico0", "value": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("missing required parameter: pin"));

        // Missing device with empty registry — auto-select finds no GPIO device → Ok(failure)
        let empty_reg = Arc::new(RwLock::new(DeviceRegistry::new()));
        let tool_no_reg = GpioWriteTool::new(empty_reg);
        let result = tool_no_reg
            .execute(json!({"pin": 25, "value": 1}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("no GPIO"));

        // Missing value
        let result = tool
            .execute(json!({"device": "pico0", "pin": 25}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("missing required parameter: value"));
    }

    // ── GpioReadTool tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn gpio_read_success() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(
            json!({"pin": 25, "value": 1, "state": "HIGH"}),
        )));
        let reg = registry_with_mock(mock.clone());
        let tool = GpioReadTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 25}))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "GPIO 25 is HIGH (1) on pico0");
        assert!(result.error.is_none());

        let cmd = mock.last_command().await.unwrap();
        assert_eq!(cmd.cmd, "gpio_read");
        assert_eq!(cmd.params["pin"], 25);
    }

    #[tokio::test]
    async fn gpio_read_low() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(
            json!({"pin": 13, "value": 0, "state": "LOW"}),
        )));
        let reg = registry_with_mock(mock);
        let tool = GpioReadTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 13}))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "GPIO 13 is LOW (0) on pico0");
    }

    #[tokio::test]
    async fn gpio_read_device_error() {
        let mock = Arc::new(MockTransport::new(ZcResponse::error("pin not configured")));
        let reg = registry_with_mock(mock);
        let tool = GpioReadTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 99}))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("pin not configured"));
    }

    #[tokio::test]
    async fn gpio_read_transport_disconnected() {
        let mock = Arc::new(MockTransport::disconnected());
        let reg = registry_with_mock(mock);
        let tool = GpioReadTool::new(reg);

        let result = tool
            .execute(json!({"device": "pico0", "pin": 25}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("transport"));
    }

    #[tokio::test]
    async fn gpio_read_missing_params() {
        let mock = Arc::new(MockTransport::new(ZcResponse::success(json!({}))));
        let reg = registry_with_mock(mock);
        let tool = GpioReadTool::new(reg);

        // Missing pin
        let result = tool.execute(json!({"device": "pico0"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("missing required parameter: pin"));

        // Missing device with empty registry — auto-select finds no GPIO device → Ok(failure)
        let empty_reg = Arc::new(RwLock::new(DeviceRegistry::new()));
        let tool_no_reg = GpioReadTool::new(empty_reg);
        let result = tool_no_reg.execute(json!({"pin": 25})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("no GPIO"));
    }

    // ── Factory / spec tests ─────────────────────────────────────────────

    #[test]
    fn gpio_tools_factory_returns_two() {
        let reg = Arc::new(RwLock::new(DeviceRegistry::new()));
        let tools = gpio_tools(reg);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name(), "gpio_write");
        assert_eq!(tools[1].name(), "gpio_read");
    }

    #[test]
    fn gpio_write_spec_is_valid() {
        let reg = Arc::new(RwLock::new(DeviceRegistry::new()));
        let tool = GpioWriteTool::new(reg);
        let spec = tool.spec();
        assert_eq!(spec.name, "gpio_write");
        assert!(spec.parameters["properties"]["device"].is_object());
        assert!(spec.parameters["properties"]["pin"].is_object());
        assert!(spec.parameters["properties"]["value"].is_object());
        let required = spec.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 2, "required should be [pin, value]");
    }

    #[test]
    fn gpio_read_spec_is_valid() {
        let reg = Arc::new(RwLock::new(DeviceRegistry::new()));
        let tool = GpioReadTool::new(reg);
        let spec = tool.spec();
        assert_eq!(spec.name, "gpio_read");
        assert!(spec.parameters["properties"]["device"].is_object());
        assert!(spec.parameters["properties"]["pin"].is_object());
        let required = spec.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 1, "required should be [pin]");
    }
}
