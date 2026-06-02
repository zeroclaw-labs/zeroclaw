# zerocode

zerocode is ZeroClaw's terminal interface for managing configuration,
chatting with agents, and monitoring your daemon. It connects over a local
IPC stream — a Unix domain socket on Unix, a named pipe on Windows — or
over WebSocket Secure (WSS) for remote use.

## Local setup

On the same machine as the daemon, no extra configuration is needed:

```bash
zerocode
```

zerocode finds the daemon's local endpoint automatically — `<data_dir>/data/daemon.sock`
on Unix, `\\.\pipe\zeroclaw-<hash>` on Windows. If the daemon isn't running,
zerocode spawns an ephemeral one.

## Remote setup (WSS)

Connect zerocode on your workstation to a daemon running on another machine
(Raspberry Pi, home server, VPS, etc.).

### On the remote host (daemon side)

1. **Generate a self-signed TLS certificate:**

   ```bash
   openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 \
     -keyout ~/.zeroclaw/wss.key \
     -out ~/.zeroclaw/wss.cert \
     -days 3650 -nodes -subj '/CN=zeroclaw'
   ```

2. **Enable WSS in `~/.zeroclaw/config.toml`:**

   ```toml
   [wss]
   enabled = true
   cert_path = "/home/youruser/.zeroclaw/wss.cert"
   key_path = "/home/youruser/.zeroclaw/wss.key"
   ```

   Use absolute paths. The config does not expand `~`.

3. **Open the firewall port:**

   ```bash
   sudo ufw allow 9781/tcp
   ```

   The default WSS port is **9781**. Change it with `port = <number>` in the `[wss]` section.

4. **Start (or restart) the daemon:**

   ```bash
   zeroclaw daemon
   ```

   You should see a log line confirming the WSS listener started on `0.0.0.0:9781`.

### On your workstation (zerocode side)

5. **Connect with TLS verification skipped:**

   ```bash
   zerocode --connect wss://<remote-ip>:9781 --tls-skip-verify
   ```

   `--tls-skip-verify` is required for self-signed certificates. The HMAC session signing still authenticates the connection.

That's it. zerocode reconnects automatically if the connection drops.

## Config reference

The `[wss]` section in `config.toml`:

| Field | Default | Description |
|-------|---------|-------------|
| `enabled` | `false` | Enable the WSS listener |
| `bind` | `0.0.0.0` | Bind address |
| `port` | `9781` | Listen port |
| `cert_path` | (none) | Absolute path to PEM certificate |
| `key_path` | (none) | Absolute path to PEM private key |

## Environment variable pass-through

The daemon runs as a background process and typically has a stripped-down
environment. Your terminal has the full environment set up by your shell
profile. There are two ways env vars reach shell subprocesses spawned by the
agent.

### zerocode forwarding (automatic)

When zerocode connects it captures its own process environment and sends it to
the daemon as part of the `initialize` handshake. The daemon stores that
snapshot in `TuiRegistry` keyed by zerocode's unique `tui_id`. When you open a
new chat session (`session/new`), the daemon looks up zerocode's snapshot and
clones it into the agent's `ShellTool`. That clone is then overlaid on top of
the safe-env baseline for every shell subprocess the agent spawns:

```
cmd.env_clear()
  → Layer 1: SAFE_ENV_VARS + shell_env_passthrough (from daemon process)
  → Layer 2: zerocode's env snapshot (wins on conflict)
```

zerocode vars win on conflict — your `PATH`, `HOME`, and credential sockets
take precedence over whatever the daemon inherited. No configuration required.

This is why `SSH_AUTH_SOCK` works when you run zerocode from a terminal that
has an ssh-agent running, even if the daemon was started as a service with no
agent:

```bash
# Terminal has SSH_AUTH_SOCK set by ssh-agent or a hardware token (YubiKey, etc.)
echo $SSH_AUTH_SOCK
# /run/user/1000/gnupg/S.gpg-agent.ssh

# Daemon was started as a systemd service — no SSH_AUTH_SOCK in its env.
# zerocode forwards its env at connect time, so any shell command the agent
# runs (git push, ssh, gpg-sign) gets SSH_AUTH_SOCK from your terminal.
```

zerocode sends its full environment. On a shared or remote daemon where that's
a concern, use WSS with a dedicated user account.

### Multiple connected clients — no cross-session clobbering

Each zerocode instance gets a unique `tui_id` (`tui_` + 8 random hex chars).
The registry is a `HashMap<tui_id → TuiEntry>` — entries are completely
independent:

```
TuiRegistry
├── "tui_a1b2c3d4"  →  { env: { PATH: "/home/alice/…", VIRTUAL_ENV: "…" } }
├── "tui_beef0042"  →  { env: { PATH: "/home/bob/…"  } }
└── "tui_cafe1234"  →  { env: { PATH: "/opt/pyenv/…" } }
```

When zerocode `tui_a1b2c3d4` opens a session, only *its* env snapshot is
cloned and used. The other clients' envs are never touched. Concretely:

| Scenario | Result |
|---|---|
| Two clients open from different shells with different `PATH`s | Each session gets its own `PATH`; neither affects the other |
| Client A has `VIRTUAL_ENV` set; Client B does not | Only sessions from Client A see `VIRTUAL_ENV` |
| Client A disconnects while Client B's session is running | Client B is unaffected — env was **cloned at session creation** |
| Client A reconnects with the same `tui_id` | Old entry is removed, new entry with fresh env is registered; already-running sessions keep their original clone |

The last point matters: `get_env` returns a **clone**, not a reference. Once a
session is created it owns its env snapshot. Reconnects or disconnects of the
originating client have no effect on running sessions.

### Risk profile passthrough (explicit allowlist)

`shell_env_passthrough` on a risk profile controls which variables from the
*daemon's own process environment* are passed to shell subprocesses. This is
useful when you want specific vars available regardless of whether zerocode is
connected — for example, on a headless server where the daemon itself has the
vars set.

```toml
[risk_profiles.default]
shell_env_passthrough = ["SSH_AUTH_SOCK", "GPG_AGENT_INFO"]
```

Subagents cannot expand this list beyond what the parent policy allows — adding
a var not present on the parent's list is rejected as a policy escalation.

## CLI flags

| Flag | Description |
|------|-------------|
| `--connect <url>` | Connect to a remote daemon via WSS (e.g. `wss://host:9781`) |
| `--tls-skip-verify` | Skip TLS certificate verification. Required for self-signed certs |
| `--config-dir <path>` | Override the config directory |
