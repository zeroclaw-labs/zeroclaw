#[allow(unused_imports)]
#[cfg(feature = "hardware")]
pub use zeroclaw_hardware::*;

use crate::config::Config;
use anyhow::Result;

#[allow(dead_code)]
pub fn handle_command(cmd: crate::HardwareCommands, _config: &Config) -> Result<()> {
    #[cfg(not(feature = "hardware"))]
    {
        let _ = &cmd;
        println!("Hardware discovery requires the 'hardware' feature.");
        println!("Build with: cargo build --features hardware");
        Ok(())
    }

    #[cfg(all(
        feature = "hardware",
        not(any(target_os = "linux", target_os = "macos", target_os = "windows"))
    ))]
    {
        let _ = &cmd;
        println!("Hardware USB discovery is not supported on this platform.");
        println!("Supported platforms: Linux, macOS, Windows.");
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
