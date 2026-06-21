# Service Management

ZeroClaw ships with first-class service integration for systemd (Linux), launchctl (macOS), and Task Scheduler (Windows). All three are driven by one CLI surface:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service install     # register the service
zeroclaw service start       # start it
zeroclaw service stop        # stop it
zeroclaw service restart     # stop + start
zeroclaw service status      # running / stopped, last exit code
zeroclaw service uninstall   # remove it
```

</div>

The platform-specific backends are implemented in `crates/zeroclaw-runtime/src/service/`. You don't have to think about them, but knowing what they produce helps when debugging.

## Linux: systemd

`zeroclaw service install` writes a user-scoped unit at `~/.config/systemd/user/zeroclaw.service`.

The unit:

- `Type=simple` with the agent process staying in the foreground
- `ExecStart={cargo-bin}/zeroclaw daemon`
- `Restart=always` with `RestartSec=3`
- `Environment=HOME=%h` and `PassEnvironment=DISPLAY XDG_RUNTIME_DIR` so headless browser tools can create profile/cache dirs and reach the user session
- `WantedBy=default.target`

### Manual control (systemd)

<div class="os-tabs-src">

#### sh

```sh
systemctl --user start zeroclaw
systemctl --user stop zeroclaw
systemctl --user status zeroclaw
systemctl --user enable zeroclaw     # start on login
```

</div>

### Logs

<div class="os-tabs-src">

#### sh

```sh
journalctl --user -u zeroclaw -f        # follow
journalctl --user -u zeroclaw --since "1h ago"
```

</div>

### Starting before user login

The CLI only ever writes a user-scoped unit (`systemctl --user`), which by default starts at login and stops at logout. To keep ZeroClaw running on a headless box without an active session, enable lingering for the service user:

<div class="os-tabs-src">

#### sh

```sh
sudo loginctl enable-linger $USER
systemctl --user enable --now zeroclaw
```

</div>

If you need a true system-scope unit (root-owned, `/etc/systemd/system/`, dedicated service account, or hardware groups via `SupplementaryGroups`), the CLI does not generate one; adapt the system-level template at [`scripts/zeroclaw.service`](https://github.com/zeroclaw-labs/zeroclaw/blob/master/scripts/zeroclaw.service) and install it yourself. On OpenRC hosts, `sudo zeroclaw service install` does provision a dedicated `zeroclaw` user and system paths (see below).

## Linux: OpenRC

Detected automatically when `/run/openrc` exists (Alpine, some Gentoo configs).

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service install   # writes /etc/init.d/zeroclaw
rc-service zeroclaw start
rc-update add zeroclaw default    # start on boot
```

</div>

## macOS: LaunchAgent

`zeroclaw service install` writes `~/Library/LaunchAgents/com.zeroclaw.daemon.plist` and loads it.

<div class="os-tabs-src">

#### sh

```sh
launchctl list | grep zeroclaw
launchctl unload ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
launchctl load ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
```

</div>

Logs go to `<config-dir>/logs/` as `daemon.stdout.log` and `daemon.stderr.log` (for a default install, `~/.zeroclaw/logs/`). Homebrew installs write to `$HOMEBREW_PREFIX/var/zeroclaw/logs/` instead.

### Homebrew-managed

If installed via Homebrew, `brew services` is the preferred interface:

<div class="os-tabs-src">

#### sh

```sh
brew services start zeroclaw
brew services restart zeroclaw
brew services info zeroclaw
```

</div>

Don't mix `zeroclaw service` CLI commands with `brew services`, pick one. Both end up writing a plist; having both around confuses `launchctl`.

## Windows: Task Scheduler

`zeroclaw service install` creates a per-user scheduled task named **ZeroClaw Daemon**:

- Trigger: at logon (`/SC ONLOGON`)
- Run level: `LIMITED` (runs as the current user, not elevated)
- Action: runs the install wrapper `zeroclaw-daemon.cmd`, which launches `zeroclaw daemon`

Verify in Task Scheduler GUI (`taskschd.msc`) under Task Scheduler Library → ZeroClaw Daemon.

Logs go to `<config-dir>\logs\` as `daemon.stdout.log` and `daemon.stderr.log` (for a default install, `%USERPROFILE%\.zeroclaw\logs\`):

<div class="os-tabs-src">

#### cmd

```cmd
type %USERPROFILE%\.zeroclaw\logs\daemon.stdout.log
```

</div>

### Manual control (Task Scheduler)

The task is driven through `zeroclaw service start|stop|status`, which wrap `schtasks /Run`, `/End`, and `/Query` against the **ZeroClaw Daemon** task. You can also manage it directly:

<div class="os-tabs-src">

#### cmd

```cmd
schtasks /Run /TN "ZeroClaw Daemon"
schtasks /End /TN "ZeroClaw Daemon"
schtasks /Query /TN "ZeroClaw Daemon" /FO LIST
```

</div>

The CLI installs only a per-user ONLOGON task; it does not register a `LocalSystem` Windows Service. For a true system service, wrap the binary with a third-party supervisor (e.g. NSSM) yourself.

## Config path resolution

The service reads config from whichever directory resolved at install time. Precedence (first match wins):

1. `$ZEROCLAW_CONFIG_DIR` (config lives directly under `$ZEROCLAW_CONFIG_DIR`)
2. `$ZEROCLAW_DATA_DIR`
3. `$ZEROCLAW_WORKSPACE` (**deprecated**, prefer `ZEROCLAW_DATA_DIR`; resolves either `$ZEROCLAW_WORKSPACE` or the legacy sibling `.zeroclaw/`)
4. On macOS only, the Homebrew config dir (`$HOMEBREW_PREFIX/var/zeroclaw/`) when installed via Homebrew
5. Default `~/.zeroclaw/` (Linux/macOS) or `%USERPROFILE%\.zeroclaw\` (Windows)

`ZEROCLAW_CONFIG_DIR` overrides everything; setting it alongside `ZEROCLAW_DATA_DIR` or `ZEROCLAW_WORKSPACE` logs a warning and ignores the others.

If your service seems to ignore config changes, check which path the daemon resolved against, `zeroclaw status` reports the active config file, and the runtime logs a resolution-source line at startup:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw status
```

</div>

The output includes the config file path it resolved against.

## Auto-update

The service does **not** auto-update. That's deliberate; you pick when to take new code. Subscribe to the GitHub release feed or the Discord `#releases` channel (see [Contributing → Communication](../contributing/communication.md)).

## See also

- [Linux setup](./linux.md), [macOS setup](./macos.md), [Windows setup](./windows.md)
- [Operations → Logs & observability](../ops/observability.md)
- [Operations → Troubleshooting](../ops/troubleshooting.md)
