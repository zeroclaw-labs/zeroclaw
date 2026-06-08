# Service & Daemon

This page is the operations-side companion to [Setup → Service management](../setup/service.md); that page covers installing and uninstalling the service. This page covers running it: tuning, resource limits, graceful restarts, and multi-workspace setups.

## Choosing between user and system scope

| Scope | Good for | Downside |
|---|---|---|
| User | Laptop, single-user dev box, simple deployments | Only runs when the user is logged in (Linux with a desktop, macOS) unless you enable lingering |
| System | Headless servers, SBCs, VPSes, multi-user hosts | Needs root to install; gets its own user account |

On desktop Linux, enable user-service lingering so the user service persists across logouts:

<div class="os-tabs-src">

#### sh

```sh
loginctl enable-linger $USER
```

</div>

Without lingering, a user-scope systemd service stops when the last session closes.

## Restart behaviour

The installed systemd user unit (`~/.config/systemd/user/zeroclaw.service`) uses:

```ini
Restart=always
RestartSec=3
```

systemd restarts the daemon on any exit with a 3-second backoff. There is no exit-code allowlist, so a daemon that fails fast on a bad config will flap; fix the config and `systemctl --user restart zeroclaw` rather than relying on the service to give up.

On macOS, the LaunchAgent (`~/Library/LaunchAgents/com.zeroclaw.daemon.plist`) sets `RunAtLoad` and `KeepAlive` to `true`, so launchd keeps the daemon running and relaunches it whenever it exits.

On Windows, `zeroclaw service install` registers a Task Scheduler task triggered `ONLOGON` at the `LIMITED` run level. It starts the daemon at logon; it does not add an automatic restart-on-failure policy.

## Graceful shutdown

On Unix the daemon traps `SIGINT` and `SIGTERM`; on Windows it traps Ctrl+C (`ctrl_c`). Any of these triggers a clean shutdown: the daemon stops its channel server and the gateway listener and exits.

`SIGHUP` is ignored (the daemon stays running). A reload requested via the `/admin/reload` endpoint restarts the daemon loop in place rather than exiting.

Conversation memory and session state are written to SQLite incrementally during operation, not buffered until shutdown, so a clean stop does not depend on a flush step. Tool receipts are in-band HMAC tokens in the conversation, not a separate on-disk log. A hard `SIGKILL` skips the clean channel teardown but does not corrupt already-committed memory; only an agent turn that was mid-write is lost.

## Manual start for debugging

Skip the service and run the daemon directly:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service stop     # free the gateway port if the service is running
zeroclaw daemon
```

</div>

`zeroclaw daemon` runs in the foreground, logs to stderr, and is the same process the service runs, just without the service harness. Useful when:

- Diagnosing startup failures that the service swallows
- Running under `gdb` / `lldb`
- Testing a config change before committing to it

Terminate with Ctrl-C, same graceful shutdown semantics as SIGTERM.

## Resource limits

### Linux: systemd

Add to a drop-in:

<div class="os-tabs-src">

#### sh

```sh
systemctl --user edit zeroclaw.service
```

</div>

```ini
[Service]
MemoryMax=2G
CPUQuota=200%            # two cores
LimitNOFILE=16384        # if opening many channel sockets
```

Reload and restart:

<div class="os-tabs-src">

#### sh

```sh
systemctl --user daemon-reload
systemctl --user restart zeroclaw
```

</div>

### macOS: launchd

Edit `~/Library/LaunchAgents/com.zeroclaw.daemon.plist`:

```xml
<key>SoftResourceLimits</key>
<dict>
  <key>NumberOfFiles</key>
  <integer>16384</integer>
</dict>
```

Unload + load the plist to apply:

<div class="os-tabs-src">

#### sh

```sh
launchctl unload ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
launchctl load ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
```

</div>

### Docker

Compose:

```yaml
services:
  zeroclaw:
    image: ghcr.io/zeroclaw-labs/zeroclaw:latest
    mem_limit: 2g
    cpus: 2.0
    ulimits:
      nofile: 16384
```

## Running multiple workspaces

Each ZeroClaw daemon owns one config directory (which contains its `data/` dir). To run two side by side, give each its own config directory via `--config-dir` (or the `ZEROCLAW_CONFIG_DIR` env var):

<div class="os-tabs-src">

#### sh

```sh
zeroclaw --config-dir ~/.zeroclaw-home daemon
zeroclaw --config-dir ~/.zeroclaw-work daemon
```

</div>

Each instance reads its own `config.toml`, its own `data/` (memory, sessions), its own gateway port (set per config), and its own channel bindings. Memory stays separate; a Telegram bot in one config dir doesn't know about the other.

`zeroclaw service install` always installs a single unit pointed at the default config directory; it has no flag to name or parameterize instances. To run more than one as a persistent service, hand-author a second unit file (copy `~/.config/systemd/user/zeroclaw.service` to a new name) whose `ExecStart` passes `--config-dir <dir>`, then enable it separately.

Don't point two daemons at the same config directory. SQLite is single-writer; the second will fail on startup.

## Observing restarts and crashes

<div class="os-tabs-src">

#### sh

```sh
# Linux
journalctl --user -u zeroclaw --since "1 day ago" | grep -E 'Started|Stopped|failed'

# macOS
log show --predicate 'process == "zeroclaw"' --last 1d | grep -E 'start|stop|error'
```

</div>

If you're seeing repeated restarts, enable debug logging (`RUST_LOG=debug` via the unit file's `Environment=`) and let one more crash happen to capture the full trace.

## See also

- [Setup → Service management](../setup/service.md)
- [Logs & observability](./observability.md)
- [Troubleshooting](./troubleshooting.md)
