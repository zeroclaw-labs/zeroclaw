//! Arduino Uno Q Bridge — GPIO via socket to Bridge app.
//!
//! When ZeroClaw runs on Uno Q, the Bridge app (Python + MCU) exposes
//! digitalWrite/digitalRead over a local socket. These tools connect to it.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroclaw_api::tool::{Tool, ToolResult};

const BRIDGE_HOST: &str = "127.0.0.1";
const BRIDGE_PORT: u16 = 9999;

async fn bridge_request(cmd: &str, args: &[String]) -> anyhow::Result<String> {
    let addr = format!("{}:{}", BRIDGE_HOST, BRIDGE_PORT);
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("Bridge connection timed out"))??;

    let msg = format!("{} {}\n", cmd, args.join(" "));
    stream.write_all(msg.as_bytes()).await?;

    let mut buf = vec![0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("Bridge response timed out"))??;
    let resp = String::from_utf8_lossy(&buf[..n]).trim().to_string();
    Ok(resp)
}

/// Tool: read GPIO pin via Uno Q Bridge.
pub struct UnoQGpioReadTool;

#[async_trait]
impl Tool for UnoQGpioReadTool {
    fn name(&self) -> &str {
        "gpio_read"
    }

    fn description(&self) -> &str {
        "Read GPIO pin value (0 or 1) on Arduino Uno Q. Requires uno-q-bridge app running."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "GPIO pin number (e.g. 13 for LED)"
                }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        match bridge_request("gpio_read", &[pin.to_string()]).await {
            Ok(resp) => {
                if resp.starts_with("error:") {
                    Ok(ToolResult {
                        success: false,
                        output: resp.clone(),
                        error: Some(resp),
                    })
                } else {
                    Ok(ToolResult {
                        success: true,
                        output: resp,
                        error: None,
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: format!("Bridge error: {}", e),
                error: Some(e.to_string()),
            }),
        }
    }
}

/// Tool: write GPIO pin via Uno Q Bridge.
pub struct UnoQGpioWriteTool;

#[async_trait]
impl Tool for UnoQGpioWriteTool {
    fn name(&self) -> &str {
        "gpio_write"
    }

    fn description(&self) -> &str {
        "Set GPIO pin high (1) or low (0) on Arduino Uno Q. Requires uno-q-bridge app running."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": {
                    "type": "integer",
                    "description": "GPIO pin number"
                },
                "value": {
                    "type": "integer",
                    "description": "0 for low, 1 for high"
                }
            },
            "required": ["pin", "value"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let value = args
            .get("value")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'value' parameter"))?;
        match bridge_request("gpio_write", &[pin.to_string(), value.to_string()]).await {
            Ok(resp) => {
                if resp.starts_with("error:") {
                    Ok(ToolResult {
                        success: false,
                        output: resp.clone(),
                        error: Some(resp),
                    })
                } else {
                    Ok(ToolResult {
                        success: true,
                        output: "done".into(),
                        error: None,
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: format!("Bridge error: {}", e),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::tool::Tool;

    // ── UnoQGpioReadTool ────────────────────────────────────────────────

    #[test]
    fn gpio_read_tool_name() {
        let tool = UnoQGpioReadTool;
        assert_eq!(tool.name(), "gpio_read");
    }

    #[test]
    fn gpio_read_tool_description_mentions_uno_q() {
        let tool = UnoQGpioReadTool;
        assert!(
            tool.description().contains("Uno Q"),
            "description should mention Uno Q"
        );
    }

    #[test]
    fn gpio_read_tool_schema_requires_pin() {
        let tool = UnoQGpioReadTool;
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["pin"].is_object());
        let required = schema["required"].as_array().expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("pin")),
            "pin should be required"
        );
    }

    #[test]
    fn gpio_read_tool_spec_valid() {
        let tool = UnoQGpioReadTool;
        let spec = tool.spec();
        assert_eq!(spec.name, "gpio_read");
        assert!(!spec.description.is_empty());
        assert_eq!(spec.parameters["type"], "object");
    }

    #[tokio::test]
    async fn gpio_read_missing_pin_returns_error() {
        let tool = UnoQGpioReadTool;
        // execute returns Err when pin is missing (anyhow bail)
        let result = tool.execute(json!({})).await;
        assert!(result.is_err(), "missing pin should return Err");
    }

    #[tokio::test]
    async fn gpio_read_no_bridge_returns_error() {
        // No bridge server running — connection should fail with a timeout or connection error.
        let tool = UnoQGpioReadTool;
        let result = tool.execute(json!({"pin": 13})).await.unwrap();
        assert!(!result.success);
        assert!(
            result.error.is_some(),
            "should report bridge connection error"
        );
    }

    // ── UnoQGpioWriteTool ───────────────────────────────────────────────

    #[test]
    fn gpio_write_tool_name() {
        let tool = UnoQGpioWriteTool;
        assert_eq!(tool.name(), "gpio_write");
    }

    #[test]
    fn gpio_write_tool_description_mentions_uno_q() {
        let tool = UnoQGpioWriteTool;
        assert!(
            tool.description().contains("Uno Q"),
            "description should mention Uno Q"
        );
    }

    #[test]
    fn gpio_write_tool_schema_requires_pin_and_value() {
        let tool = UnoQGpioWriteTool;
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["pin"].is_object());
        assert!(schema["properties"]["value"].is_object());
        let required = schema["required"].as_array().expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("pin")),
            "pin should be required"
        );
        assert!(
            required.iter().any(|v| v.as_str() == Some("value")),
            "value should be required"
        );
    }

    #[test]
    fn gpio_write_tool_spec_valid() {
        let tool = UnoQGpioWriteTool;
        let spec = tool.spec();
        assert_eq!(spec.name, "gpio_write");
        assert!(!spec.description.is_empty());
        assert_eq!(spec.parameters["type"], "object");
    }

    #[tokio::test]
    async fn gpio_write_missing_pin_returns_error() {
        let tool = UnoQGpioWriteTool;
        // execute returns Err when pin is missing (anyhow bail)
        let result = tool.execute(json!({"value": 1})).await;
        assert!(result.is_err(), "missing pin should return Err");
    }

    #[tokio::test]
    async fn gpio_write_missing_value_returns_error() {
        let tool = UnoQGpioWriteTool;
        // execute returns Err when value is missing (anyhow bail)
        let result = tool.execute(json!({"pin": 13})).await;
        assert!(result.is_err(), "missing value should return Err");
    }

    #[tokio::test]
    async fn gpio_write_no_bridge_returns_error() {
        // No bridge server running — connection should fail.
        let tool = UnoQGpioWriteTool;
        let result = tool.execute(json!({"pin": 13, "value": 1})).await.unwrap();
        assert!(!result.success);
        assert!(
            result.error.is_some(),
            "should report bridge connection error"
        );
    }

    // ── Constants ───────────────────────────────────────────────────────

    #[test]
    fn bridge_host_is_localhost() {
        assert_eq!(BRIDGE_HOST, "127.0.0.1");
    }

    #[test]
    fn bridge_port_is_9999() {
        assert_eq!(BRIDGE_PORT, 9999);
    }
}
