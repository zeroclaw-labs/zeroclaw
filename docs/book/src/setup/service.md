# Service Management

ZeroClaw ships with first-class service integration for systemd (Linux), launchctl (macOS), and Task Scheduler (Windows). All three are driven by one CLI surface:

```bash
zeroclaw service install     # register the service
zeroclaw service start       # start it
zeroclaw service stop        # stop it
zeroclaw service restart     # stop + start
zeroclaw service status      # running / stopped, last exit code
zeroclaw service uninstall   # remove it
```

The platform-specific backends are implemented in `crates/zeroclaw-runtime/src/service/`. You don't have to think about them — but knowing what they produce helps when debugging.

## Linux — systemd

`zeroclaw service install` writes a user-scoped unit at `~/.config/systemd/user/zeroclaw.service`.

The unit:

- `Type=simple` with the agent process staying in the foreground
- `User=` set to the invoking user
- `SupplementaryGroups=gpio spi i2c` (enabled if hardware feature is compiled in)
- `Restart=on-failure` with a 10-second backoff
- `ExecStart=/home/$USER/.cargo/bin/zeroclaw daemon`

### Manual control

```bash
systemctl --user start zeroclaw
systemctl --user stop zeroclaw
systemctl --user status zeroclaw
systemctl --user enable zeroclaw     # start on login
```

### Logs

```bash
journalctl --user -u zeroclaw -f        # follow
journalctl --user -u zeroclaw --since "1h ago"
```

### System-scope (root) service

If you need ZeroClaw to start before user login (headless SBCs, VPSes), run the install command as root:

```bash
sudo zeroclaw service install
sudo systemctl enable --now zeroclaw
```

When invoked with sudo/root, `zeroclaw service install` creates a system-scope unit at `/etc/systemd/system/zeroclaw.service` and provisions a dedicated `zeroclaw` service user.

## Linux — OpenRC

Detected automatically when `/run/openrc` exists (Alpine, some Gentoo configs).

```bash
zeroclaw service install   # writes /etc/init.d/zeroclaw
rc-service zeroclaw start
rc-update add zeroclaw default    # start on boot
```

## macOS — LaunchAgent

`zeroclaw service install` writes `~/Library/LaunchAgents/com.zeroclaw.daemon.plist` and loads it.

```bash
launchctl list | grep zeroclaw
launchctl unload ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
launchctl load ~/Library/LaunchAgents/com.zeroclaw.daemon.plist
```

Logs go to `~/Library/Logs/ZeroClaw/zeroclaw.log` (stdout) and `zeroclaw.err` (stderr).

### Homebrew-managed

If installed via Homebrew, `brew services` is the preferred interface:

```bash
brew services start zeroclaw
brew services restart zeroclaw
brew services info zeroclaw
```

Don't mix `zeroclaw service` CLI commands with `brew services` — pick one. Both end up writing a plist; having both around confuses `launchctl`.

## Windows — Task Scheduler

`zeroclaw service install` creates a scheduled task in the current user's session:

- Trigger: at logon
- Condition: battery, idle, and power-save conditions are **all disabled** (otherwise the task would stop unexpectedly)
- Action: run `zeroclaw daemon` hidden

Verify in Task Scheduler GUI (`taskschd.msc`) under Task Scheduler Library → ZeroClaw.

Logs go to `%USERPROFILE%\.zeroclaw\logs\`:

```cmd
type %USERPROFILE%\.zeroclaw\logs\zeroclaw.log
```

The wrapper script that the scheduled task runs is at `%USERPROFILE%\.zeroclaw\logs\zeroclaw-daemon.cmd`.

> **No Windows Service / LocalSystem path.** The current release does not register a Windows Service via `sc.exe` even when `zeroclaw service install` is run from an elevated prompt — the underlying code path in `crates/zeroclaw-runtime/src/service/mod.rs` always installs a user-scoped scheduled task on Windows. Running elevated has no effect on which path is used. Native Windows Service / LocalSystem support is on the roadmap but not yet implemented; for headless server installs, use Task Scheduler → ZeroClaw Daemon → Properties → "Run whether user is logged on or not" instead.

## Config path resolution

The service reads config from whichever workspace it was installed against. Order:

1. `$ZEROCLAW_CONFIG_DIR/config.toml` if set
2. `$ZEROCLAW_WORKSPACE/.zeroclaw/config.toml` if set
3. `$HOMEBREW_PREFIX/var/zeroclaw/.zeroclaw/config.toml` if installed via Homebrew
4. `~/.zeroclaw/config.toml` (Linux/macOS) or `%USERPROFILE%\.zeroclaw\config.toml` (Windows)

If your service seems to ignore config changes, check which path the daemon is reading:

```bash
zeroclaw config list
```

The first few lines of its output show the config file path it resolved against.

## Auto-update

The service does **not** auto-update. That's deliberate — you pick when to take new code. Subscribe to the GitHub release feed or the Discord `#releases` channel (see [Contributing → Communication](../contributing/communication.md)).

## See also

- [Linux setup](./linux.md), [macOS setup](./macos.md), [Windows setup](./windows.md)
- [Operations → Logs & observability](../ops/observability.md)
- [Operations → Troubleshooting](../ops/troubleshooting.md)
