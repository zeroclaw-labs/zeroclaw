# MCP Hot Reload RFC

## Overview

This document describes the design and implementation of zero-downtime MCP configuration reloading in ZeroClaw.

## Problem Statement

Currently, any change to MCP configuration requires a full daemon restart:

```bash
systemctl --user restart zeroclaw
```

This causes:
- Loss of active conversations
- Disruption of LLM API connections
- 10-30 second downtime
- Poor user experience

## Proposed Solution

Implement SIGHUP signal handling for graceful MCP reloading without daemon restart.

## Design

### Architecture

```
┌─────────────────┐
│   SIGHUP Signal │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Signal Handler  │
│   (Thread)      │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Config Loader  │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Diff Engine   │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ MCP Controller  │
│  (Start/Stop/   │
│   Restart)      │
└─────────────────┘
```

### Signal Handling

Uses the `signal-hook` crate for cross-platform signal handling:

```rust
let mut signals = Signals::new(&[SIGHUP])?;
for sig in signals.forever() {
    match sig {
        SIGHUP => reload_mcps(),
        _ => {}
    }
}
```

### Configuration Diff Algorithm

1. **Hash Comparison**: Compare SHA-256 hash of old vs new config
2. **If changed**, compute detailed diff:
   - **Added**: MCPs in new config but not in old → Start
   - **Removed**: MCPs in old but not in new → Stop
   - **Modified**: MCPs with different settings → Restart
   - **Unchanged**: Same config → Keep running

### State Management

- Uses `Arc<Mutex<HashMap>>` for thread-safe MCP state
- Each MCP tracked with: name, config, process_id
- Hash stored to detect changes

## Security Considerations

- Signal handler runs in isolated thread
- Lock poisoning handled gracefully
- Failed MCP starts don't crash daemon

## Performance

- Config diff: O(n log n) where n = number of MCPs
- Typical reload time: <100ms for 10 MCPs
- Zero downtime for unchanged MCPs

## Future Enhancements

- [ ] File watcher for automatic reload
- [ ] Health checks before/after reload
- [ ] Rollback on failure
- [ ] Dry-run mode

## References

- signal-hook crate: https://docs.rs/signal-hook/
- systemd.service(5): ExecReload directive
