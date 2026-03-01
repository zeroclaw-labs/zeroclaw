use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing_subscriber::EnvFilter;
use zeroclaw::config::schema::McpServerConfig;

#[derive(Default, Deserialize)]
struct FileMcp {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

#[derive(Default, Deserialize)]
struct FileRoot {
    #[serde(default)]
    mcp: FileMcp,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let (enabled, servers) = match std::fs::read_to_string("config.toml") {
        Ok(s) => {
            let start = s
                .lines()
                .position(|line| line.trim() == "[mcp]")
                .unwrap_or(0);
            let slice = s.lines().skip(start).collect::<Vec<_>>().join("\n");
            let root: FileRoot = toml::from_str(&slice).context("failed to parse ./config.toml")?;
            (root.mcp.enabled, root.mcp.servers)
        }
        Err(_) => {
            let config = zeroclaw::Config::load_or_init().await?;
            (config.mcp.enabled, config.mcp.servers)
        }
    };

    if !enabled || servers.is_empty() {
        bail!("MCP is disabled or no servers configured");
    }

    let registry = zeroclaw::tools::McpRegistry::connect_all(&servers).await?;
    let tool_count = registry.tool_names().len();
    tracing::info!(
        "MCP smoke ok: {} server(s), {} tool(s)",
        registry.server_count(),
        tool_count
    );

    if registry.server_count() == 0 {
        bail!("no MCP servers connected");
    }

    Ok(())
}
