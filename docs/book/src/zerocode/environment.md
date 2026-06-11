# Environment variable pass-through

The daemon runs as a background process and typically has a stripped-down
environment. Your terminal has the full environment set up by your shell
profile. There are two ways env vars reach shell subprocesses spawned by the
agent.

## zerocode forwarding (automatic)

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

zerocode vars win on conflict: your `PATH`, `HOME`, and credential sockets
take precedence over whatever the daemon inherited. No configuration required.

This is why `SSH_AUTH_SOCK` works when you run zerocode from a terminal that
has an ssh-agent running, even if the daemon was started as a service with no
agent:

<div class="os-tabs-src">

#### sh

```sh
# Terminal has SSH_AUTH_SOCK set by ssh-agent or a hardware token (YubiKey, etc.)
echo $SSH_AUTH_SOCK
# /run/user/1000/gnupg/S.gpg-agent.ssh

# Daemon was started as a systemd service, with no SSH_AUTH_SOCK in its env.
# zerocode forwards its env at connect time, so any shell command the agent
# runs (git push, ssh, gpg-sign) gets SSH_AUTH_SOCK from your terminal.
```

</div>

zerocode sends its full environment. On a shared or remote daemon where that's
a concern, use WSS with a dedicated user account.

## Multiple connected clients: no cross-session clobbering

Each zerocode instance gets a unique `tui_id` (`tui_` + 8 random hex chars).
The registry is a `HashMap<tui_id → TuiEntry>`, and entries are completely
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
| Client A disconnects while Client B's session is running | Client B is unaffected; env was **cloned at session creation** |
| Client A reconnects with the same `tui_id` | Old entry is removed, new entry with fresh env is registered; already-running sessions keep their original clone |

The last point matters: `get_env` returns a **clone**, not a reference. Once a
session is created it owns its env snapshot. Reconnects or disconnects of the
originating client have no effect on running sessions.

## Risk profile passthrough (explicit allowlist)

`shell_env_passthrough` on a risk profile controls which variables from the
*daemon's own process environment* are passed to shell subprocesses. This is
useful when you want specific vars available regardless of whether zerocode is
connected, for example on a headless server where the daemon itself has the
vars set.

Subagents cannot expand this list beyond what the parent policy allows: adding
a var not present on the parent's list is rejected as a policy escalation.
