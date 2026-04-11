#[allow(unused_imports)]
pub use zeroclaw_runtime::integrations::*;

use crate::config::Config;
use anyhow::Result;

#[allow(dead_code)]
pub fn handle_command(command: crate::IntegrationCommands, config: &Config) -> Result<()> {
    match command {
        crate::IntegrationCommands::Info { name } => show_integration_info(config, &name),
    }
}
