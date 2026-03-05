# How to Submit the ZeroClaw MCP Hot Reload PR

## Step 1: Fork & Clone ZeroClaw

```bash
# Go to https://github.com/original-owner/zeroclaw
# Click "Fork" button (top right)
# Clone YOUR fork
git clone https://github.com/YOUR_USERNAME/zeroclaw.git
cd zeroclaw
```

## Step 2: Create a Branch

```bash
git checkout -b feature/mcp-hot-reload
```

## Step 3: Add the Implementation Files

Copy these files from `ZEROCALW_PR_SUBMISSION/` to the repo:

```bash
# Create the new module
cp SIGHUP_IMPLEMENTATION.rs src/mcp_reload.rs

# Update main.rs (add the signal handler init)
# See INTEGRATION_CODE_SNIPPETS.md for exact changes

# Update Cargo.toml (add signal-hook dependency if needed)
# See DEPENDENCIES.md
```

## Step 4: Commit & Push

```bash
git add src/mcp_reload.rs
git add src/main.rs  # if modified
git add Cargo.toml   # if modified
git commit -m "feat: Add MCP hot reload with SIGHUP support

- Add signal handler for SIGHUP to trigger MCP reload
- Implement intelligent change detection (hash-based)
- Add zeroclaw reload CLI command
- Support granular MCP lifecycle (stop/start/restart)
- Add comprehensive error handling and rollback
- Update systemd service with ExecReload

This allows reloading MCP configurations without restarting
the entire daemon, preserving conversation state."

git push origin feature/mcp-hot-reload
```

## Step 5: Create the PR

1. Go to https://github.com/YOUR_USERNAME/zeroclaw
2. Click "Compare & pull request"
3. **Title:** `feat: MCP hot reload with SIGHUP support`
4. **Description:** Copy/paste from `PR_DESCRIPTION.md`
5. Click "Create pull request"

## What to Include in the PR

### Required Files

Upload these as part of the PR:

| File | Location | Notes |
|------|----------|-------|
| `mcp_reload.rs` | `src/mcp_reload.rs` | New module |
| `main.rs` changes | `src/main.rs` | See INTEGRATION_CODE_SNIPPETS.md |
| `Cargo.toml` | `Cargo.toml` | Add `signal-hook = "0.3"` |
| `zeroclaw.service` | `systemd/zeroclaw.service` | Updated service file |

### Documentation Files (Optional but Recommended)

| File | Purpose |
|------|---------|
| `MCP_HOT_RELOAD_RFC.md` | Architecture explanation |
| `MCP_HOT_RELOAD_SOLUTIONS.md` | User guide |
| `TODO_WISHLIST.md` | Future roadmap |

## Quick Copy-Paste

### PR Title
```
feat: MCP hot reload with SIGHUP support
```

### PR Body
```markdown
## Summary
Adds hot reload capability for MCP configurations using SIGHUP signals.
Users can now reload MCPs without restarting the daemon.

## Changes
- Signal handler for SIGHUP (`signal-hook`)
- Intelligent config change detection
- New `zeroclaw reload` CLI command
- Updated systemd service with ExecReload

## Usage
```bash
systemctl --user reload zeroclaw  # Graceful reload
# or
kill -HUP $(pgrep zeroclaw)       # Direct signal
```

## Testing
- [x] Tested on Linux with systemd
- [x] Backward compatible
- [x] Error handling implemented

See full description in PR_DESCRIPTION.md
```

## After Submission

1. **Watch for maintainer feedback** - They may suggest changes
2. **Update files** if requested:
   ```bash
   # Make changes
   git add .
   git commit -m "address review feedback"
   git push origin feature/mcp-hot-reload
   ```
3. **Celebrate** when merged! 🎉

## Need Help?

- GitHub Docs: https://docs.github.com/en/pull-requests
- First PR Guide: https://opensource.guide/how-to-contribute/
