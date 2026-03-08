/// OpenClaw compatible node client
///
/// Allows ZeroClaw instances to connect to an OpenClaw gateway as agent-delegation nodes.
/// Implements the OpenClaw WebSocket protocol (v3) for challenge-response authentication,
/// heartbeat ticks, and remote command invocation.

pub mod client;
pub mod identity;
pub mod protocol;
pub mod runner;

#[cfg(test)]
mod tests;

pub use runner::OpenClawNodeRunner;

pub async fn run_openclaw_node(config: crate::config::Config) -> anyhow::Result<()> {
    let runner = OpenClawNodeRunner::new(config);
    runner.run().await
}
