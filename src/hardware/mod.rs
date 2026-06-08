#[allow(unused_imports)]
#[cfg(feature = "hardware")]
pub use zeroclaw_hardware::*;

use crate::config::Config;
use anyhow::Result;
#[allow(unused_imports)]
use zeroclaw_runtime::i18n::get_required_cli_string;

#[allow(dead_code)]
pub fn handle_command(cmd: crate::HardwareCommands, _config: &Config) -> Result<()> {
    #[cfg(not(feature = "hardware"))]
    {
        let _ = &cmd;
        println!(
            "{}",
            get_required_cli_string("cli-hardware-feature-required")
        );
        println!("{}", get_required_cli_string("cli-hardware-feature-build"));
        Ok(())
    }

    #[cfg(all(
        feature = "hardware",
        not(any(target_os = "linux", target_os = "macos", target_os = "windows"))
    ))]
    {
        let _ = &cmd;
        println!(
            "{}",
            get_required_cli_string("cli-hardware-unsupported-platform")
        );
        println!(
            "{}",
            get_required_cli_string("cli-hardware-supported-platforms")
        );
        return Ok(());
    }

    #[cfg(all(
        feature = "hardware",
        any(target_os = "linux", target_os = "macos", target_os = "windows")
    ))]
    match cmd {
        crate::HardwareCommands::Discover => run_discover(),
        crate::HardwareCommands::Introspect { path } => run_introspect(&path),
        crate::HardwareCommands::Info { chip } => run_info(&chip),
    }
}
