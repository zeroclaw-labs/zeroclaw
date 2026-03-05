# MCP Hot Reload User Guide

## Overview

ZeroClaw now supports **zero-downtime MCP reloading**. You can add, remove, or modify MCP servers without restarting the daemon.

## Quick Start

### Reload MCPs

```bash
# Method 1: systemd (recommended)
systemctl --user reload zeroclaw

# Method 2: Direct signal
kill -HUP $(cat ~/.zeroclaw/daemon.pid)

# Method 3: CLI (if implemented)
zeroclaw reload mcp
```

### What Happens

When you reload:
1. ZeroClaw re-reads `~/.zeroclaw/config.toml`
2. Compares old vs new MCP configuration
3. Applies changes:
   - **New MCPs**: Started automatically
   - **Removed MCPs**: Stopped gracefully
   - **Changed MCPs**: Restarted with new settings
   - **Unchanged**: Continue running (no interruption!)

## Configuration

### Enable Hot Reload

Edit `~/.zeroclaw/config.toml`:

```toml
[mcp]
# Enable file watcher for automatic reload (optional)
auto_reload = true

[[mcp.servers]]
name = "fetch"
command = "uvx"
args = ["mcp-server-fetch"]

[[mcp.servers]]
name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/docs"]
```

### Adding a New MCP

1. Edit `~/.zeroclaw/config.toml`:
```toml
[[mcp.servers]]
name = "my-new-mcp"
command = "python"
args = ["/path/to/server.py"]
```

2. Reload:
```bash
systemctl --user reload zeroclaw
```

3. Verify:
```bash
zeroclaw mcp list
```

### Removing an MCP

1. Remove the `[[mcp.servers]]` section from config
2. Reload: `systemctl --user reload zeroclaw`
3. The MCP will be stopped automatically

### Modifying an MCP

1. Edit the MCP's settings in config
2. Reload: `systemctl --user reload zeroclaw`
3. The MCP will be restarted with new settings

## Troubleshooting

### Reload Not Working

Check logs:
```bash
journalctl --user -u zeroclaw -f
```

Common issues:
- Config file syntax error
- MCP command not found
- Port conflicts

### View Current MCPs

```bash
zeroclaw mcp list
# or
cat ~/.zeroclaw/daemon.pid | xargs kill -HUP
```

### Manual Reload

If systemd isn't available:
```bash
# Find ZeroClaw PID
pgrep -f zeroclaw

# Send SIGHUP
kill -HUP <pid>
```

## Best Practices

1. **Test changes**: Use `zeroclaw config validate` before reloading
2. **Check logs**: Always verify successful reload in logs
3. **Backup config**: Keep a backup of working config
4. **Reload vs Restart**:
   - Use `reload` for config changes
   - Use `restart` only for ZeroClaw updates

## systemd Integration

The systemd service file now includes:

```ini
[Service]
ExecReload=/bin/kill -HUP $MAINPID
```

This enables `systemctl reload zeroclaw` to work correctly.

## Limitations

- MCP processes are terminated gracefully (SIGTERM)
- No rollback if new config breaks something
- Reload affects all MCPs simultaneously

## See Also

- `MCP_HOT_RELOAD_RFC.md` - Technical architecture
- `zeroclaw-labs/zeroclaw` - Main repository
