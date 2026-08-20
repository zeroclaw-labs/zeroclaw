pub use zeroclaw_runtime::service::*;

use crate::config::Config;
use anyhow::Result;

pub fn handle_command(
    command: &crate::ServiceCommands,
    config: &Config,
    init_system: InitSystem,
) -> Result<()> {
    match command {
        crate::ServiceCommands::RunLaunchdDaemon => {
            anyhow::bail!("internal launchd runner must dispatch before config loading")
        }
        crate::ServiceCommands::RunOpenrcLogWriter { .. } => {
            anyhow::bail!("internal OpenRC logger must dispatch before config loading")
        }
        crate::ServiceCommands::Install => install(config, init_system),
        crate::ServiceCommands::Start => start(config, init_system),
        crate::ServiceCommands::Stop => stop(config, init_system),
        crate::ServiceCommands::Restart => restart(config, init_system),
        crate::ServiceCommands::Status => status(config, init_system),
        crate::ServiceCommands::Uninstall => uninstall(config, init_system),
        crate::ServiceCommands::Logs { lines, follow } => {
            logs(config, init_system, *lines, *follow)
        }
    }
}
