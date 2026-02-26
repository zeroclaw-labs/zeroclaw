use super::traits::Peripheral;
use crate::config::PeripheralBoardConfig;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use tokio_serial::{SerialPortBuilderExt, SerialStream};

const ALLOWED_PATH_PREFIXES: &[&str] = &[
    "/dev/ttyUSB",
    "/dev/ttyACM",
    "/dev/tty.usbserial",
    "/dev/cu.usbserial",
    "/dev/tty.usbmodem",
    "/dev/cu.usbmodem",
    "/dev/tty.SLAB",
    "/dev/cu.SLAB",
    "COM",
];

fn is_path_allowed(path: &str) -> bool {
    ALLOWED_PATH_PREFIXES.iter().any(|p| path.starts_with(p))
}

const SERIAL_TIMEOUT_SECS: u64 = 5;

pub(crate) struct Esp32Transport {
    port: Mutex<Option<SerialStream>>,
}

impl Esp32Transport {
    fn new(port: SerialStream) -> Self {
        Self {
            port: Mutex::new(Some(port)),
        }
    }

    #[cfg(test)]
    fn new_disconnected() -> Self {
        Self {
            port: Mutex::new(None),
        }
    }

    async fn request(&self, cmd: &str, args: Value) -> anyhow::Result<ToolResult> {
        let mut guard = self.port.lock().await;
        let port = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ESP32 serial port not connected"))?;

        static ID: AtomicU64 = AtomicU64::new(0);
        let id = ID.fetch_add(1, Ordering::Relaxed);
        let id_str = id.to_string();
        let req = json!({ "id": id_str, "cmd": cmd, "args": args });
        let line = format!("{}\n", req);

        let send_recv = async {
            port.write_all(line.as_bytes()).await?;
            port.flush().await?;
            let mut buf = Vec::new();
            let mut b = [0u8; 1];
            while port.read_exact(&mut b).await.is_ok() {
                if b[0] == b'\n' {
                    break;
                }
                buf.push(b[0]);
            }
            let line_str = String::from_utf8_lossy(&buf);
            let resp: Value = serde_json::from_str(line_str.trim())?;
            Ok::<Value, anyhow::Error>(resp)
        };

        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(SERIAL_TIMEOUT_SECS),
            send_recv,
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "ESP32 serial request timed out after {}s",
                SERIAL_TIMEOUT_SECS
            )
        })??;

        let ok = resp["ok"].as_bool().unwrap_or(false);
        let result = resp["result"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| resp["result"].to_string());
        let error = resp["error"].as_str().map(String::from);

        Ok(ToolResult {
            success: ok,
            output: result,
            error,
        })
    }
}

pub struct Esp32Peripheral {
    name: String,
    board_type: String,
    transport: Arc<Esp32Transport>,
}

impl Esp32Peripheral {
    #[allow(clippy::unused_async)]
    pub async fn connect(config: &PeripheralBoardConfig) -> anyhow::Result<Self> {
        let path = config
            .path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("ESP32 peripheral requires serial path"))?;

        if !is_path_allowed(path) {
            anyhow::bail!("Serial path not allowed for ESP32: {}", path);
        }

        let port = tokio_serial::new(path, config.baud)
            .open_native_async()
            .map_err(|e| anyhow::anyhow!("Failed to open ESP32 serial {}: {}", path, e))?;

        let name = format!("esp32-{}", path.replace('/', "_"));
        let transport = Arc::new(Esp32Transport::new(port));

        Ok(Self {
            name,
            board_type: config.board.clone(),
            transport,
        })
    }
}

#[async_trait]
impl Peripheral for Esp32Peripheral {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_type(&self) -> &str {
        &self.board_type
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.transport
            .request("ping", json!({}))
            .await
            .map(|r| r.success)
            .unwrap_or(false)
    }

    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(Esp32GpioReadTool(self.transport.clone())),
            Box::new(Esp32GpioWriteTool(self.transport.clone())),
            Box::new(Esp32AdcReadTool(self.transport.clone())),
            Box::new(Esp32PwmSetTool(self.transport.clone())),
            Box::new(Esp32WifiStatusTool(self.transport.clone())),
        ]
    }
}

struct Esp32GpioReadTool(Arc<Esp32Transport>);

#[async_trait]
impl Tool for Esp32GpioReadTool {
    fn name(&self) -> &str {
        "esp32_gpio_read"
    }

    fn description(&self) -> &str {
        "Read GPIO pin value (0 or 1) on ESP32. Supports GPIO 0-39."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "GPIO pin number (0-39)" }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        if pin > 39 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("ESP32 GPIO pin {} out of range (0-39)", pin)),
            });
        }
        self.0.request("gpio_read", json!({ "pin": pin })).await
    }
}

struct Esp32GpioWriteTool(Arc<Esp32Transport>);

#[async_trait]
impl Tool for Esp32GpioWriteTool {
    fn name(&self) -> &str {
        "esp32_gpio_write"
    }

    fn description(&self) -> &str {
        "Set GPIO pin high (1) or low (0) on ESP32. Output-capable pins: 0-33."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "GPIO pin number (0-33 for output)" },
                "value": { "type": "integer", "description": "0 for low, 1 for high" }
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
        if pin > 33 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("ESP32 GPIO pin {} not output-capable (0-33)", pin)),
            });
        }
        self.0
            .request("gpio_write", json!({ "pin": pin, "value": value }))
            .await
    }
}

struct Esp32AdcReadTool(Arc<Esp32Transport>);

#[async_trait]
impl Tool for Esp32AdcReadTool {
    fn name(&self) -> &str {
        "esp32_adc_read"
    }

    fn description(&self) -> &str {
        "Read ADC value (0-4095, 12-bit) from an ESP32 analog pin."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "ADC-capable GPIO pin number" },
                "attenuation": {
                    "type": "string",
                    "description": "ADC attenuation (default: 11db)",
                    "enum": ["0db", "2.5db", "6db", "11db"]
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
        let attenuation = args
            .get("attenuation")
            .and_then(|v| v.as_str())
            .unwrap_or("11db");
        self.0
            .request(
                "adc_read",
                json!({ "pin": pin, "attenuation": attenuation }),
            )
            .await
    }
}

struct Esp32PwmSetTool(Arc<Esp32Transport>);

#[async_trait]
impl Tool for Esp32PwmSetTool {
    fn name(&self) -> &str {
        "esp32_pwm_set"
    }

    fn description(&self) -> &str {
        "Set PWM output on an ESP32 GPIO pin. Configure frequency and duty cycle (0-1023)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "GPIO pin number for PWM" },
                "frequency": { "type": "integer", "description": "PWM frequency in Hz" },
                "duty": { "type": "integer", "description": "Duty cycle (0-1023)" }
            },
            "required": ["pin", "frequency", "duty"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let frequency = args
            .get("frequency")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'frequency' parameter"))?;
        let duty = args
            .get("duty")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'duty' parameter"))?;
        if duty > 1023 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Duty cycle {} out of range (0-1023)", duty)),
            });
        }
        self.0
            .request(
                "pwm_set",
                json!({ "pin": pin, "frequency": frequency, "duty": duty }),
            )
            .await
    }
}

struct Esp32WifiStatusTool(Arc<Esp32Transport>);

#[async_trait]
impl Tool for Esp32WifiStatusTool {
    fn name(&self) -> &str {
        "esp32_wifi_status"
    }

    fn description(&self) -> &str {
        "Query ESP32 WiFi status: SSID, IP address, RSSI signal strength."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let _ = args;
        self.0.request("wifi_status", json!({})).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_transport() -> Arc<Esp32Transport> {
        Arc::new(Esp32Transport::new_disconnected())
    }

    #[test]
    fn esp32_tool_schemas_valid() {
        let t = mock_transport();
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(Esp32GpioReadTool(t.clone())),
            Box::new(Esp32GpioWriteTool(t.clone())),
            Box::new(Esp32AdcReadTool(t.clone())),
            Box::new(Esp32PwmSetTool(t.clone())),
            Box::new(Esp32WifiStatusTool(t)),
        ];
        let expected = [
            "esp32_gpio_read",
            "esp32_gpio_write",
            "esp32_adc_read",
            "esp32_pwm_set",
            "esp32_wifi_status",
        ];
        assert_eq!(tools.len(), expected.len());
        for (tool, name) in tools.iter().zip(expected.iter()) {
            assert_eq!(tool.name(), *name);
            let s = tool.parameters_schema();
            assert_eq!(s["type"], "object");
            assert!(s.get("properties").is_some());
        }
    }

    #[test]
    fn esp32_path_validation() {
        assert!(is_path_allowed("/dev/ttyUSB0"));
        assert!(is_path_allowed("/dev/cu.usbserial-1410"));
        assert!(is_path_allowed("/dev/cu.SLAB_USBtoUART"));
        assert!(is_path_allowed("COM3"));
        assert!(!is_path_allowed("/dev/sda1"));
        assert!(!is_path_allowed("/etc/passwd"));
        assert!(!is_path_allowed("/tmp/exploit"));
    }

    #[tokio::test]
    async fn esp32_gpio_read_rejects_invalid_pin() {
        let tool = Esp32GpioReadTool(mock_transport());
        let result = tool.execute(json!({ "pin": 40 })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn esp32_gpio_write_rejects_output_only_pin() {
        let tool = Esp32GpioWriteTool(mock_transport());
        let result = tool
            .execute(json!({ "pin": 34, "value": 1 }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not output-capable"));
    }

    #[tokio::test]
    async fn esp32_pwm_rejects_invalid_duty() {
        let tool = Esp32PwmSetTool(mock_transport());
        let result = tool
            .execute(json!({ "pin": 2, "frequency": 5000, "duty": 2000 }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn esp32_gpio_read_missing_pin() {
        let tool = Esp32GpioReadTool(mock_transport());
        assert!(tool.execute(json!({})).await.is_err());
    }

    #[tokio::test]
    async fn esp32_disconnected_returns_error() {
        let tool = Esp32GpioReadTool(mock_transport());
        let result = tool.execute(json!({ "pin": 2 })).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not connected"));
    }

    #[test]
    fn esp32_config_parsing() {
        let config = PeripheralBoardConfig {
            board: "esp32".into(),
            transport: "serial".into(),
            path: Some("/dev/ttyUSB0".into()),
            baud: 115_200,
        };
        assert_eq!(config.board, "esp32");
        assert_eq!(config.baud, 115_200);
        assert!(is_path_allowed(config.path.as_deref().unwrap()));
    }
}
