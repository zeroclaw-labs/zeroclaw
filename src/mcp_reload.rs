// SIGHUP Hot Reload Implementation for ZeroClaw
// Add this to daemon/src/main.rs or create daemon/src/signal_handler.rs

use std::sync::Arc;
use tokio::sync::RwLock;
use signal_hook::{consts::SIGHUP, iterator::Signals};
use log::{info, warn, error};

/// Signal handler for SIGHUP - triggers MCP config reload
pub struct SignalHandler {
    mcp_manager: Arc<RwLock<McpManager>>,
    config_path: std::path::PathBuf,
}

impl SignalHandler {
    pub fn new(mcp_manager: Arc<RwLock<McpManager>>, config_path: std::path::PathBuf) -> Self {
        Self { mcp_manager, config_path }
    }
    
    /// Start listening for SIGHUP signals
    pub fn start(self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut signals = Signals::new(&[SIGHUP]).expect("Failed to create signal handler");
            
            for sig in signals.forever() {
                match sig {
                    SIGHUP => {
                        info!("Received SIGHUP, reloading MCP configuration...");
                        
                        // Spawn async task to handle reload
                        let manager = self.mcp_manager.clone();
                        let config_path = self.config_path.clone();
                        
                        tokio::runtime::Handle::current().spawn(async move {
                            if let Err(e) = reload_mcps(manager, config_path).await {
                                error!("Failed to reload MCPs: {}", e);
                            }
                        });
                    }
                    _ => {}
                }
            }
        })
    }
}

/// Reload MCPs from configuration file
async fn reload_mcps(
    mcp_manager: Arc<RwLock<McpManager>>, 
    config_path: std::path::PathBuf
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Read new configuration
    let config_content = tokio::fs::read_to_string(&config_path).await?;
    let new_config: Config = toml::from_str(&config_content)?;
    
    // 2. Get current MCP configurations
    let mut manager = mcp_manager.write().await;
    
    // 3. Calculate what changed
    let old_servers = manager.get_server_configs().await;
    let new_servers = new_config.mcp.servers;
    
    let changes = calculate_mcp_changes(&old_servers, &new_servers);
    
    info!(
        "MCP changes detected: +{} added, -{} removed, ~{} modified, ={} unchanged",
        changes.added.len(),
        changes.removed.len(),
        changes.modified.len(),
        changes.unchanged.len()
    );
    
    // 4. Apply changes
    
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
        if let Err(e) = manager.restart_server(name, 
            new_servers.iter().find(|s| s.name == *name).unwrap()
        ).await {
            error!("Error restarting MCP {}: {}", name, e);
        }
    }
    
    // Start new MCPs
    for name in &changes.added {
        info!("Starting new MCP: {}", name);
        let server_config = new_servers.iter().find(|s| s.name == *name).unwrap();
        if let Err(e) = manager.start_server(server_config).await {
            error!("Error starting MCP {}: {}", name, e);
        }
    }
    
    // 5. Update stored configuration
    manager.update_configs(new_servers).await;
    
    info!("MCP reload complete");
    Ok(())
}

/// Calculate differences between old and new MCP configurations
fn calculate_mcp_changes(
    old: &[McpServerConfig], 
    new: &[McpServerConfig]
) -> McpChanges {
    let old_names: std::collections::HashSet<_> = old.iter().map(|s| &s.name).collect();
    let new_names: std::collections::HashSet<_> = new.iter().map(|s| &s.name).collect();
    
    let added: Vec<_> = new_names.difference(&old_names).cloned().collect();
    let removed: Vec<_> = old_names.difference(&new_names).cloned().collect();
    
    let mut modified = vec![];
    let mut unchanged = vec![];
    
    for name in old_names.intersection(&new_names) {
        let old_config = old.iter().find(|s| &s.name == name).unwrap();
        let new_config = new.iter().find(|s| &s.name == name).unwrap();
        
        if configs_equal(old_config, new_config) {
            unchanged.push(name.clone());
        } else {
            modified.push(name.clone());
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
    // Compare all fields that affect connection
    a.transport == b.transport &&
    a.url == b.url &&
    a.command == b.command &&
    a.args == b.args &&
    a.env == b.env &&
    a.headers == b.headers
}

#[derive(Debug)]
struct McpChanges {
    added: Vec<String>,
    removed: Vec<String>,
    modified: Vec<String>,
    unchanged: Vec<String>,
}

// ============================================================================
// Integration with existing McpManager
// Add these methods to daemon/src/mcp/manager.rs:
// ============================================================================

impl McpManager {
    /// Get current server configurations
    pub async fn get_server_configs(&self) -> Vec<McpServerConfig> {
        self.servers
            .iter()
            .map(|(name, server)| McpServerConfig {
                name: name.clone(),
                transport: server.transport(),
                url: server.url(),
                command: server.command(),
                args: server.args(),
                env: server.env(),
                headers: server.headers(),
            })
            .collect()
    }
    
    /// Update stored configurations after reload
    pub async fn update_configs(&mut self, configs: Vec<McpServerConfig>) {
        self.configs = configs;
    }
    
    /// Stop a specific MCP server
    pub async fn stop_server(&mut self, name: &str) -> Result<(), McpError> {
        if let Some(mut server) = self.servers.remove(name) {
            server.shutdown().await?;
            info!("Stopped MCP server: {}", name);
        }
        Ok(())
    }
    
    /// Start a new MCP server
    pub async fn start_server(&mut self, config: &McpServerConfig) -> Result<(), McpError> {
        let server = McpServer::new(config.clone()).await?;
        self.servers.insert(config.name.clone(), server);
        info!("Started MCP server: {}", config.name);
        Ok(())
    }
    
    /// Restart an MCP server with new configuration
    pub async fn restart_server(
        &mut self, 
        name: &str, 
        config: &McpServerConfig
    ) -> Result<(), McpError> {
        // Stop existing
        self.stop_server(name).await?;
        
        // Start with new config
        self.start_server(config).await?;
        
        info!("Restarted MCP server: {}", name);
        Ok(())
    }
}

// ============================================================================
// Modified main.rs to integrate signal handler
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ... existing setup ...
    
    let mcp_manager = Arc::new(RwLock::new(McpManager::new(&config).await?));
    
    // Start MCP servers
    {
        let mut manager = mcp_manager.write().await;
        manager.initialize_servers().await?;
    }
    
    // Start SIGHUP signal handler
    let signal_handler = SignalHandler::new(
        mcp_manager.clone(),
        config_path.clone()
    );
    let signal_thread = signal_handler.start();
    
    info!("ZeroClaw daemon started with SIGHUP support");
    info!("Send `kill -HUP {}` to reload MCP configuration", std::process::id());
    
    // ... rest of main loop ...
    
    // Wait for signal thread (optional, for graceful shutdown)
    let _ = signal_thread.join();
    
    Ok(())
}

// ============================================================================
// Cargo.toml dependencies to add
// ============================================================================

[dependencies]
# Existing dependencies...

# Signal handling
signal-hook = "0.3"
signal-hook-tokio = { version = "0.3", features = ["futures-v0_3"] }

# File watching (for Phase 2)
notify = "6.0"

# Hashing for change detection
sha2 = "0.10"

// ============================================================================
// systemd service file update for zeroclaw.service
// ============================================================================

[Service]
Type=notify
ExecStart=/usr/local/bin/zeroclaw daemon
ExecReload=/bin/kill -HUP $MAINPID
KillMode=mixed
Restart=on-failure

# This allows SIGHUP to be sent for reloading
# User can now run: systemctl --user reload zeroclaw

// ============================================================================
// CLI command to add: zeroclaw reload
// Add to cli/src/commands/mod.rs
// ============================================================================

use clap::Parser;

#[derive(Parser)]
pub struct ReloadCommand {
    /// What to reload (default: mcp)
    #[arg(default_value = "mcp")]
    pub target: String,
    
    /// Specific MCP name to reload (reloads all if not specified)
    #[arg(short, long)]
    pub name: Option<String>,
}

impl ReloadCommand {
    pub async fn execute(&self) -> Result<(), Box<dyn std::error::Error>> {
        match self.target.as_str() {
            "mcp" | "mcps" => {
                // Find daemon PID
                let pid = find_daemon_pid()?;
                
                // Send SIGHUP
                unsafe {
                    libc::kill(pid, libc::SIGHUP);
                }
                
                println!("✅ Sent reload signal to ZeroClaw daemon (PID: {})", pid);
                
                // Wait a moment and check status
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                
                // Verify reload by checking log
                if check_reload_success().await? {
                    println!("✅ MCP reload completed successfully");
                } else {
                    println!("⚠️  MCP reload status unknown, check logs");
                }
            }
            _ => {
                return Err(format!("Unknown reload target: {}", self.target).into());
            }
        }
        
        Ok(())
    }
}

fn find_daemon_pid() -> Result<i32, Box<dyn std::error::Error>> {
    // Read PID from pidfile
    let pidfile = dirs::home_dir()
        .ok_or("Cannot find home directory")?
        .join(".zeroclaw")
        .join("daemon.pid");
    
    let pid_str = std::fs::read_to_string(&pidfile)?;
    let pid: i32 = pid_str.trim().parse()?;
    
    Ok(pid)
}

async fn check_reload_success() -> Result<bool, Box<dyn std::error::Error>> {
    // Check log file for reload success message
    let logfile = dirs::home_dir()
        .ok_or("Cannot find home directory")?
        .join(".zeroclaw")
        .join("zeroclaw.log");
    
    let log_content = tokio::fs::read_to_string(&logfile).await?;
    
    // Check for recent reload completion
    Ok(log_content.contains("MCP reload complete"))
}

// ============================================================================
// Testing the implementation
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_calculate_mcp_changes() {
        let old = vec![
            McpServerConfig { name: "gina".to_string(), transport: Transport::Http, url: Some("https://example.com".to_string()), ..Default::default() },
            McpServerConfig { name: "fs".to_string(), transport: Transport::Stdio, command: Some("cmd".to_string()), ..Default::default() },
        ];
        
        let new = vec![
            McpServerConfig { name: "gina".to_string(), transport: Transport::Http, url: Some("https://new-url.com".to_string()), ..Default::default() },
            McpServerConfig { name: "new".to_string(), transport: Transport::Http, url: Some("https://new.com".to_string()), ..Default::default() },
        ];
        
        let changes = calculate_mcp_changes(&old, &new);
        
        assert_eq!(changes.added, vec!["new"]);
        assert_eq!(changes.removed, vec!["fs"]);
        assert_eq!(changes.modified, vec!["gina"]);
        assert!(changes.unchanged.is_empty());
    }
    
    #[tokio::test]
    async fn test_reload_mcps() {
        // Integration test would go here
        // Requires mock McpManager and temp config files
    }
}
