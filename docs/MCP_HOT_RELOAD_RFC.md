# MCP Hot Reload Architecture RFC

## Problem Statement
Currently, adding or modifying MCP servers requires a full ZeroClaw daemon restart:
```bash
systemctl --user restart zeroclaw
```

This is disruptive because:
- Loses in-memory context/conversation state
- Interrupts active operations
- Poor user experience
- Prevents dynamic MCP management

## Proposed Solution: Multi-Layer Reload Architecture

### Layer 1: SIGHUP Signal Handler (Immediate Implementation)

**Unix Standard Approach**
```rust
// In ZeroClaw daemon
use std::sync::Arc;
use tokio::sync::RwLock;
use signal_hook::consts::SIGHUP;
use signal_hook_tokio::Signals;

async fn handle_reload_signal(
    mcp_manager: Arc<RwLock<McpManager>>,
    config_path: PathBuf,
) {
    let mut signals = Signals::new(&[SIGHUP]).unwrap();
    
    for sig in signals.recv() {
        match sig {
            SIGHUP => {
                log::info!("Received SIGHUP, reloading MCP configuration...");
                
                // 1. Parse new config
                let new_config = match load_mcp_config(&config_path).await {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        log::error!("Failed to parse MCP config: {}", e);
                        continue;
                    }
                };
                
                // 2. Diff with current config
                let mut manager = mcp_manager.write().await;
                let changes = manager.diff_config(&new_config);
                
                // 3. Apply changes granularly
                if let Err(e) = manager.apply_changes(changes).await {
                    log::error!("Failed to apply MCP changes: {}", e);
                }
                
                log::info!("MCP reload complete");
            }
            _ => {}
        }
    }
}
```

**User Experience:**
```bash
# Instead of restart
kill -HUP $(pgrep zeroclaw)
# or
systemctl reload zeroclaw
```

### Layer 2: File Watch with Debouncing

**Automatic reload on config changes:**
```rust
use notify::{Watcher, RecursiveMode, DebouncedEvent};
use std::time::Duration;

async fn watch_config_changes(
    config_path: PathBuf,
    reload_tx: mpsc::Sender<()>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher: RecommendedWatcher = Watcher::new(
        tx, 
        Duration::from_secs(2)  // Debounce period
    ).unwrap();
    
    watcher.watch(&config_path, RecursiveMode::NonRecursive).unwrap();
    
    loop {
        match rx.recv() {
            Ok(DebouncedEvent::Write(_)) | Ok(DebouncedEvent::Create(_)) => {
                // Wait for write to complete (debounce)
                tokio::time::sleep(Duration::from_millis(500)).await;
                
                if let Err(e) = reload_tx.send(()).await {
                    log::error!("Failed to trigger reload: {}", e);
                }
            }
            Ok(_) => {}
            Err(e) => log::error!("Watch error: {}", e),
        }
    }
}
```

**Configuration:**
```toml
[mcp]
enabled = true
hot_reload = true           # Enable file watching
reload_debounce_secs = 2    # Debounce window
```

### Layer 3: Granular MCP Lifecycle Management

**Config Diffing Logic:**
```rust
#[derive(Debug, Clone)]
struct McpConfigDiff {
    added: Vec<McpServerConfig>,
    removed: Vec<String>,      // server names
    modified: Vec<McpServerConfig>,
    unchanged: Vec<String>,
}

impl McpManager {
    fn diff_config(&self, new_config: &McpConfig) -> McpConfigDiff {
        let current: HashMap<_, _> = self.servers.iter()
            .map(|(k, v)| (k.clone(), v.config.clone()))
            .collect();
        
        let new: HashMap<_, _> = new_config.servers.iter()
            .map(|s| (s.name.clone(), s.clone()))
            .collect();
        
        McpConfigDiff {
            added: new_config.servers.iter()
                .filter(|s| !current.contains_key(&s.name))
                .cloned()
                .collect(),
            removed: current.keys()
                .filter(|k| !new.contains_key(*k))
                .cloned()
                .collect(),
            modified: new_config.servers.iter()
                .filter(|s| {
                    current.get(&s.name)
                        .map(|old| old != &s.config)
                        .unwrap_or(false)
                })
                .cloned()
                .collect(),
            unchanged: current.keys()
                .filter(|k| new.contains_key(*k) && 
                    current.get(*k) == new.get(*k).map(|s| &s.config))
                .cloned()
                .collect(),
        }
    }
    
    async fn apply_changes(&mut self, changes: McpConfigDiff) -> Result<(), Error> {
        // 1. Health check new/updated servers before making changes
        for server in &changes.added {
            self.health_check(server).await?;
        }
        for server in &changes.modified {
            self.health_check(server).await?;
        }
        
        // 2. Gracefully shutdown removed servers
        for name in &changes.removed {
            if let Some(server) = self.servers.remove(name) {
                server.shutdown().await;
                log::info!("Shutdown MCP server: {}", name);
            }
        }
        
        // 3. Shutdown and restart modified servers
        for server_config in &changes.modified {
            if let Some(server) = self.servers.remove(&server_config.name) {
                server.shutdown().await;
            }
            let new_server = McpServer::start(server_config.clone()).await?;
            self.servers.insert(server_config.name.clone(), new_server);
            log::info!("Restarted MCP server: {}", server_config.name);
        }
        
        // 4. Start new servers
        for server_config in &changes.added {
            let server = McpServer::start(server_config.clone()).await?;
            self.servers.insert(server_config.name.clone(), server);
            log::info!("Started new MCP server: {}", server_config.name);
        }
        
        Ok(())
    }
}
```

### Layer 4: Admin Control Socket (Future)

**Unix domain socket for runtime control:**
```rust
use tokio::net::UnixListener;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum AdminCommand {
    ReloadMcpConfig,
    ListMcpServers,
    RestartMcpServer(String),
    GetMcpStatus(String),
}

async fn admin_socket_handler(
    socket_path: PathBuf,
    mcp_manager: Arc<RwLock<McpManager>>,
) {
    let _ = tokio::fs::remove_file(&socket_path).await;
    let listener = UnixListener::bind(&socket_path).unwrap();
    
    loop {
        if let Ok((mut stream, _)) = listener.accept().await {
            let manager = mcp_manager.clone();
            
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                if let Ok(n) = stream.read(&mut buf).await {
                    if let Ok(cmd) = serde_json::from_slice::<AdminCommand>(&buf[..n]) {
                        let response = match cmd {
                            AdminCommand::ReloadMcpConfig => {
                                // Trigger reload
                                json!({"status": "reloading"})
                            }
                            AdminCommand::ListMcpServers => {
                                let m = manager.read().await;
                                json!({"servers": m.list_servers()})
                            }
                            _ => json!({"error": "not implemented"}),
                        };
                        
                        let _ = stream.write_all(
                            serde_json::to_string(&response).unwrap().as_bytes()
                        ).await;
                    }
                }
            });
        }
    }
}
```

**Usage:**
```bash
zeroclawctl reload-mcps
zeroclawctl list-mcps
zeroclawctl restart-mcp gina-predictions
```

## Implementation Phases

### Phase 1: SIGHUP Support (1-2 days)
- Add signal handler
- Implement config diffing
- Granular MCP lifecycle management
- **Result:** Users can `kill -HUP` instead of full restart

### Phase 2: File Watching (2-3 days)
- Add notify dependency
- Debounced file watcher
- Configurable opt-in
- **Result:** Automatic reload when config changes

### Phase 3: Admin Socket (1 week)
- Unix socket control interface
- CLI tool (zeroclawctl)
- Status/monitoring commands
- **Result:** Full runtime MCP management

### Phase 4: State Persistence (2-3 days)
- Serialize conversation state before reload
- Restore after MCP changes
- **Result:** Zero-downtime MCP updates

## Backwards Compatibility

All changes are additive:
- Default behavior unchanged (no auto-reload)
- Opt-in via config
- SIGHUP only works if handler is registered
- Graceful fallback to restart if reload fails

## Security Considerations

1. **SIGHUP:** Only owner can signal their process
2. **File Watch:** Config file permissions checked
3. **Admin Socket:** Unix socket with proper permissions
4. **Health Checks:** Validate new MCPs before switching

## Migration Path

```bash
# Before
systemctl --user restart zeroclaw

# After (Phase 1)
kill -HUP $(pgrep zeroclaw)

# After (Phase 2)
# Automatic - just edit config.toml

# After (Phase 3)
zeroclawctl reload-mcps
```

## Success Metrics

- [ ] MCP reload completes in < 5 seconds
- [ ] No conversation state lost during reload
- [ ] Failed MCP config doesn't crash daemon
- [ ] 99.9% reload success rate
- [ ] User never needs full restart for MCP changes
