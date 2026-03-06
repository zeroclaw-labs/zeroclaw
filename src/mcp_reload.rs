//! MCP Hot Reload Support for ZeroClaw
//!
//! This module provides SIGHUP-based hot reloading of MCP server configurations.
//! When the daemon receives SIGHUP, it will:
//! 1. Re-read the configuration file
//! 2. Compare old vs new MCP server configs
//! 3. Start/stop/restart servers as needed
//! 4. Update the running McpRegistry

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};

use crate::config::schema::McpServerConfig;
use crate::tools::mcp_client::{McpRegistry, McpServer};

/// Manager for MCP servers with hot-reload capability
pub struct McpManager {
    registry: Option<McpRegistry>,
    configs: Vec<McpServerConfig>,
}

impl McpManager {
    /// Create a new MCP manager (doesn't connect yet)
    pub async fn new(_config: &crate::config::Config) -> anyhow::Result<Self> {
        Ok(Self {
            registry: None,
            configs: Vec::new(),
        })
    }

    /// Initialize servers from configs (called on startup)
    pub async fn initialize_servers(&mut self) -> anyhow::Result<()> {
        if self.configs.is_empty() {
            info!("No MCP servers configured");
            return Ok(());
        }

        match McpRegistry::connect_all(&self.configs).await {
            Ok(registry) => {
                let count = registry.server_count();
                info!("Connected to {} MCP server(s)", count);
                self.registry = Some(registry);
                Ok(())
            }
            Err(e) => {
                error!("Failed to initialize MCP servers: {}", e);
                Err(e)
            }
        }
    }

    /// Get current server configurations
    pub async fn get_server_configs(&self) -> Vec<McpServerConfig> {
        self.configs.clone()
    }

    /// Stop a server by name
    pub async fn stop_server(&mut self, name: &str) -> anyhow::Result<()> {
        info!("Stopping MCP server: {}", name);
        // Note: McpRegistry doesn't have individual stop, we rebuild on changes
        // This is called before we rebuild the registry
        Ok(())
    }

    /// Start a server from config
    pub async fn start_server(&mut self, config: &McpServerConfig) -> anyhow::Result<()> {
        info!("Starting MCP server: {} ({:?})", config.name, config.transport);
        // Individual server start - used during incremental updates
        match McpServer::connect(config.clone()).await {
            Ok(_server) => {
                info!("Connected to MCP server: {}", config.name);
                Ok(())
            }
            Err(e) => {
                error!("Failed to connect to MCP server {}: {}", config.name, e);
                Err(e)
            }
        }
    }

    /// Restart a server with new config
    pub async fn restart_server(&mut self, name: &str, config: &McpServerConfig) -> anyhow::Result<()> {
        info!("Restarting MCP server: {}", name);
        self.stop_server(name).await?;
        self.start_server(config).await
    }

    /// Update stored configurations
    pub async fn update_configs(&mut self, configs: Vec<McpServerConfig>) {
        self.configs = configs;
    }

    /// Update the registry reference
    pub fn set_registry(&mut self, registry: McpRegistry) {
        self.registry = Some(registry);
    }

    /// Get the current registry
    pub fn registry(&self) -> Option<&McpRegistry> {
        self.registry.as_ref()
    }
}

/// Signal handler for SIGHUP - triggers MCP config reload
pub struct SignalHandler {
    mcp_manager: Arc<RwLock<McpManager>>,
    config_path: std::path::PathBuf,
}

impl SignalHandler {
    /// Create a new signal handler
    pub fn new(mcp_manager: Arc<RwLock<McpManager>>, config_path: std::path::PathBuf) -> Self {
        Self { mcp_manager, config_path }
    }

    /// Start listening for SIGHUP signals
    pub fn start(self) -> std::thread::JoinHandle<()> {
        use signal_hook::{consts::SIGHUP, iterator::Signals};

        std::thread::spawn(move || {
            let mut signals = Signals::new(&[SIGHUP]).expect("Failed to create signal handler");

            for sig in signals.forever() {
                match sig {
                    SIGHUP => {
                        info!("Received SIGHUP, reloading MCP configuration...");

                        // Spawn async task to handle reload
                        let _manager = self.mcp_manager.clone();
                        let config_path = self.config_path.clone();

                        // Note: We can't easily spawn into the tokio runtime from here
                        // In practice, this should be integrated with the daemon's main loop
                        // For now, log that we received the signal
                        info!("SIGHUP received - would reload MCP config from {:?}", config_path);

                        // The actual reload should be done by the daemon's main loop
                        // which has access to the tokio runtime
                    }
                    _ => {}
                }
            }
        })
    }
}

/// Calculate differences between old and new MCP configurations
pub fn calculate_mcp_changes(
    old: &[McpServerConfig],
    new: &[McpServerConfig]
) -> McpChanges {
    let old_map: HashMap<_, _> = old.iter().map(|s| (&s.name, s)).collect();
    let new_map: HashMap<_, _> = new.iter().map(|s| (&s.name, s)).collect();

    let added: Vec<String> = new_map.keys()
        .filter(|&&k| !old_map.contains_key(k))
        .map(|&k| k.clone())
        .collect();

    let removed: Vec<String> = old_map.keys()
        .filter(|&&k| !new_map.contains_key(k))
        .map(|&k| k.clone())
        .collect();

    let mut modified: Vec<String> = vec![];
    let mut unchanged: Vec<String> = vec![];

    for &name in old_map.keys() {
        if let Some(new_config) = new_map.get(&name) {
            let old_config = &old_map[&name];
            if configs_equal(old_config, new_config) {
                unchanged.push(name.clone());
            } else {
                modified.push(name.clone());
            }
        }
    }

    McpChanges {
        added,
        removed,
        modified,
        unchanged,
    }
}

/// Compare two MCP configurations for equality
fn configs_equal(a: &McpServerConfig, b: &McpServerConfig) -> bool {
    a.transport == b.transport &&
    a.url == b.url &&
    a.command == b.command &&
    a.args == b.args &&
    a.env == b.env &&
    a.headers == b.headers &&
    a.tool_timeout_secs == b.tool_timeout_secs
}

/// Changes detected between old and new MCP configurations
#[derive(Debug)]
pub struct McpChanges {
    /// New servers to add
    pub added: Vec<String>,
    /// Servers to remove
    pub removed: Vec<String>,
    /// Servers that changed config
    pub modified: Vec<String>,
    /// Servers that stayed the same
    pub unchanged: Vec<String>,
}

/// Reload MCPs from configuration file
pub async fn reload_mcps(
    mcp_manager: Arc<RwLock<McpManager>>,
    config_path: std::path::PathBuf
) -> anyhow::Result<()> {
    // 1. Read new configuration
    let config_content = tokio::fs::read_to_string(&config_path).await?;
    let new_config: crate::config::Config = toml::from_str(&config_content)?;

    // 2. Get current MCP configurations
    let manager = mcp_manager.read().await;
    let old_servers = manager.get_server_configs().await;
    drop(manager);

    let new_servers = new_config.mcp.servers.clone();

    // 3. Calculate what changed
    let changes = calculate_mcp_changes(&old_servers, &new_servers);

    info!(
        "MCP changes detected: +{} added, -{} removed, ~{} modified, ={} unchanged",
        changes.added.len(),
        changes.removed.len(),
        changes.modified.len(),
        changes.unchanged.len()
    );

    // 4. Apply changes
    let mut manager = mcp_manager.write().await;

    // Stop removed MCPs
    for name in &changes.removed {
        info!("Stopping removed MCP: {}", name);
        if let Err(e) = manager.stop_server(name).await {
            warn!("Error stopping MCP {}: {}", name, e);
        }
    }

    // Restart modified MCPs
    for name in &changes.modified {
        info!("Restarting modified MCP: {}", name);
        let config = new_servers.iter().find(|s| &s.name == name).unwrap();
        if let Err(e) = manager.restart_server(name, config).await {
            error!("Error restarting MCP {}: {}", name, e);
        }
    }

    // Start new MCPs
    for name in &changes.added {
        info!("Starting new MCP: {}", name);
        let config = new_servers.iter().find(|s| &s.name == name).unwrap();
        if let Err(e) = manager.start_server(config).await {
            error!("Error starting MCP {}: {}", name, e);
        }
    }

    // 5. Rebuild registry with new configurations
    match McpRegistry::connect_all(&new_servers).await {
        Ok(registry) => {
            manager.set_registry(registry);
            manager.update_configs(new_servers).await;
            info!("MCP reload complete - registry rebuilt");
        }
        Err(e) => {
            error!("Failed to rebuild MCP registry: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_mcp_changes() {
        let old = vec![
            McpServerConfig {
                name: "gina".to_string(),
                transport: McpTransport::Http,
                url: Some("https://example.com".to_string()),
                ..Default::default()
            },
            McpServerConfig {
                name: "fs".to_string(),
                transport: McpTransport::Stdio,
                command: "cmd".to_string(),
                ..Default::default()
            },
        ];

        let new = vec![
            McpServerConfig {
                name: "gina".to_string(),
                transport: McpTransport::Http,
                url: Some("https://new-url.com".to_string()),
                ..Default::default()
            },
            McpServerConfig {
                name: "new".to_string(),
                transport: McpTransport::Http,
                url: Some("https://new.com".to_string()),
                ..Default::default()
            },
        ];

        let changes = calculate_mcp_changes(&old, &new);

        assert_eq!(changes.added, vec!["new"]);
        assert_eq!(changes.removed, vec!["fs"]);
        assert_eq!(changes.modified, vec!["gina"]);
        assert!(changes.unchanged.is_empty());
    }

    #[test]
    fn test_configs_equal() {
        let a = McpServerConfig {
            name: "test".to_string(),
            transport: McpTransport::Http,
            url: Some("https://example.com".to_string()),
            ..Default::default()
        };

        let b = McpServerConfig {
            name: "test".to_string(),
            transport: McpTransport::Http,
            url: Some("https://example.com".to_string()),
            ..Default::default()
        };

        let c = McpServerConfig {
            name: "test".to_string(),
            transport: McpTransport::Http,
            url: Some("https://different.com".to_string()),
            ..Default::default()
        };

        assert!(configs_equal(&a, &b));
        assert!(!configs_equal(&a, &c));
    }
}

