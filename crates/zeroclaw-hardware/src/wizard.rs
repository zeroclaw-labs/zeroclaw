//! Interactive hardware onboarding wizard UI.
//!
//! Provides [`run_setup`] — the hardware step of the ZeroClaw onboarding
//! wizard. The function is intended to be registered as
//! `WizardCallbacks::hardware_setup` from the binary crate.

use anyhow::Result;
use console::style;
use dialoguer::{Confirm, Select};
use zeroclaw_config::schema::{HardwareConfig, HardwareTransport};

use crate::discover_hardware;
use crate::{config_from_wizard_choice, recommended_wizard_default};

/// Run the interactive hardware setup step of the onboarding wizard.
///
/// Discovers connected devices, presents selection prompts for transport mode,
/// port, baud rate, probe target, and datasheet RAG, then returns the resulting
/// [`HardwareConfig`].
pub fn run_setup() -> Result<HardwareConfig> {
    println!(
        "  {} {}",
        style("ℹ").dim(),
        style("ZeroClaw can talk to physical hardware (LEDs, sensors, motors).").dim()
    );
    println!(
        "  {} {}",
        style("ℹ").dim(),
        style("Scanning for connected devices...").dim()
    );
    println!();

    let devices = discover_hardware();

    if devices.is_empty() {
        println!(
            "  {} {}",
            style("ℹ").dim(),
            style("No hardware devices detected on this system.").dim()
        );
        println!(
            "  {} {}",
            style("ℹ").dim(),
            style("You can enable hardware later in config.toml under [hardware].").dim()
        );
    } else {
        println!(
            "  {} {} device(s) found:",
            style("✓").green().bold(),
            devices.len()
        );
        for device in &devices {
            let detail = device
                .detail
                .as_deref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default();
            let path = device
                .device_path
                .as_deref()
                .map(|p| format!(" → {p}"))
                .unwrap_or_default();
            println!(
                "    {} {}{}{} [{}]",
                style("›").cyan(),
                style(&device.name).green(),
                style(&detail).dim(),
                style(&path).dim(),
                style(device.transport.to_string()).cyan()
            );
        }
    }
    println!();

    let options = vec![
        "🚀 Native — direct GPIO on this Linux board (Raspberry Pi, Orange Pi, etc.)",
        "🔌 Tethered — control an Arduino/ESP32/Nucleo plugged into USB",
        "🔬 Debug Probe — flash/read MCUs via SWD/JTAG (probe-rs)",
        "☁️  Software Only — no hardware access (default)",
    ];

    let recommended = recommended_wizard_default(&devices);

    let choice = Select::new()
        .with_prompt("  How should ZeroClaw interact with the physical world?")
        .items(&options)
        .default(recommended)
        .interact()?;

    let mut hw_config = config_from_wizard_choice(choice, &devices);

    // Serial: pick a port if multiple found
    if hw_config.transport_mode() == HardwareTransport::Serial {
        let serial_devices: Vec<&crate::DiscoveredDevice> = devices
            .iter()
            .filter(|d| d.transport == HardwareTransport::Serial)
            .collect();

        if serial_devices.len() > 1 {
            let port_labels: Vec<String> = serial_devices
                .iter()
                .map(|d| {
                    format!(
                        "{} ({})",
                        d.device_path.as_deref().unwrap_or("unknown"),
                        d.name
                    )
                })
                .collect();

            let port_idx = Select::new()
                .with_prompt("  Multiple serial devices found — select one")
                .items(&port_labels)
                .default(0)
                .interact()?;

            hw_config.serial_port = serial_devices[port_idx].device_path.clone();
        } else if serial_devices.is_empty() {
            let manual_port: String = dialoguer::Input::new()
                .with_prompt("  Serial port path (e.g. /dev/ttyUSB0)")
                .default("/dev/ttyUSB0".into())
                .interact_text()?;
            hw_config.serial_port = Some(manual_port);
        }

        // Baud rate
        let baud_options = vec![
            "115200 (default, recommended)",
            "9600 (legacy Arduino)",
            "57600",
            "230400",
            "Custom",
        ];
        let baud_idx = Select::new()
            .with_prompt("  Serial baud rate")
            .items(&baud_options)
            .default(0)
            .interact()?;

        hw_config.baud_rate = match baud_idx {
            1 => 9600,
            2 => 57600,
            3 => 230_400,
            4 => {
                let custom: String = dialoguer::Input::new()
                    .with_prompt("  Custom baud rate")
                    .default("115200".into())
                    .interact_text()?;
                custom.parse::<u32>().unwrap_or(115_200)
            }
            _ => 115_200,
        };
    }

    // Probe: ask for target chip
    if hw_config.transport_mode() == HardwareTransport::Probe && hw_config.probe_target.is_none() {
        let target: String = dialoguer::Input::new()
            .with_prompt("  Target MCU chip (e.g. STM32F411CEUx, nRF52840_xxAA)")
            .default("STM32F411CEUx".into())
            .interact_text()?;
        hw_config.probe_target = Some(target);
    }

    // Datasheet RAG
    if hw_config.enabled {
        let datasheets = Confirm::new()
            .with_prompt("  Enable datasheet RAG? (index PDF schematics for AI pin lookups)")
            .default(true)
            .interact()?;
        hw_config.workspace_datasheets = datasheets;
    }

    // Summary
    if hw_config.enabled {
        let transport_label = match hw_config.transport_mode() {
            HardwareTransport::Native => "Native GPIO".to_string(),
            HardwareTransport::Serial => format!(
                "Serial → {} @ {} baud",
                hw_config.serial_port.as_deref().unwrap_or("?"),
                hw_config.baud_rate
            ),
            HardwareTransport::Probe => format!(
                "Probe (SWD/JTAG) → {}",
                hw_config.probe_target.as_deref().unwrap_or("?")
            ),
            HardwareTransport::None => "Software Only".to_string(),
        };

        println!(
            "  {} Hardware: {} | datasheets: {}",
            style("✓").green().bold(),
            style(&transport_label).green(),
            if hw_config.workspace_datasheets {
                style("on").green().to_string()
            } else {
                style("off").dim().to_string()
            }
        );
    } else {
        println!(
            "  {} Hardware: {}",
            style("✓").green().bold(),
            style("disabled (software only)").dim()
        );
    }

    Ok(hw_config)
}
