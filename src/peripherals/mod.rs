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
pub mod uno_q_bridge;
#[cfg(feature = "hardware")]
pub mod uno_q_setup;

#[cfg(all(feature = "peripheral-rpi", target_os = "linux"))]
pub mod rpi;

pub use traits::Peripheral;

use crate::config::{Config, PeripheralBoardConfig, PeripheralsConfig};
use crate::tools::{HardwareMemoryMapTool, Tool};
use anyhow::Result;

/// List configured boards from config (no connection yet).
pub fn list_configured_boards(config: &PeripheralsConfig) -> Vec<&PeripheralBoardConfig> {
    if !config.enabled {
        return Vec::new();
    }
    config.boards.iter().collect()
}

/// Handle `zeroclaw peripheral` subcommands.
#[allow(clippy::module_name_repetitions)]
pub fn handle_command(cmd: crate::PeripheralCommands, config: &Config) -> Result<()> {
    match cmd {
        crate::PeripheralCommands::List => {
            let boards = list_configured_boards(&config.peripherals);
            if boards.is_empty() {
                println!("No peripherals configured.");
                println!();
                println!("Add one with: zeroclaw peripheral add <board> <path>");
                println!("  Example: zeroclaw peripheral add nucleo-f401re /dev/ttyACM0");
                println!();
                println!("Or add to config.toml:");
                println!("  [peripherals]");
                println!("  enabled = true");
                println!();
                println!("  [[peripherals.boards]]");
                println!("  board = \"nucleo-f401re\"");
                println!("  transport = \"serial\"");
                println!("  path = \"/dev/ttyACM0\"");
            } else {
                println!("Configured peripherals:");
                for b in boards {
                    let path = b.path.as_deref().unwrap_or("(native)");
                    println!("  {}  {}  {}", b.board, b.transport, path);
                }
            }
        }
        crate::PeripheralCommands::Add { board, path } => {
            let transport = if path == "native" { "native" } else { "serial" };
            let path_opt = if path == "native" {
                None
            } else {
                Some(path.clone())
            };

            let mut cfg = crate::config::Config::load_or_init()?;
            cfg.peripherals.enabled = true;

            if cfg
                .peripherals
                .boards
                .iter()
                .any(|b| b.board == board && b.path.as_deref() == path_opt.as_deref())
            {
                println!("Board {} at {:?} already configured.", board, path_opt);
                return Ok(());
            }

            cfg.peripherals.boards.push(PeripheralBoardConfig {
                board: board.clone(),
                transport: transport.to_string(),
                path: path_opt,
                baud: 115200,
            });
            cfg.save()?;
            println!("Added {} at {}. Restart daemon to apply.", board, path);
        }
        #[cfg(feature = "hardware")]
        crate::PeripheralCommands::Flash { port } => {
            let port_str = arduino_flash::resolve_port(config, port.as_deref())
                .or_else(|| port.clone())
                .ok_or_else(|| anyhow::anyhow!(
                    "No port specified. Use --port /dev/cu.usbmodem* or add arduino-uno to config.toml"
                ))?;
            arduino_flash::flash_arduino_firmware(&port_str)?;
        }
        #[cfg(not(feature = "hardware"))]
        crate::PeripheralCommands::Flash { .. } => {
            println!("Arduino flash requires the 'hardware' feature.");
            println!("Build with: cargo build --features hardware");
        }
        #[cfg(feature = "hardware")]
        crate::PeripheralCommands::SetupUnoQ { host } => {
            uno_q_setup::setup_uno_q_bridge(host.as_deref())?;
        }
        #[cfg(not(feature = "hardware"))]
        crate::PeripheralCommands::SetupUnoQ { .. } => {
            println!("Uno Q setup requires the 'hardware' feature.");
            println!("Build with: cargo build --features hardware");
        }
        #[cfg(feature = "hardware")]
        crate::PeripheralCommands::FlashNucleo => {
            nucleo_flash::flash_nucleo_firmware()?;
        }
        #[cfg(not(feature = "hardware"))]
        crate::PeripheralCommands::FlashNucleo => {
            println!("Nucleo flash requires the 'hardware' feature.");
            println!("Build with: cargo build --features hardware");
        }
    }
    Ok(())
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
                serial_transports.push((board.board.clone(), p.transport()));
                tools.extend(p.tools());
                if board.board == "arduino-uno" {
                    if let Some(ref path) = board.path {
                        tools.push(Box::new(arduino_upload::ArduinoUploadTool::new(
                            path.clone(),
                        )));
                        tracing::info!("Arduino upload tool added (port: {})", path);
                    }
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
        tools.push(Box::new(crate::tools::HardwareBoardInfoTool::new(
            board_names.clone(),
        )));
        tools.push(Box::new(crate::tools::HardwareMemoryReadTool::new(
            board_names,
        )));
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
pub async fn create_peripheral_tools(_config: &PeripheralsConfig) -> Result<Vec<Box<dyn Tool>>> {
    Ok(Vec::new())
}
