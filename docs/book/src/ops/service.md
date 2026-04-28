# Service & Daemon

This page is the operations-side companion to [Setup → Service management](../setup/service.md) — that page covers installing and uninstalling the service. This page covers running it: tuning, resource limits, graceful restarts, and multi-workspace setups.

## Choosing between user and system scope

| Scope | Good for | Downside |
|---|---|---|
| User | Laptop, single-user dev box, simple deployments | Only runs when the user is logged in (Linux with a desktop, macOS) unless you enable lingering |
| System | Headless servers, SBCs, VPSes, multi-user hosts | Needs root to install; gets its own user account |

On desktop Linux, enable user-service lingering so the user service persists across logouts:

```bash
loginctl enable-linger $USER
```

Without lingering, a user-scope systemd service stops when the last session closes.

## Restart behaviour

The stock unit (`~/.config/systemd/user/zeroclaw.service`) uses:

```ini
Restart=on-failure
RestartSec=10s
```

The agent exits cleanly on config errors (`exit 2`) and is not restarted — this prevents a flapping service from chewing CPU while you fix the config. For other exit codes, systemd restarts with a 10-second backoff.

On macOS, the LaunchAgent plist has `KeepAlive = true` with `SuccessfulExit = false`. Same semantics as `on-failure`.

On Windows, the Task Scheduler task is configured with "Restart if task fails" — retry every 10s, up to 10 times.

## Graceful shutdown

The daemon traps `SIGTERM` (Unix) or `CTRL_CLOSE_EVENT` (Windows):

1. Stop accepting new channel events
2. Drain in-flight agent loops (up to `[daemon] shutdown_grace_secs`, default 30)
3. Flush tool receipts and conversation memory to SQLite
4. Disconnect channels and close the gateway listener
5. Exit 0

If the agent is mid-tool-call when shutdown starts, the tool is given the grace period to finish. After that, `SIGKILL` ends it; the receipt is marked interrupted.

Force an immediate exit with `SIGKILL` if you must, but expect the conversation memory for in-flight sessions to be incomplete.

## Manual start for debugging

Skip the service and run the daemon directly:

```bash
zeroclaw service stop     # free the gateway port if the service is running
zeroclaw daemon
```

`zeroclaw daemon` runs in the foreground, logs to stderr, and is the same process the service runs — just without the service harness. Useful when:

- Diagnosing startup failures that the service swallows
- Running under `gdb` / `lldb`
- Testing a config change before committing to it

Terminate with Ctrl-C — same graceful shutdown semantics as SIGTERM.

## Resource limits

### Linux — systemd

Add to a drop-in:

```bash
systemctl --user edit zeroclaw.service
```

```ini
[Service]
MemoryMax=2G
CPUQuota=200%            # two cores
LimitNOFILE=16384        # if opening many channel sockets
```

Reload and restart:

```bash
systemctl --user daemon-reload
systemctl --user restart zeroclaw
```

### macOS — launchd

Edit `~/Library/LaunchAgents/com.zeroclaw.daemon.plist`:

```xml
<key>SoftResourceLimits</key>
<dict>
  <key>NumberOfFiles</key>
  <integer>16384</integer>
</dict>
```

Unload + load the plist to apply:

```bash
launchctl unload ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
launchctl load ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
```

### Docker

Compose:

```yaml
services:
  zeroclaw:
    image: zeroclawlabs/zeroclaw:latest
    mem_limit: 2g
    cpus: 2.0
    ulimits:
      nofile: 16384
```

## Running multiple workspaces

Each ZeroClaw instance owns one workspace. To run two:

1. Install the binary once
2. Create `~/.zeroclaw-home/` and `~/.zeroclaw-work/` (or wherever)
3. Run two services pointing at different workspaces:

```bash
ZEROCLAW_WORKSPACE=~/.zeroclaw-home zeroclaw service install --name zeroclaw-home
ZEROCLAW_WORKSPACE=~/.zeroclaw-work zeroclaw service install --name zeroclaw-work
```

Each gets its own unit file / plist, its own gateway port (configurable in each config), and its own channel bindings. Memory stays separate; a Telegram bot in one workspace doesn't know about the other.

Don't point two daemons at the same workspace. SQLite is single-writer; the second will fail on startup.

## Observing restarts and crashes

```bash
# Linux
journalctl --user -u zeroclaw --since "1 day ago" | grep -E 'Started|Stopped|failed'

# macOS
log show --predicate 'process == "zeroclaw"' --last 1d | grep -E 'start|stop|error'
```

If you're seeing repeated restarts, enable debug logging (`RUST_LOG=zeroclaw=debug` via the unit file's `Environment=`) and let one more crash happen to capture the full trace.

## See also

- [Setup → Service management](../setup/service.md)
- [Logs & observability](./observability.md)
- [Troubleshooting](./troubleshooting.md)
