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
    "/dev/ttyACM",
    "/dev/ttyUSB",
    "/dev/tty.usbmodem",
    "/dev/cu.usbmodem",
    "/dev/tty.usbserial",
    "/dev/cu.usbserial",
    "COM",
];

fn is_path_allowed(path: &str) -> bool {
    ALLOWED_PATH_PREFIXES.iter().any(|p| path.starts_with(p))
}

pub(crate) const FIRMATA_SET_PIN_MODE: u8 = 0xF4;
pub(crate) const FIRMATA_DIGITAL_WRITE: u8 = 0x90;
pub(crate) const FIRMATA_REPORT_VERSION: u8 = 0xF9;
pub(crate) const FIRMATA_ANALOG_READ: u8 = 0xC0;
pub(crate) const FIRMATA_START_SYSEX: u8 = 0xF0;
pub(crate) const FIRMATA_END_SYSEX: u8 = 0xF7;

pub(crate) const FIRMATA_PIN_INPUT: u8 = 0x00;
pub(crate) const FIRMATA_PIN_OUTPUT: u8 = 0x01;
#[allow(dead_code)]
pub(crate) const FIRMATA_PIN_SERVO: u8 = 0x04;

pub(crate) fn encode_set_pin_mode(pin: u8, mode: u8) -> Vec<u8> {
    vec![FIRMATA_SET_PIN_MODE, pin, mode]
}

pub(crate) fn encode_digital_write(pin: u8, value: u8) -> Vec<u8> {
    let port = pin / 8;
    let bit = pin % 8;
    let port_value = if value != 0 { 1 << bit } else { 0 };
    vec![
        FIRMATA_DIGITAL_WRITE | port,
        port_value & 0x7F,
        (port_value >> 7) & 0x7F,
    ]
}

pub(crate) fn encode_report_version() -> Vec<u8> {
    vec![FIRMATA_REPORT_VERSION]
}

pub(crate) fn encode_analog_report(pin: u8, enable: bool) -> Vec<u8> {
    vec![FIRMATA_ANALOG_READ | (pin & 0x0F), u8::from(enable)]
}

pub(crate) fn encode_servo_config(pin: u8, min_pulse: u16, max_pulse: u16) -> Vec<u8> {
    vec![
        FIRMATA_START_SYSEX,
        0x70,
        pin,
        (min_pulse & 0x7F) as u8,
        ((min_pulse >> 7) & 0x7F) as u8,
        (max_pulse & 0x7F) as u8,
        ((max_pulse >> 7) & 0x7F) as u8,
        FIRMATA_END_SYSEX,
    ]
}

const SERIAL_TIMEOUT_SECS: u64 = 5;

pub(crate) struct ArduinoTransport {
    port: Mutex<Option<SerialStream>>,
}

impl ArduinoTransport {
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
            .ok_or_else(|| anyhow::anyhow!("Arduino serial port not connected"))?;

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
                "Arduino serial request timed out after {}s",
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

pub struct ArduinoPeripheral {
    name: String,
    board_type: String,
    transport: Arc<ArduinoTransport>,
}

impl ArduinoPeripheral {
    #[allow(clippy::unused_async)]
    pub async fn connect(config: &PeripheralBoardConfig) -> anyhow::Result<Self> {
        let path = config
            .path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Arduino peripheral requires serial path"))?;

        if !is_path_allowed(path) {
            anyhow::bail!("Serial path not allowed for Arduino: {}", path);
        }

        let baud = if config.baud == 115_200 {
            57_600
        } else {
            config.baud
        };

        let port = tokio_serial::new(path, baud)
            .open_native_async()
            .map_err(|e| anyhow::anyhow!("Failed to open Arduino serial {}: {}", path, e))?;

        let name = format!("arduino-nano-{}", path.replace('/', "_"));
        let transport = Arc::new(ArduinoTransport::new(port));

        Ok(Self {
            name,
            board_type: config.board.clone(),
            transport,
        })
    }
}

#[async_trait]
impl Peripheral for ArduinoPeripheral {
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
            Box::new(ArduinoDigitalReadTool(self.transport.clone())),
            Box::new(ArduinoDigitalWriteTool(self.transport.clone())),
            Box::new(ArduinoAnalogReadTool(self.transport.clone())),
            Box::new(ArduinoServoWriteTool(self.transport.clone())),
        ]
    }
}

struct ArduinoDigitalReadTool(Arc<ArduinoTransport>);

#[async_trait]
impl Tool for ArduinoDigitalReadTool {
    fn name(&self) -> &str {
        "arduino_digital_read"
    }

    fn description(&self) -> &str {
        "Read digital pin value (0 or 1) on Arduino Nano. Pins D0-D13."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "Digital pin number (0-13)" }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        if pin > 13 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Arduino Nano digital pin {} out of range (0-13)",
                    pin
                )),
            });
        }
        self.0.request("digital_read", json!({ "pin": pin })).await
    }
}

struct ArduinoDigitalWriteTool(Arc<ArduinoTransport>);

#[async_trait]
impl Tool for ArduinoDigitalWriteTool {
    fn name(&self) -> &str {
        "arduino_digital_write"
    }

    fn description(&self) -> &str {
        "Set digital pin HIGH (1) or LOW (0) on Arduino Nano. Pins D0-D13."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "Digital pin number (0-13)" },
                "value": { "type": "integer", "description": "0 for LOW, 1 for HIGH" }
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
        if pin > 13 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Arduino Nano digital pin {} out of range (0-13)",
                    pin
                )),
            });
        }
        self.0
            .request("digital_write", json!({ "pin": pin, "value": value }))
            .await
    }
}

struct ArduinoAnalogReadTool(Arc<ArduinoTransport>);

#[async_trait]
impl Tool for ArduinoAnalogReadTool {
    fn name(&self) -> &str {
        "arduino_analog_read"
    }

    fn description(&self) -> &str {
        "Read analog value (0-1023) from Arduino Nano analog pin A0-A7."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "Analog pin number (0-7 for A0-A7)" }
            },
            "required": ["pin"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        if pin > 7 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Arduino Nano analog pin {} out of range (0-7)",
                    pin
                )),
            });
        }
        self.0.request("analog_read", json!({ "pin": pin })).await
    }
}

struct ArduinoServoWriteTool(Arc<ArduinoTransport>);

#[async_trait]
impl Tool for ArduinoServoWriteTool {
    fn name(&self) -> &str {
        "arduino_servo_write"
    }

    fn description(&self) -> &str {
        "Set servo angle (0-180) on Arduino Nano PWM pin. Pins: D3, D5, D6, D9, D10, D11."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pin": { "type": "integer", "description": "PWM pin (3, 5, 6, 9, 10, 11)" },
                "angle": { "type": "integer", "description": "Servo angle (0-180)" }
            },
            "required": ["pin", "angle"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let pin = args
            .get("pin")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'pin' parameter"))?;
        let angle = args
            .get("angle")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'angle' parameter"))?;
        if angle > 180 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Servo angle {} out of range (0-180)", angle)),
            });
        }
        let pwm_pins: &[u64] = &[3, 5, 6, 9, 10, 11];
        if !pwm_pins.contains(&pin) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Pin {} is not PWM-capable. Use 3, 5, 6, 9, 10, or 11.",
                    pin
                )),
            });
        }
        self.0
            .request("servo_write", json!({ "pin": pin, "angle": angle }))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_transport() -> Arc<ArduinoTransport> {
        Arc::new(ArduinoTransport::new_disconnected())
    }

    #[test]
    fn arduino_tool_schemas_valid() {
        let t = mock_transport();
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(ArduinoDigitalReadTool(t.clone())),
            Box::new(ArduinoDigitalWriteTool(t.clone())),
            Box::new(ArduinoAnalogReadTool(t.clone())),
            Box::new(ArduinoServoWriteTool(t)),
        ];
        let expected = [
            "arduino_digital_read",
            "arduino_digital_write",
            "arduino_analog_read",
            "arduino_servo_write",
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
    fn arduino_path_validation() {
        assert!(is_path_allowed("/dev/ttyACM0"));
        assert!(is_path_allowed("/dev/cu.usbmodem14101"));
        assert!(is_path_allowed("COM3"));
        assert!(!is_path_allowed("/dev/sda1"));
        assert!(!is_path_allowed("/etc/passwd"));
    }

    #[test]
    fn firmata_set_pin_mode_encoding() {
        let bytes = encode_set_pin_mode(13, FIRMATA_PIN_OUTPUT);
        assert_eq!(bytes, vec![0xF4, 13, 0x01]);
        let bytes = encode_set_pin_mode(2, FIRMATA_PIN_INPUT);
        assert_eq!(bytes, vec![0xF4, 2, 0x00]);
    }

    #[test]
    fn firmata_digital_write_encoding() {
        let bytes = encode_digital_write(13, 1);
        assert_eq!(bytes[0], 0x90 | 1);
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[1] & 0x80, 0);
        assert_eq!(bytes[2] & 0x80, 0);
        let bytes_low = encode_digital_write(13, 0);
        assert_eq!(bytes_low[1], 0);
    }

    #[test]
    fn firmata_report_version_encoding() {
        assert_eq!(encode_report_version(), vec![0xF9]);
    }

    #[test]
    fn firmata_analog_report_encoding() {
        assert_eq!(encode_analog_report(0, true), vec![0xC0, 1]);
        assert_eq!(encode_analog_report(3, false), vec![0xC3, 0]);
    }

    #[test]
    fn firmata_servo_config_encoding() {
        let bytes = encode_servo_config(9, 544, 2400);
        assert_eq!(bytes[0], FIRMATA_START_SYSEX);
        assert_eq!(bytes[1], 0x70);
        assert_eq!(bytes[2], 9);
        assert_eq!(*bytes.last().unwrap(), FIRMATA_END_SYSEX);
        assert_eq!(bytes.len(), 8);
    }

    #[tokio::test]
    async fn arduino_digital_read_rejects_invalid_pin() {
        let tool = ArduinoDigitalReadTool(mock_transport());
        let result = tool.execute(json!({ "pin": 14 })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn arduino_analog_read_rejects_invalid_pin() {
        let tool = ArduinoAnalogReadTool(mock_transport());
        let result = tool.execute(json!({ "pin": 8 })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn arduino_servo_rejects_invalid_angle() {
        let tool = ArduinoServoWriteTool(mock_transport());
        let result = tool
            .execute(json!({ "pin": 9, "angle": 200 }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn arduino_servo_rejects_non_pwm_pin() {
        let tool = ArduinoServoWriteTool(mock_transport());
        let result = tool
            .execute(json!({ "pin": 2, "angle": 90 }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not PWM-capable"));
    }

    #[tokio::test]
    async fn arduino_disconnected_returns_error() {
        let tool = ArduinoDigitalReadTool(mock_transport());
        let result = tool.execute(json!({ "pin": 2 })).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not connected"));
    }

    #[test]
    fn arduino_config_parsing() {
        let config = PeripheralBoardConfig {
            board: "arduino-nano".into(),
            transport: "serial".into(),
            path: Some("/dev/ttyACM0".into()),
            baud: 57_600,
        };
        assert_eq!(config.board, "arduino-nano");
        assert_eq!(config.baud, 57_600);
        assert!(is_path_allowed(config.path.as_deref().unwrap()));
    }
}
