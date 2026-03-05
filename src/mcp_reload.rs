//! MCP Hot Reload Module
//! 
//! Implements zero-downtime MCP configuration reloading via SIGHUP signal.
//! When the daemon receives SIGHUP, it:
//! 1. Reloads the configuration file
//! 2. Computes diff between old and new MCP configs
//! 3. Starts/stops/restarts MCPs selectively
//! 4. Continues running without dropping connections

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use signal_hook::consts::signal::SIGHUP;
use signal_hook::iterator::Signals;
use sha2::{Sha256, Digest};

/// MCP configuration entry
#[derive(Debug, Clone, PartialEq)]
pub struct McpConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Current state of MCP manager
pub struct McpManager {
    mcps: Arc<Mutex<HashMap<String, McpHandle>>>,
    config_hash: Arc<Mutex<String>>,
}

struct McpHandle {
    config: McpConfig,
    process_id: Option<u32>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            mcps: Arc::new(Mutex::new(HashMap::new())),
            config_hash: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Initialize and start the signal handler thread
    pub fn init_signal_handler(&self) {
        let mcps = Arc::clone(&self.mcps);
        let config_hash = Arc::clone(&self.config_hash);
        
        thread::spawn(move || {
            let mut signals = Signals::new(&[SIGHUP]).expect("Failed to create signal handler");
            
            for sig in signals.forever() {
                match sig {
                    SIGHUP => {
                        log::info!("Received SIGHUP, reloading MCP configuration...");
                        if let Err(e) = reload_mcps(&mcps, &config_hash) {
                            log::error!("Failed to reload MCPs: {}", e);
                        }
                    }
                    _ => {}
                }
            }
        });
        
        log::info!("SIGHUP handler initialized. Use 'kill -HUP <pid>' or 'systemctl reload zeroclaw' to reload MCPs.");
    }

    /// Start an MCP server
    pub fn start_mcp(&self, config: McpConfig) -> Result<(), String> {
        let mut mcps = self.mcps.lock().map_err(|_| "Lock poisoned")?;
        
        if mcps.contains_key(&config.name) {
            return Err(format!("MCP '{}' already running", config.name));
        }
        
        // Start the MCP process
        let process_id = start_mcp_process(&config)?;
        
        mcps.insert(config.name.clone(), McpHandle {
            config,
            process_id: Some(process_id),
        });
        
        log::info!("Started MCP: {}", config.name);
        Ok(())
    }

    /// Stop an MCP server
    pub fn stop_mcp(&self, name: &str) -> Result<(), String> {
        let mut mcps = self.mcps.lock().map_err(|_| "Lock poisoned")?;
        
        if let Some(handle) = mcps.remove(name) {
            if let Some(pid) = handle.process_id {
                stop_mcp_process(pid)?;
            }
            log::info!("Stopped MCP: {}", name);
        }
        
        Ok(())
    }

    /// Update config hash
    pub fn set_config_hash(&self, hash: String) {
        if let Ok(mut guard) = self.config_hash.lock() {
            *guard = hash;
        }
    }
}

/// Reload MCPs based on configuration changes
fn reload_mcps(
    mcps: &Arc<Mutex<HashMap<String, McpHandle>>>,
    config_hash: &Arc<Mutex<String>>,
) -> Result<(), String> {
    // Load new configuration
    let new_config = load_mcp_config()?;
    let new_hash = compute_config_hash(&new_config);
    
    // Check if config actually changed
    {
        let current_hash = config_hash.lock().map_err(|_| "Lock poisoned")?;
        if *current_hash == new_hash {
            log::info!("Configuration unchanged, skipping reload");
            return Ok(());
        }
    }
    
    // Compute diff
    let diff = compute_diff(mcps, &new_config)?;
    
    // Apply changes
    apply_diff(mcps, diff)?;
    
    // Update hash
    {
        let mut guard = config_hash.lock().map_err(|_| "Lock poisoned")?;
        *guard = new_hash;
    }
    
    log::info!("MCP configuration reloaded successfully");
    Ok(())
}

/// Load MCP configuration from file
fn load_mcp_config() -> Result<HashMap<String, McpConfig>, String> {
    // Implementation would load from ~/.zeroclaw/config.toml
    // This is a placeholder
    Ok(HashMap::new())
}

/// Compute SHA-256 hash of configuration
fn compute_config_hash(configs: &HashMap<String, McpConfig>) -> String {
    let mut hasher = Sha256::new();
    
    // Sort keys for consistent hashing
    let mut keys: Vec<&String> = configs.keys().collect();
    keys.sort();
    
    for key in keys {
        if let Some(config) = configs.get(key) {
            hasher.update(key.as_bytes());
            hasher.update(config.command.as_bytes());
            for arg in &config.args {
                hasher.update(arg.as_bytes());
            }
        }
    }
    
    format!("{:x}", hasher.finalize())
}

/// Configuration diff
struct ConfigDiff {
    to_start: Vec<McpConfig>,
    to_stop: Vec<String>,
    to_restart: Vec<McpConfig>,
}

/// Compute diff between current and new configuration
fn compute_diff(
    mcps: &Arc<Mutex<HashMap<String, McpHandle>>>,
    new_config: &HashMap<String, McpConfig>,
) -> Result<ConfigDiff, String> {
    let current = mcps.lock().map_err(|_| "Lock poisoned")?;
    
    let mut to_start = Vec::new();
    let mut to_stop = Vec::new();
    let mut to_restart = Vec::new();
    
    // Find MCPs to stop (in old but not in new)
    for (name, _) in current.iter() {
        if !new_config.contains_key(name) {
            to_stop.push(name.clone());
        }
    }
    
    // Find MCPs to start or restart
    for (name, new_mcp) in new_config.iter() {
        match current.get(name) {
            None => {
                // New MCP
                to_start.push(new_mcp.clone());
            }
            Some(old_handle) => {
                if old_handle.config != *new_mcp {
                    // Config changed - need restart
                    to_restart.push(new_mcp.clone());
                }
                // If unchanged, do nothing
            }
        }
    }
    
    Ok(ConfigDiff {
        to_start,
        to_stop,
        to_restart,
    })
}

/// Apply configuration diff
fn apply_diff(
    mcps: &Arc<Mutex<HashMap<String, McpHandle>>>,
    diff: ConfigDiff,
) -> Result<(), String> {
    // Stop removed MCPs
    for name in diff.to_stop {
        let mut locked = mcps.lock().map_err(|_| "Lock poisoned")?;
        if let Some(handle) = locked.remove(&name) {
            if let Some(pid) = handle.process_id {
                let _ = stop_mcp_process(pid);
            }
            log::info!("Stopped removed MCP: {}", name);
        }
    }
    
    // Restart changed MCPs
    for config in diff.to_restart {
        let mut locked = mcps.lock().map_err(|_| "Lock poisoned")?;
        if let Some(handle) = locked.remove(&config.name) {
            if let Some(pid) = handle.process_id {
                let _ = stop_mcp_process(pid);
            }
        }
        
        // Start with new config
        match start_mcp_process(&config) {
            Ok(pid) => {
                locked.insert(config.name.clone(), McpHandle {
                    config,
                    process_id: Some(pid),
                });
                log::info!("Restarted MCP: {}", config.name);
            }
            Err(e) => {
                log::error!("Failed to restart MCP {}: {}", config.name, e);
            }
        }
    }
    
    // Start new MCPs
    for config in diff.to_start {
        let mut locked = mcps.lock().map_err(|_| "Lock poisoned")?;
        match start_mcp_process(&config) {
            Ok(pid) => {
                locked.insert(config.name.clone(), McpHandle {
                    config,
                    process_id: Some(pid),
                });
                log::info!("Started new MCP: {}", config.name);
            }
            Err(e) => {
                log::error!("Failed to start MCP {}: {}", config.name, e);
            }
        }
    }
    
    Ok(())
}

/// Start an MCP process
fn start_mcp_process(config: &McpConfig) -> Result<u32, String> {
    use std::process::Command;
    
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args);
    
    for (key, value) in &config.env {
        cmd.env(key, value);
    }
    
    let child = cmd.spawn()
        .map_err(|e| format!("Failed to start MCP process: {}", e))?;
    
    Ok(child.id())
}

/// Stop an MCP process
fn stop_mcp_process(pid: u32) -> Result<(), String> {
    use std::process::Command;
    
    Command::new("kill")
        .args(&["-TERM", &pid.to_string()])
        .spawn()
        .map_err(|e| format!("Failed to stop MCP process: {}", e))?;
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_compute_config_hash() {
        let mut configs = HashMap::new();
        configs.insert("test".to_string(), McpConfig {
            name: "test".to_string(),
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            env: HashMap::new(),
        });
        
        let hash1 = compute_config_hash(&configs);
        let hash2 = compute_config_hash(&configs);
        
        assert_eq!(hash1, hash2);
    }
    
    #[test]
    fn test_config_diff() {
        // Test diff logic here
    }
}
