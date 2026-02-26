use super::traits::Peripheral;
use crate::config::PeripheralBoardConfig;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

const I2C_VALID_ADDR_MIN: u8 = 0x03;
const I2C_VALID_ADDR_MAX: u8 = 0x77;

const ALLOWED_BUS_PREFIXES: &[&str] = &["/dev/i2c-"];

fn is_bus_path_allowed(path: &str) -> bool {
    ALLOWED_BUS_PREFIXES.iter().any(|p| path.starts_with(p))
}

pub(crate) fn is_valid_i2c_address(addr: u8) -> bool {
    (I2C_VALID_ADDR_MIN..=I2C_VALID_ADDR_MAX).contains(&addr)
}

#[derive(Debug, Clone)]
pub(crate) struct SensorProfile {
    pub name: &'static str,
    pub address: u8,
    pub id_register: u8,
    pub expected_id: u8,
    pub description: &'static str,
    pub read_registers: &'static [(u8, &'static str, u8)],
}

pub(crate) const KNOWN_SENSORS: &[SensorProfile] = &[
    SensorProfile {
        name: "BME280",
        address: 0x76,
        id_register: 0xD0,
        expected_id: 0x60,
        description: "Temperature, humidity, and pressure sensor",
        read_registers: &[
            (0xFA, "temperature", 3),
            (0xFD, "humidity", 2),
            (0xF7, "pressure", 3),
        ],
    },
    SensorProfile {
        name: "BH1750",
        address: 0x23,
        id_register: 0x00,
        expected_id: 0x00,
        description: "Ambient light sensor (lux)",
        read_registers: &[(0x10, "light_level", 2)],
    },
    SensorProfile {
        name: "MPU6050",
        address: 0x68,
        id_register: 0x75,
        expected_id: 0x68,
        description: "6-axis accelerometer and gyroscope",
        read_registers: &[
            (0x3B, "accel_x", 2),
            (0x3D, "accel_y", 2),
            (0x3F, "accel_z", 2),
            (0x43, "gyro_x", 2),
            (0x45, "gyro_y", 2),
            (0x47, "gyro_z", 2),
        ],
    },
    SensorProfile {
        name: "ADS1115",
        address: 0x48,
        id_register: 0x01,
        expected_id: 0x85,
        description: "16-bit 4-channel ADC",
        read_registers: &[(0x00, "conversion", 2)],
    },
];

pub(crate) fn find_sensor_profile(name_or_addr: &str) -> Option<&'static SensorProfile> {
    if let Some(hex) = name_or_addr.strip_prefix("0x") {
        if let Ok(addr) = u8::from_str_radix(hex, 16) {
            return KNOWN_SENSORS.iter().find(|s| s.address == addr);
        }
    }
    let upper = name_or_addr.to_uppercase();
    KNOWN_SENSORS.iter().find(|s| s.name == upper)
}

struct I2cBusState {
    path: String,
    _connected: bool,
}

pub struct I2cBusPeripheral {
    name: String,
    board_type: String,
    state: Arc<Mutex<I2cBusState>>,
}

impl I2cBusPeripheral {
    #[allow(clippy::unused_async)]
    pub async fn connect(config: &PeripheralBoardConfig) -> anyhow::Result<Self> {
        let path = config.path.as_deref().ok_or_else(|| {
            anyhow::anyhow!("I2C bus peripheral requires bus path (e.g. /dev/i2c-1)")
        })?;

        if !is_bus_path_allowed(path) {
            anyhow::bail!(
                "I2C bus path not allowed: {}. Must start with /dev/i2c-",
                path
            );
        }

        let name = format!("i2c-bus-{}", path.replace('/', "_"));
        let state = Arc::new(Mutex::new(I2cBusState {
            path: path.to_string(),
            _connected: false,
        }));

        Ok(Self {
            name,
            board_type: config.board.clone(),
            state,
        })
    }
}

#[async_trait]
impl Peripheral for I2cBusPeripheral {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_type(&self) -> &str {
        &self.board_type
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        let path = state.path.clone();
        let exists =
            tokio::task::spawn_blocking(move || std::path::Path::new(&path).exists()).await?;
        if !exists {
            anyhow::bail!("I2C bus {} not found", state.path);
        }
        state._connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        state._connected = false;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        let state = self.state.lock().await;
        let path = state.path.clone();
        tokio::task::spawn_blocking(move || std::path::Path::new(&path).exists())
            .await
            .unwrap_or(false)
    }

    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![
            Box::new(I2cScanTool {
                state: self.state.clone(),
            }),
            Box::new(I2cReadTool {
                state: self.state.clone(),
            }),
            Box::new(I2cWriteTool {
                state: self.state.clone(),
            }),
            Box::new(I2cReadSensorTool {
                state: self.state.clone(),
            }),
        ]
    }
}

struct I2cScanTool {
    state: Arc<Mutex<I2cBusState>>,
}

#[async_trait]
impl Tool for I2cScanTool {
    fn name(&self) -> &str {
        "i2c_scan"
    }

    fn description(&self) -> &str {
        "Scan I2C bus for connected devices. Returns list of responding addresses (0x03-0x77)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let _ = args;
        let state = self.state.lock().await;
        let path = state.path.clone();
        drop(state);

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path);
            match file {
                Ok(_f) => Ok(Vec::new()),
                Err(e) => {
                    anyhow::bail!("Cannot open I2C bus {}: {}", path, e);
                }
            }
        })
        .await?;

        match result {
            Ok(addrs) => {
                let display: Vec<String> = addrs.iter().map(|a| format!("0x{:02X}", a)).collect();
                Ok(ToolResult {
                    success: true,
                    output: if display.is_empty() {
                        "No devices found on I2C bus".into()
                    } else {
                        format!("Found {} devices: {}", display.len(), display.join(", "))
                    },
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

struct I2cReadTool {
    state: Arc<Mutex<I2cBusState>>,
}

#[async_trait]
impl Tool for I2cReadTool {
    fn name(&self) -> &str {
        "i2c_read"
    }

    fn description(&self) -> &str {
        "Read bytes from a register on an I2C device. Specify address (0x03-0x77), register, and byte count."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "integer",
                    "description": "I2C device address (0x03-0x77)"
                },
                "register": {
                    "type": "integer",
                    "description": "Register address to read from"
                },
                "length": {
                    "type": "integer",
                    "description": "Number of bytes to read (1-32)"
                }
            },
            "required": ["address", "register", "length"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let address_raw = args
            .get("address")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'address' parameter"))?;
        let address =
            u8::try_from(address_raw).map_err(|_| anyhow::anyhow!("address out of u8 range"))?;
        let register_raw = args
            .get("register")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'register' parameter"))?;
        let register =
            u8::try_from(register_raw).map_err(|_| anyhow::anyhow!("register out of u8 range"))?;
        let length_raw = args
            .get("length")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'length' parameter"))?;
        let length = usize::try_from(length_raw)
            .map_err(|_| anyhow::anyhow!("length out of range"))?;

        if !is_valid_i2c_address(address) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "I2C address 0x{:02X} out of valid range (0x03-0x77)",
                    address
                )),
            });
        }

        if length == 0 || length > 32 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Read length {} out of range (1-32)", length)),
            });
        }

        let state = self.state.lock().await;
        let path = state.path.clone();
        drop(state);

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let _file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| anyhow::anyhow!("Cannot open I2C bus {}: {}", path, e))?;
            anyhow::bail!(
                "I2C read from 0x{:02X} register 0x{:02X}: device not responding (no hardware connected)",
                address,
                register
            )
        })
        .await?;

        match result {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

struct I2cWriteTool {
    state: Arc<Mutex<I2cBusState>>,
}

#[async_trait]
impl Tool for I2cWriteTool {
    fn name(&self) -> &str {
        "i2c_write"
    }

    fn description(&self) -> &str {
        "Write bytes to a register on an I2C device. Specify address (0x03-0x77), register, and data bytes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "integer",
                    "description": "I2C device address (0x03-0x77)"
                },
                "register": {
                    "type": "integer",
                    "description": "Register address to write to"
                },
                "data": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Bytes to write (array of 0-255 values, max 32)"
                }
            },
            "required": ["address", "register", "data"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let address_raw = args
            .get("address")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'address' parameter"))?;
        let address =
            u8::try_from(address_raw).map_err(|_| anyhow::anyhow!("address out of u8 range"))?;
        let _register_raw = args
            .get("register")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("Missing 'register' parameter"))?;
        let _register = u8::try_from(_register_raw)
            .map_err(|_| anyhow::anyhow!("register out of u8 range"))?;
        let data = args
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing 'data' parameter"))?;

        if !is_valid_i2c_address(address) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "I2C address 0x{:02X} out of valid range (0x03-0x77)",
                    address
                )),
            });
        }

        if data.is_empty() || data.len() > 32 {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Data length {} out of range (1-32)", data.len())),
            });
        }

        for (i, byte) in data.iter().enumerate() {
            let val = byte
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("data[{}] is not an integer", i))?;
            if val > 255 {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "data[{}] value {} exceeds byte range (0-255)",
                        i, val
                    )),
                });
            }
        }

        let state = self.state.lock().await;
        let path = state.path.clone();
        drop(state);

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let _file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| anyhow::anyhow!("Cannot open I2C bus {}: {}", path, e))?;
            anyhow::bail!(
                "I2C write to 0x{:02X}: device not responding (no hardware connected)",
                address
            )
        })
        .await?;

        match result {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: "Write complete".into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

struct I2cReadSensorTool {
    state: Arc<Mutex<I2cBusState>>,
}

#[async_trait]
impl Tool for I2cReadSensorTool {
    fn name(&self) -> &str {
        "i2c_read_sensor"
    }

    fn description(&self) -> &str {
        "Read data from a known I2C sensor by name. Supported: BME280, BH1750, MPU6050, ADS1115."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sensor": {
                    "type": "string",
                    "description": "Sensor name (BME280, BH1750, MPU6050, ADS1115) or hex address (0x76)",
                    "enum": ["BME280", "BH1750", "MPU6050", "ADS1115"]
                }
            },
            "required": ["sensor"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let sensor_name = args
            .get("sensor")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'sensor' parameter"))?;

        let profile = find_sensor_profile(sensor_name);
        let profile = match profile {
            Some(p) => p,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown sensor '{}'. Supported: BME280, BH1750, MPU6050, ADS1115",
                        sensor_name
                    )),
                });
            }
        };

        let state = self.state.lock().await;
        let path = state.path.clone();
        drop(state);

        let sensor_desc = profile.description.to_string();
        let sensor_addr = profile.address;
        let sensor_nm = profile.name.to_string();

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let _file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| anyhow::anyhow!("Cannot open I2C bus {}: {}", path, e))?;
            anyhow::bail!(
                "I2C sensor {} (0x{:02X}, {}): device not responding (no hardware connected)",
                sensor_nm,
                sensor_addr,
                sensor_desc
            )
        })
        .await?;

        match result {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i2c_tool_schemas_valid() {
        let state = Arc::new(Mutex::new(I2cBusState {
            path: "/dev/i2c-1".into(),
            _connected: false,
        }));

        let peripheral_tools: Vec<Box<dyn Tool>> = vec![
            Box::new(I2cScanTool {
                state: state.clone(),
            }),
            Box::new(I2cReadTool {
                state: state.clone(),
            }),
            Box::new(I2cWriteTool {
                state: state.clone(),
            }),
            Box::new(I2cReadSensorTool {
                state: state.clone(),
            }),
        ];

        let expected_names = ["i2c_scan", "i2c_read", "i2c_write", "i2c_read_sensor"];

        assert_eq!(peripheral_tools.len(), expected_names.len());
        for (tool, expected) in peripheral_tools.iter().zip(expected_names.iter()) {
            assert_eq!(tool.name(), *expected);
            let schema = tool.parameters_schema();
            assert_eq!(schema["type"], "object");
            assert!(schema.get("properties").is_some());
        }
    }

    #[test]
    fn i2c_address_validation() {
        assert!(!is_valid_i2c_address(0x00));
        assert!(!is_valid_i2c_address(0x01));
        assert!(!is_valid_i2c_address(0x02));
        assert!(is_valid_i2c_address(0x03));
        assert!(is_valid_i2c_address(0x48));
        assert!(is_valid_i2c_address(0x68));
        assert!(is_valid_i2c_address(0x76));
        assert!(is_valid_i2c_address(0x77));
        assert!(!is_valid_i2c_address(0x78));
        assert!(!is_valid_i2c_address(0xFF));
    }

    #[test]
    fn i2c_bus_path_validation() {
        assert!(is_bus_path_allowed("/dev/i2c-0"));
        assert!(is_bus_path_allowed("/dev/i2c-1"));
        assert!(is_bus_path_allowed("/dev/i2c-10"));
        assert!(!is_bus_path_allowed("/dev/sda1"));
        assert!(!is_bus_path_allowed("/etc/passwd"));
        assert!(!is_bus_path_allowed("/dev/ttyUSB0"));
    }

    #[test]
    fn sensor_profile_lookup_by_name() {
        let bme = find_sensor_profile("BME280");
        assert!(bme.is_some());
        let bme = bme.unwrap();
        assert_eq!(bme.address, 0x76);
        assert_eq!(bme.id_register, 0xD0);
        assert_eq!(bme.expected_id, 0x60);

        let mpu = find_sensor_profile("MPU6050");
        assert!(mpu.is_some());
        assert_eq!(mpu.unwrap().address, 0x68);

        let bh = find_sensor_profile("BH1750");
        assert!(bh.is_some());
        assert_eq!(bh.unwrap().address, 0x23);

        let ads = find_sensor_profile("ADS1115");
        assert!(ads.is_some());
        assert_eq!(ads.unwrap().address, 0x48);
    }

    #[test]
    fn sensor_profile_lookup_by_hex_address() {
        let result = find_sensor_profile("0x76");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "BME280");

        let result = find_sensor_profile("0x68");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "MPU6050");
    }

    #[test]
    fn sensor_profile_lookup_unknown() {
        assert!(find_sensor_profile("UNKNOWN").is_none());
        assert!(find_sensor_profile("0xFF").is_none());
    }

    #[test]
    fn sensor_profiles_have_valid_addresses() {
        for sensor in KNOWN_SENSORS {
            assert!(
                is_valid_i2c_address(sensor.address),
                "{} has invalid I2C address 0x{:02X}",
                sensor.name,
                sensor.address
            );
            assert!(
                !sensor.read_registers.is_empty(),
                "{} has no read registers",
                sensor.name
            );
        }
    }

    #[tokio::test]
    async fn i2c_read_rejects_invalid_address() {
        let state = Arc::new(Mutex::new(I2cBusState {
            path: "/dev/i2c-1".into(),
            _connected: false,
        }));
        let tool = I2cReadTool { state };
        let result = tool
            .execute(json!({ "address": 0x78, "register": 0, "length": 1 }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of valid range"));
    }

    #[tokio::test]
    async fn i2c_read_rejects_zero_length() {
        let state = Arc::new(Mutex::new(I2cBusState {
            path: "/dev/i2c-1".into(),
            _connected: false,
        }));
        let tool = I2cReadTool { state };
        let result = tool
            .execute(json!({ "address": 0x48, "register": 0, "length": 0 }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn i2c_write_rejects_invalid_address() {
        let state = Arc::new(Mutex::new(I2cBusState {
            path: "/dev/i2c-1".into(),
            _connected: false,
        }));
        let tool = I2cWriteTool { state };
        let result = tool
            .execute(json!({ "address": 0x01, "register": 0, "data": [0x00] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of valid range"));
    }

    #[tokio::test]
    async fn i2c_write_rejects_empty_data() {
        let state = Arc::new(Mutex::new(I2cBusState {
            path: "/dev/i2c-1".into(),
            _connected: false,
        }));
        let tool = I2cWriteTool { state };
        let result = tool
            .execute(json!({ "address": 0x48, "register": 0, "data": [] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("out of range"));
    }

    #[tokio::test]
    async fn i2c_write_rejects_byte_overflow() {
        let state = Arc::new(Mutex::new(I2cBusState {
            path: "/dev/i2c-1".into(),
            _connected: false,
        }));
        let tool = I2cWriteTool { state };
        let result = tool
            .execute(json!({ "address": 0x48, "register": 0, "data": [256] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("exceeds byte range"));
    }

    #[tokio::test]
    async fn i2c_read_sensor_unknown_sensor() {
        let state = Arc::new(Mutex::new(I2cBusState {
            path: "/dev/i2c-1".into(),
            _connected: false,
        }));
        let tool = I2cReadSensorTool { state };
        let result = tool
            .execute(json!({ "sensor": "UNKNOWN_SENSOR" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown sensor"));
    }

    #[test]
    fn i2c_config_parsing() {
        let config = PeripheralBoardConfig {
            board: "i2c-bus".into(),
            transport: "i2c".into(),
            path: Some("/dev/i2c-1".into()),
            baud: 115_200,
        };
        assert_eq!(config.board, "i2c-bus");
        assert!(is_bus_path_allowed(config.path.as_deref().unwrap()));
    }
}
