#[allow(unused_imports)]
#[cfg(feature = "hardware")]
pub use zeroclaw_hardware::peripherals::*;

use crate::config::{Config, PeripheralBoardConfig};
use anyhow::Result;
use zeroclaw_runtime::i18n::{get_required_cli_string, get_required_cli_string_with_args};

pub async fn handle_command(cmd: crate::PeripheralCommands, config: &Config) -> Result<()> {
    match cmd {
        crate::PeripheralCommands::List => {
            let boards: Vec<&PeripheralBoardConfig> = if config.peripherals.enabled {
                config.peripherals.boards.iter().collect()
            } else {
                Vec::new()
            };
            if boards.is_empty() {
                println!("{}", get_required_cli_string("cli-peripherals-none"));
                println!();
                println!("{}", get_required_cli_string("cli-peripherals-add-hint"));
                println!("{}", get_required_cli_string("cli-peripherals-add-example"));
                println!();
                println!("{}", get_required_cli_string("cli-peripherals-config-hint"));
                println!("  [peripherals]"); // i18n-exempt: literal config.toml snippet
                println!("  enabled = true"); // i18n-exempt: literal config.toml snippet
                println!();
                println!("  [[peripherals.boards]]"); // i18n-exempt: literal config.toml snippet
                println!("  board = \"nucleo-f401re\""); // i18n-exempt: literal config.toml snippet
                println!("  transport = \"serial\""); // i18n-exempt: literal config.toml snippet
                println!("  path = \"/dev/ttyACM0\""); // i18n-exempt: literal config.toml snippet
            } else {
                println!("{}", get_required_cli_string("cli-peripherals-configured"));
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
                println!(
                    "{}",
                    get_required_cli_string_with_args(
                        "cli-peripherals-already-configured",
                        &[("board", &board), ("path", &format!("{path_opt:?}"))],
                    )
                );
                return Ok(());
            }

            cfg.peripherals.boards.push(PeripheralBoardConfig {
                board: board.clone(),
                transport: transport.to_string(),
                path: path_opt,
                baud: 115_200,
            });
            Box::pin(cfg.save()).await?;
            println!(
                "{}",
                get_required_cli_string_with_args(
                    "cli-peripherals-added",
                    &[("board", &board), ("path", &path)],
                )
            );
        }
        #[cfg(feature = "hardware")]
        crate::PeripheralCommands::Flash { port } => {
            let port_str = arduino_flash::resolve_port(config, port.as_deref())
                .or_else(|| port.clone())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "peripheral flash refused: no port resolved (no --port flag and no arduino-uno in config)"
                    );
                    anyhow::Error::msg(
                        "No port specified. Use --port /dev/cu.usbmodem* or add arduino-uno to config.toml"
                    )
                })?;
            arduino_flash::flash_arduino_firmware(&port_str)?;
        }
        #[cfg(not(feature = "hardware"))]
        crate::PeripheralCommands::Flash { .. } => {
            println!(
                "{}",
                get_required_cli_string("cli-peripherals-flash-needs-hardware")
            );
            println!("{}", get_required_cli_string("cli-hardware-feature-build"));
        }
        #[cfg(feature = "hardware")]
        crate::PeripheralCommands::SetupUnoQ { host } => {
            uno_q_setup::setup_uno_q_bridge(host.as_deref())?;
        }
        #[cfg(not(feature = "hardware"))]
        crate::PeripheralCommands::SetupUnoQ { .. } => {
            println!(
                "{}",
                get_required_cli_string("cli-peripherals-unoq-needs-hardware")
            );
            println!("{}", get_required_cli_string("cli-hardware-feature-build"));
        }
        #[cfg(feature = "hardware")]
        crate::PeripheralCommands::FlashNucleo => {
            nucleo_flash::flash_nucleo_firmware()?;
        }
        #[cfg(not(feature = "hardware"))]
        crate::PeripheralCommands::FlashNucleo => {
            println!(
                "{}",
                get_required_cli_string("cli-peripherals-nucleo-needs-hardware")
            );
            println!("{}", get_required_cli_string("cli-hardware-feature-build"));
        }
    }
    Ok(())
}
