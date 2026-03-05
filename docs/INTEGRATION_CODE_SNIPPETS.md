# Integration Code Snippets

This file shows exactly what code to add to existing ZeroClaw files.

## 1. Add to `src/main.rs`

### At the top (with other imports):
```rust
use std::sync::Arc;
use tokio::sync::RwLock;
```

### In your main/startup function:

Find where `McpManager` is created and modify:

```rust
// OLD CODE:
// let mcp_manager = McpManager::new(config.mcp_servers).await?;

// NEW CODE:
let mcp_manager = Arc::new(RwLock::new(McpManager::new(config.mcp_servers).await?));

// Initialize signal handler for MCP hot reload
let shutdown_tx_clone = shutdown_tx.clone();
let mcp_manager_clone = mcp_manager.clone();
tokio::spawn(async move {
    if let Err(e) = crate::mcp_reload::init_signal_handler(
        mcp_manager_clone,
        shutdown_tx_clone,
    ).await {
        log::error!("Failed to initialize MCP signal handler: {}", e);
    }
});
```

### Update your CLI argument parsing:

```rust
// Add to your CLI enum/struct
#[derive(Parser)]
#[command(name = "zeroclaw")]
#[command(about = "ZeroClaw AI daemon")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon
    Start {
        #[arg(long, default_value = "~/.zeroclaw/config.toml")]
        config: PathBuf,
    },
    /// Stop the daemon
    Stop,
    /// Reload MCP configurations
    Reload,
    /// Check daemon status
    Status,
}

// In main(), handle the reload command:
match cli.command {
    Some(Commands::Reload) => {
        // Find PID and send SIGHUP
        match find_zeroclaw_pid() {
            Some(pid) => {
                println!("Sending SIGHUP to ZeroClaw (PID: {})...", pid);
                match signal::kill(Pid::from_raw(pid), Signal::SIGHUP) {
                    Ok(()) => {
                        println!("✅ Reload signal sent successfully");
                        println!("Check logs with: journalctl --user -u zeroclaw -f");
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to send signal: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            None => {
                eprintln!("❌ ZeroClaw daemon is not running");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    // ... handle other commands
}
```

### Add helper function to find PID:

```rust
use nix::unistd::Pid;
use nix::sys::signal::{self, Signal};

fn find_zeroclaw_pid() -> Option<i32> {
    // Method 1: Check PID file
    if let Ok(pid_str) = std::fs::read_to_string(
        dirs::runtime_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("zeroclaw.pid")
    ) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Verify process exists
            if PathBuf::from(format!("/proc/{}", pid)).exists() {
                return Some(pid);
            }
        }
    }
    
    // Method 2: Use pgrep
    use std::process::Command;
    if let Ok(output) = Command::new("pgrep")
        .args(&["-f", "zeroclaw"])
        .output() {
        if output.status.success() {
            if let Ok(pid_str) = String::from_utf8(output.stdout) {
                if let Ok(pid) = pid_str.trim().parse::<i32>() {
                    return Some(pid);
                }
            }
        }
    }
    
    None
}
```

## 2. Add to `Cargo.toml`

### Under `[dependencies]`:

```toml
[dependencies]
# ... existing dependencies ...

# Signal handling for MCP hot reload
signal-hook = "0.3"
signal-hook-tokio = { version = "0.3", features = ["futures-v0_3"] }

# For kill/ signal operations (nix crate may already be present)
nix = { version = "0.27", features = ["signal", "process"] }

# For hashing configs
sha2 = "0.10"

# For diffing (optional, can implement simple diff manually)
similar = "2.4"
```

### Or minimal addition (if some already exist):

```toml
[dependencies]
# Add only what's not already present
signal-hook = "0.3"
sha2 = "0.10"
```

## 3. Create `src/mcp_reload.rs`

Copy the entire contents of `SIGHUP_IMPLEMENTATION.rs` to this file.

## 4. Modify `src/mcp/mod.rs` (or wherever McpManager is defined)

### Add to your McpManager implementation:

```rust
impl McpManager {
    // ... existing methods ...
    
    /// Gracefully stop a specific MCP
    pub async fn stop_mcp(&mut self, name: &str) -> Result<(), String> {
        if let Some(mcp) = self.mcps.remove(name) {
            log::info!("Stopping MCP: {}", name);
            mcp.shutdown().await.map_err(|e| format!("Failed to stop {}: {}", name, e))?;
            log::info!("MCP {} stopped successfully", name);
        }
        Ok(())
    }
    
    /// Start a new MCP
    pub async fn start_mcp(&mut self, name: &str, config: McpConfig) -> Result<(), String> {
        log::info!("Starting MCP: {}", name);
        let mcp = McpConnection::new(config).await
            .map_err(|e| format!("Failed to start {}: {}", name, e))?;
        self.mcps.insert(name.to_string(), mcp);
        log::info!("MCP {} started successfully", name);
        Ok(())
    }
    
    /// Restart an MCP
    pub async fn restart_mcp(&mut self, name: &str, config: McpConfig) -> Result<(), String> {
        self.stop_mcp(name).await?;
        self.start_mcp(name, config).await
    }
}
```

## 5. Update `systemd/zeroclaw.service`

### Add ExecReload:

```ini
[Unit]
Description=ZeroClaw AI Assistant Daemon
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/zeroclaw start
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
```

### Key addition:
```ini
ExecReload=/bin/kill -HUP $MAINPID
```

## 6. Create PID file on startup

### In your daemon startup code:

```rust
use std::fs;

fn create_pid_file() -> std::io::Result<()> {
    let pid = std::process::id();
    let pid_path = dirs::runtime_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("zeroclaw.pid");
    
    fs::write(&pid_path, pid.to_string())?;
    log::info!("Created PID file at {:?}", pid_path);
    
    // Clean up on exit
    let pid_path_clone = pid_path.clone();
    tokio::spawn(async move {
        // Wait for shutdown signal
        // ... 
        // Cleanup
        let _ = fs::remove_file(&pid_path_clone);
    });
    
    Ok(())
}
```

## 7. Add module declaration

### In `src/lib.rs` or `src/main.rs`:

```rust
pub mod mcp_reload;
```

## Complete Integration Checklist

- [ ] Copy `SIGHUP_IMPLEMENTATION.rs` → `src/mcp_reload.rs`
- [ ] Add `pub mod mcp_reload;` to `src/lib.rs`
- [ ] Add signal handler init to `main()`
- [ ] Add `signal-hook` to `Cargo.toml`
- [ ] Add `sha2` to `Cargo.toml` (for hashing)
- [ ] Add `Reload` command to CLI
- [ ] Add PID file creation
- [ ] Add helper methods to `McpManager`
- [ ] Update systemd service file
- [ ] Test: `cargo build --release`
- [ ] Test: Start daemon
- [ ] Test: Modify config
- [ ] Test: `systemctl --user reload zeroclaw`

## Troubleshooting

### "signal-hook not found"
Add to Cargo.toml and run `cargo update`

### "no method named stop_mcp found"
Ensure you're using the updated McpManager with the new methods

### "cannot find function find_zeroclaw_pid"
Add the helper function from section 1

### "module mcp_reload not found"
Add `pub mod mcp_reload;` to your lib.rs or main.rs
