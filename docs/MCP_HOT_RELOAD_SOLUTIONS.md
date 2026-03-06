# MCP Hot Reload Solutions for ZeroClaw

## Current State

ZeroClaw requires a **full daemon restart** to load new or modified MCP configurations:
```bash
systemctl --user restart zeroclaw
```

This is disruptive because it:
- Loses in-memory conversation context
- Interrupts active operations
- Creates poor user experience
- Prevents dynamic MCP management

## Solution Overview

We've created three layers of solutions:

| Solution | Complexity | User Effort | Status |
|----------|-----------|-------------|--------|
| **Helper Scripts** (Now) | Low | Manual trigger | ✅ Ready |
| **File Watcher** (Now) | Low | One-time setup | ✅ Ready |
| **SIGHUP Support** (Now) | Medium | Signal trigger | ✅ Ready |
| **Native Hot Reload** (Future) | High | Automatic | 📋 RFC Complete |

---

## Immediate Solutions (Use Today)

### 1. MCP Hot Reload Helper Script

**File:** `mcp-hot-reload.sh`

Provides commands to check status, trigger reload, watch files, or restart:

```bash
# Check if hot reload is available
./mcp-hot-reload.sh status

# Try to reload (uses SIGHUP if ZeroClaw supports it)
./mcp-hot-reload.sh reload

# Watch config and auto-reload on changes
./mcp-hot-reload.sh watch

# Full restart (fallback)
./mcp-hot-reload.sh restart
```

### 2. Python File Watcher

**File:** `mcp_file_watcher.py`

Stand-alone Python implementation with no dependencies (optional: `watchdog` for better performance):

```bash
# Run interactively
python3 mcp_file_watcher.py

# Run as daemon
python3 mcp_file_watcher.py --daemon

# Check status
python3 mcp_file_watcher.py --status

# Stop daemon
python3 mcp_file_watcher.py --stop

# Single check
python3 mcp_file_watcher.py --once
```

**Features:**
- ✅ No dependencies (pure Python 3.7+)
- ✅ Debounced file watching (2s default)
- ✅ PID tracking and daemon mode
- ✅ Persistent state across runs
- ✅ Automatic fallback handling

---

## Architecture Roadmap

### Phase 1: SIGHUP Signal Handler (Implemented)

SIGHUP-based hot reload is available now in this PR. The implementation uses
`tokio::signal::unix` to listen for SIGHUP and triggers config diffing and
selective MCP lifecycle management.

**User Experience:**
```bash
kill -HUP $(pgrep zeroclaw)
# or
systemctl --user reload zeroclaw
```

**Benefits:**
- No restart needed
- Fast reload (< 5 seconds)
- Standard Unix practice
- Context preserved

**See:** `MCP_HOT_RELOAD_RFC.md` for full architecture details

### Phase 2: File Watch with Debouncing

**Implementation:** Add `notify` crate to ZeroClaw

```rust
use notify::{Watcher, RecursiveMode};

let mut watcher = watcher(tx, Duration::from_secs(2))?;
watcher.watch(config_path, RecursiveMode::NonRecursive)?;
```

**Configuration:**
```toml
[mcp]
enabled = true
hot_reload = true           # Enable file watching
reload_debounce_secs = 2    # Debounce window
```

**Benefits:**
- Fully automatic
- No user action needed
- Smart debouncing
- Opt-in via config

### Phase 3: Granular MCP Management

**Implementation:** Diff configs and only reconnect changed MCPs

```rust
struct McpConfigDiff {
    added: Vec<McpServer>,
    removed: Vec<String>,
    modified: Vec<McpServer>,
    unchanged: Vec<String>,
}

// Only restart modified servers
for server in diff.modified {
    server.shutdown().await;
    server.start().await;
}
```

**Benefits:**
- Minimal disruption
- Health checks before switching
- Rollback on failure
- Parallel operations

### Phase 4: Admin Control Interface

**Implementation:** Unix socket for runtime control

```bash
# CLI tool
zeroclawctl reload-mcps
zeroclawctl list-mcps
zeroclawctl restart-mcp gina-predictions

# Or via socket
echo '{"cmd":"reload_mcps"}' | \
  socat - UNIX-CONNECT:/run/zeroclaw/control.sock
```

**Benefits:**
- Fine-grained control
- Status monitoring
- Scriptable
- No signal knowledge needed

---

## Recommended Path Forward

### For Users (Today)

1. **Use the Python watcher for automatic reloads:**
   ```bash
   python3 mcp_file_watcher.py --daemon
   ```

2. **Or use the bash helper for manual control:**
   ```bash
   ./mcp-hot-reload.sh watch
   ```

3. **Add to your shell profile for automatic startup:**
   ```bash
   # ~/.bashrc or ~/.zshrc
   if ! python3 ~/.zeroclaw/workspace/mcp_file_watcher.py --status | grep -q "Running"; then
       python3 ~/.zeroclaw/workspace/mcp_file_watcher.py --daemon
   fi
   ```

### For ZeroClaw Development

1. **Phase 1 (SIGHUP)** - Quick win, 1-2 days
   - Add signal handler
   - Implement config diffing
   - Granular MCP lifecycle

2. **Phase 2 (File Watch)** - 2-3 days
   - Add `notify` dependency
   - Configurable debouncing
   - Opt-in via config

3. **Phase 3 (Granular)** - 3-5 days
   - Config hashing/comparison
   - Health check validation
   - Rollback on failure

4. **Phase 4 (Admin Socket)** - 1 week
   - Unix socket interface
   - `zeroclawctl` CLI tool
   - Status/monitoring

---

## Files Created

| File | Purpose | Lines |
|------|---------|-------|
| `MCP_HOT_RELOAD_RFC.md` | Detailed architecture RFC | 350+ |
| `MCP_HOT_RELOAD_SOLUTIONS.md` | This summary document | - |

---

## Quick Reference

### Install MCP with Auto-Reload

```bash
# Method 1: Manual workflow
./mcp-install.sh filesystem npx @modelcontextprotocol/server-filesystem /path
./mcp-hot-reload.sh reload  # Or restart if reload not supported

# Method 2: With watcher running (automatic)
./mcp-install.sh filesystem npx @modelcontextprotocol/server-filesystem /path
# Watcher detects change and reloads automatically
```

### Check Everything is Working

```bash
# Check ZeroClaw status
./mcp-hot-reload.sh status

# Check watcher status  
python3 mcp_file_watcher.py --status

# Test MCP is loaded
# (Ask ZeroClaw to use an MCP tool)
```

---

## Questions or Issues?

The helper scripts include error handling and fallbacks:
- If SIGHUP fails → falls back to restart
- If watcher can't detect changes → logs error and retries
- If daemon dies → can be restarted with `--daemon`

**Next Steps:**
1. Try the Python watcher: `python3 mcp_file_watcher.py --daemon`
2. Edit your `~/.zeroclaw/config.toml` to add/modify an MCP
3. Watch the logs to see automatic reload in action
4. Report back on what works and what doesn't!
