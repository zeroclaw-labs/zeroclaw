//! Hardware peripherals — STM32, RPi GPIO, etc.
//!
//! Peripherals extend the agent with physical capabilities. See
//! `docs/hardware-peripherals-design.md` for the full design.

pub mod traits;

#[cfg(feature = "hardware")]
pub mod serial;

#[cfg(feature = "hardware")]
pub mod arduino_flash;
#[cfg(feature = "hardware")]
pub mod arduino_upload;
#[cfg(feature = "hardware")]
pub mod capabilities_tool;
#[cfg(feature = "hardware")]
pub mod nucleo_flash;
#[cfg(feature = "hardware")]
pub mod smartroom;
#[cfg(feature = "hardware")]
pub mod uno_q_bridge;
#[cfg(feature = "hardware")]
pub mod uno_q_setup;

#[cfg(all(feature = "peripheral-rpi", target_os = "linux"))]
pub mod rpi;

#[cfg(any(feature = "hardware", feature = "peripheral-rpi"))]
pub use traits::Peripheral;

use anyhow::Result;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::schema::{PeripheralBoardConfig, PeripheralsConfig};
#[cfg(feature = "hardware")]
use zeroclaw_tools::hardware_memory_map::HardwareMemoryMapTool;

/// List configured boards from config (no connection yet).
pub fn list_configured_boards(config: &PeripheralsConfig) -> Vec<&PeripheralBoardConfig> {
    if !config.enabled {
        return Vec::new();
    }
    config.boards.iter().collect()
}

/// Create and connect peripherals from config, returning their tools.
/// Returns empty vec if peripherals disabled or hardware feature off.
#[cfg(feature = "hardware")]
pub async fn create_peripheral_tools(config: &PeripheralsConfig) -> Result<Vec<Box<dyn Tool>>> {
    if !config.enabled || config.boards.is_empty() {
        return Ok(Vec::new());
    }

    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    let mut serial_transports: Vec<(String, std::sync::Arc<serial::SerialTransport>)> = Vec::new();

    for board in &config.boards {
        // Arduino Uno Q: Bridge transport (socket to local Bridge app)
        if board.transport == "bridge" && (board.board == "arduino-uno-q" || board.board == "uno-q")
        {
            tools.push(Box::new(uno_q_bridge::UnoQGpioReadTool));
            tools.push(Box::new(uno_q_bridge::UnoQGpioWriteTool));
            tracing::info!(board = %board.board, "Uno Q Bridge GPIO tools added");
            continue;
        }

        // Native transport: RPi GPIO (Linux only)
        #[cfg(all(feature = "peripheral-rpi", target_os = "linux"))]
        if board.transport == "native"
            && (board.board == "rpi-gpio" || board.board == "raspberry-pi")
        {
            match rpi::RpiGpioPeripheral::connect_from_config(board).await {
                Ok(peripheral) => {
                    tools.extend(peripheral.tools());
                    tracing::info!(board = %board.board, "RPi GPIO peripheral connected");
                }
                Err(e) => {
                    tracing::warn!("Failed to connect RPi GPIO {}: {}", board.board, e);
                }
            }
            continue;
        }

        // Serial transport (STM32, ESP32, Arduino, etc.)
        if board.transport != "serial" {
            continue;
        }
        if board.path.is_none() {
            tracing::warn!("Skipping serial board {}: no path", board.board);
            continue;
        }

        match serial::SerialPeripheral::connect(board).await {
            Ok(peripheral) => {
                let mut p = peripheral;
                if p.connect().await.is_err() {
                    tracing::warn!("Peripheral {} connect warning (continuing)", p.name());
                }
                let transport = p.transport();
                serial_transports.push((board.board.clone(), transport.clone()));
                tools.extend(p.tools());
                if board.board == "arduino-uno"
                    && let Some(ref path) = board.path
                {
                    tools.push(Box::new(arduino_upload::ArduinoUploadTool::new(
                        path.clone(),
                    )));
                    tracing::info!("Arduino upload tool added (port: {})", path);
                }
                // Smart-room demo: high-level device-name tools that hide pin
                // numbers from the LLM. Triggered by board=esp32 (the demo sim).
                if board.board == "esp32" || board.board == "esp32-sim" {
                    tools.push(Box::new(smartroom::SetDeviceTool {
                        transport: transport.clone(),
                    }));
                    tools.push(Box::new(smartroom::ReadDeviceTool {
                        transport: transport.clone(),
                    }));
                    tracing::info!("Smart-room device tools added (set_device, read_device)");
                }
                tracing::info!(board = %board.board, "Serial peripheral connected");
            }
            Err(e) => {
                tracing::warn!("Failed to connect {}: {}", board.board, e);
            }
        }
    }

    // Phase B: Add hardware tools when any boards configured
    if !tools.is_empty() {
        let board_names: Vec<String> = config.boards.iter().map(|b| b.board.clone()).collect();
        tools.push(Box::new(HardwareMemoryMapTool::new(board_names.clone())));
        tools.push(Box::new(
            zeroclaw_tools::hardware_board_info::HardwareBoardInfoTool::new(board_names.clone()),
        ));
        tools.push(Box::new(
            zeroclaw_tools::hardware_memory_read::HardwareMemoryReadTool::new(board_names),
        ));
    }

    // Phase C: Add hardware_capabilities tool when any serial boards
    if !serial_transports.is_empty() {
        tools.push(Box::new(capabilities_tool::HardwareCapabilitiesTool::new(
            serial_transports,
        )));
    }

    Ok(tools)
}

#[cfg(not(feature = "hardware"))]
#[allow(clippy::unused_async)]
pub async fn create_peripheral_tools(_config: &PeripheralsConfig) -> Result<Vec<Box<dyn Tool>>> {
    Ok(Vec::new())
}

/// Create probe-rs / static board info tools (hardware_board_info, hardware_memory_map,
/// hardware_memory_read). These use USB/probe-rs or static datasheet data — they never
/// open a serial port, so they are safe to register regardless of the `hardware` feature.
#[cfg(feature = "hardware")]
pub fn create_board_info_tools(config: &PeripheralsConfig) -> Vec<Box<dyn Tool>> {
    if !config.enabled || config.boards.is_empty() {
        return Vec::new();
    }
    let board_names: Vec<String> = config.boards.iter().map(|b| b.board.clone()).collect();
    vec![
        Box::new(
            zeroclaw_tools::hardware_memory_map::HardwareMemoryMapTool::new(board_names.clone()),
        ),
        Box::new(
            zeroclaw_tools::hardware_board_info::HardwareBoardInfoTool::new(board_names.clone()),
        ),
        Box::new(zeroclaw_tools::hardware_memory_read::HardwareMemoryReadTool::new(board_names)),
    ]
}

#[cfg(not(feature = "hardware"))]
pub fn create_board_info_tools(_config: &PeripheralsConfig) -> Vec<Box<dyn Tool>> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{PeripheralBoardConfig, PeripheralsConfig};

    #[test]
    fn list_configured_boards_when_disabled_returns_empty() {
        let config = PeripheralsConfig {
            enabled: false,
            boards: vec![PeripheralBoardConfig {
                board: "nucleo-f401re".into(),
                transport: "serial".into(),
                path: Some("/dev/ttyACM0".into()),
                baud: 115_200,
            }],
            datasheet_dir: None,
        };
        let result = list_configured_boards(&config);
        assert!(
            result.is_empty(),
            "disabled peripherals should return no boards"
        );
    }

    #[test]
    fn list_configured_boards_when_enabled_with_boards() {
        let config = PeripheralsConfig {
            enabled: true,
            boards: vec![
                PeripheralBoardConfig {
                    board: "nucleo-f401re".into(),
                    transport: "serial".into(),
                    path: Some("/dev/ttyACM0".into()),
                    baud: 115_200,
                },
                PeripheralBoardConfig {
                    board: "rpi-gpio".into(),
                    transport: "native".into(),
                    path: None,
                    baud: 115_200,
                },
            ],
            datasheet_dir: None,
        };
        let result = list_configured_boards(&config);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].board, "nucleo-f401re");
        assert_eq!(result[1].board, "rpi-gpio");
    }

    #[test]
    fn list_configured_boards_when_enabled_but_no_boards() {
        let config = PeripheralsConfig {
            enabled: true,
            boards: vec![],
            datasheet_dir: None,
        };
        let result = list_configured_boards(&config);
        assert!(
            result.is_empty(),
            "enabled with no boards should return empty"
        );
    }

    #[tokio::test]
    async fn create_peripheral_tools_returns_empty_when_disabled() {
        let config = PeripheralsConfig {
            enabled: false,
            boards: vec![],
            datasheet_dir: None,
        };
        let tools = create_peripheral_tools(&config).await.unwrap();
        assert!(
            tools.is_empty(),
            "disabled peripherals should produce no tools"
        );
    }
}
