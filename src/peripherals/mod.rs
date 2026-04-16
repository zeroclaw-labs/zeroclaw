#[allow(unused_imports)]
#[cfg(feature = "hardware")]
pub use zeroclaw_hardware::peripherals::*;

use crate::config::{Config, PeripheralBoardConfig};
use anyhow::Result;

pub async fn handle_command(cmd: crate::PeripheralCommands, config: &Config) -> Result<()> {
    match cmd {
        crate::PeripheralCommands::List => {
            let boards: Vec<&PeripheralBoardConfig> = if config.peripherals.enabled {
                config.peripherals.boards.iter().collect()
            } else {
                Vec::new()
            };
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

            let mut cfg = Box::pin(crate::config::Config::load_or_init()).await?;
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
                baud: 115_200,
            });
            cfg.save().await?;
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
