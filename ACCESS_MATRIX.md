# DaemonClaw Access Matrix

Reference document for `daemonclaw service install`. Every file and directory the installer creates must match this matrix exactly. If a permission can't be expressed in this table, it shouldn't exist on disk.

---

## Groups

Two groups. One ACL.

| Group | Scope | Purpose |
|---|---|---|
| `agents` | System-wide | Read tier. Primary group for agent service accounts (daemonclaw, zeroclaw). Claude-code and operators also join. Group-owns config, state, logs, backups, and shared workspaces. Files agents create are group `agents` by default — other agents and tools can immediately work with them. |
| `daemonclaw-admin` | DaemonClaw-specific | Config write escalation. One ACL on `config.toml` granting `rw-`. That is the entire scope of this group. |

### Group membership

| User | Primary group | Purpose |
|---|---|---|
| `daemonclaw` (service) | `agents` | Runs the daemon. System user, nologin. |
| `zeroclaw` (if coexisting) | `agents` | Existing agent runtime. Shares read access. Can work in shared paths. |
| `claude-code` | `agents` | Dev tool. Can read agent state, manipulate files agents create. |
| Operator (read-only) | their own | Joins `agents` for read visibility. |
| Operator (read-write) | their own | Joins `agents` + `daemonclaw-admin` for config write. |

### Why `agents` as primary group

Files the daemon creates at runtime inherit its primary group. With `agents` as the primary group:
- Files created in shared/public spaces are immediately accessible to every `agents` member. No ACLs, no chown.
- Files created inside private directories (0700) are still private. The directory mode is the gate, not the file's group.

The installer creates `agents` if it doesn't already exist, but doesn't own it. If another setup creates it first, the installer detects it and skips creation.

---

## How the service account accesses files

For files it **owns** (everything under its home except config): access via **owner bits**. Group bits control what other `agents` members can do.

For `config.toml` (at `/etc/daemonclaw/config.toml`, owned by `root:agents`): access via **group membership** in `agents`. The agent reads through a symlink from its home directory. It can delete the symlink but cannot touch the actual file because `/etc/daemonclaw/` is owned by root.

---

## Access Matrix

Legend:
- **Owner** = access via Unix user ownership bits
- **Group** = access via Unix group ownership bits
- **ACL** = access via POSIX ACL entry
- **—** = no access
- **chattr +i** = immutable flag, only root can toggle

All `$HOME` paths are relative to `/var/lib/daemonclaw`.

### Config

Config lives in `/etc/daemonclaw/`, a root-owned directory the agent cannot write to. A symlink from the agent's home provides read access.

| Path | owner:group | Mode | `daemonclaw` | Other `agents` | `daemonclaw-admin` | Notes |
|---|---|---|---|---|---|---|
| `/etc/daemonclaw/` | `root:agents` | `0750` | **read/traverse** (Group) | **read/traverse** (Group) | **read/traverse** (via `agents`) | Root-owned. Agent cannot create, rename, or delete files here. |
| `/etc/daemonclaw/config.toml` | `root:agents` | `0640` | **read** (Group) | **read** (Group) | **read/write** (ACL `g:daemonclaw-admin:rw-`) | The one ACL in the system. |
| `$HOME/.daemonclaw/config.toml` | `daemonclaw:agents` | symlink | → `/etc/daemonclaw/config.toml` | — | — | Agent can delete the symlink but the target is untouchable. |

### Secret key

| Path | owner:group | Mode | `daemonclaw` | Other `agents` | Notes |
|---|---|---|---|---|---|
| `$HOME/.daemonclaw/.secret_key` | `daemonclaw:agents` | `0600` | **read/write** (Owner) | — | **chattr +i** after creation. Mode 0600 blocks group. Only root can remove immutable flag. |

### Workspace directories

Agent-owned. The agent has full control over contents, including the ability to delete them. Durability of important data is handled outside the agent's home (see Data Durability section).

| Path | owner:group | Mode | `daemonclaw` | Other `agents` | Notes |
|---|---|---|---|---|---|
| `$HOME/.daemonclaw/` | `daemonclaw:agents` | `0750` | **full** (Owner) | **read/traverse** (Group) | Parent directory. |
| `$HOME/.daemonclaw/workspace/` | `daemonclaw:agents` | `0750` | **full** (Owner) | **read/traverse** (Group) | Agent workspace root. |
| `$HOME/.daemonclaw/workspace/github/` | `daemonclaw:agents` | `0750` | **full** (Owner) | **read/traverse** (Group) | Cloned repos. Other agents and claude-code can read. |
| `$HOME/.daemonclaw/workspace/skills/` | `daemonclaw:agents` | `0750` | **full** (Owner) | **read/traverse** (Group) | Installed skills. |
| `$HOME/.daemonclaw/workspace/memory/` | `daemonclaw:agents` | `0700` | **full** (Owner) | — | **Private.** Conversation memory. |
| `$HOME/.daemonclaw/workspace/sessions/` | `daemonclaw:agents` | `0700` | **full** (Owner) | — | **Private.** Session persistence. |

### State and logs

Agent-owned. The agent can destroy these — durability comes from journald and external backups, not file permissions.

| Path | owner:group | Mode | `daemonclaw` | Other `agents` | Notes |
|---|---|---|---|---|---|
| `$HOME/.daemonclaw/state/` | `daemonclaw:agents` | `0750` | **full** (Owner) | **read/traverse** (Group) | Runtime state and traces. |
| `$HOME/.daemonclaw/state/backups/` | `daemonclaw:agents` | `0750` | **full** (Owner) | **read/traverse** (Group) | Agent-side backups. Not the durable copy. |
| `$HOME/.daemonclaw/logs/` | `daemonclaw:agents` | `0750` | **full** (Owner) | **read/traverse** (Group) | Agent-side logs. Secondary to journald. |

### Scratch space

| Path | owner:group | Mode | `daemonclaw` | Other `agents` | Notes |
|---|---|---|---|---|---|
| `$HOME/tmp/` | `daemonclaw:agents` | `0700` | **full** (Owner) | — | Private scratch. Not /tmp. |

### Injected prompt files (future)

| Path | owner:group | Mode | `daemonclaw` | Notes |
|---|---|---|---|---|
| `$HOME/.daemonclaw/workspace/*.md` | `daemonclaw:agents` | `0640` | **read** (Owner, but immutable) | **chattr +i** after creation. Other agents can read (Group). Only root can modify. |

### External backups (root-owned, outside agent home)

The agent can read backups via `agents` group but cannot delete or modify them.

| Path | owner:group | Mode | `daemonclaw` | Other `agents` | Notes |
|---|---|---|---|---|---|
| `/var/backups/daemonclaw/` | `root:agents` | `0750` | **read/traverse** (Group) | **read/traverse** (Group) | Root-owned. Agent cannot write or delete here. Managed by systemd timer + tmpfiles. |

---

## ACL summary

| Target path | ACL entry | Effect |
|---|---|---|
| `/etc/daemonclaw/config.toml` | `group:daemonclaw-admin:rw-` | Admin can edit config |

One ACL.

### Default ACLs

To guarantee `agents` can read runtime-created files regardless of daemon umask:

| Target path | Default ACL entry | Effect |
|---|---|---|
| `$HOME/.daemonclaw/logs/` | `default:group:agents:r--` | New log files readable by `agents` |
| `$HOME/.daemonclaw/state/` | `default:group:agents:r--` | New state files readable by `agents` |

### ACLs NOT applied

- No ACL on `memory/` or `sessions/` — directory mode 0700 is the gate.
- No ACL on `.secret_key` — mode 0600 + chattr +i.
- No ACL for `agents` on anything — all access via group ownership.

---

## Immutability flags (chattr +i)

| Path | Set by | Can be removed by |
|---|---|---|
| `$HOME/.daemonclaw/.secret_key` | Installer, after key generation | root only |
| `$HOME/.daemonclaw/workspace/*.md` | Installer or operator | root only |

---

## Data durability

The agent can destroy everything it owns. Durability lives outside the agent's reach.

### Logs → journald

systemd captures the daemon's stdout/stderr automatically. The agent's log directory is secondary — a convenience for structured logs the daemon writes directly. The durable copy is journald, root-owned, untouchable by the agent.

```bash
# Always works, even if agent nukes its own logs
journalctl -u daemonclaw -f
```

No additional configuration needed. The systemd unit just needs `StandardOutput=journal` and `StandardError=journal` (systemd defaults).

### State → external backups

A systemd timer copies state snapshots to `/var/backups/daemonclaw/`, owned by `root:agents 0750`. The agent can read backups (via `agents` group) but cannot modify or delete them.

#### Backup timer

```ini
# /etc/systemd/system/daemonclaw-backup.timer
[Unit]
Description=DaemonClaw state backup

[Timer]
OnCalendar=hourly
Persistent=true

[Install]
WantedBy=timers.target
```

```ini
# /etc/systemd/system/daemonclaw-backup.service
[Unit]
Description=DaemonClaw state backup

[Service]
Type=oneshot
ExecStart=/bin/bash -c '\
    ts=$(date +%%Y%%m%%d-%%H%%M%%S) && \
    tar czf /var/backups/daemonclaw/state-${ts}.tar.gz \
        -C /var/lib/daemonclaw/.daemonclaw state/ \
    '
User=root
Group=agents
```

The backup runs as root so it can read the agent's private directories (memory, sessions) if included. Adjust the tar paths to include or exclude private data based on your retention policy.

#### Rotation via tmpfiles

```ini
# /etc/tmpfiles.d/daemonclaw-backups.conf
d /var/backups/daemonclaw 0750 root agents - -
e /var/backups/daemonclaw - - - 30d -
```

`d` creates the directory if missing. `e` cleans files older than 30d. `systemd-tmpfiles --clean` runs daily on Ubuntu by default via `systemd-tmpfiles-clean.timer`. Change `30d` to whatever TTL you want — one config line.

---

## systemd hardening

### Port binding

The agent communicates via Telegram polling — outbound only. No reason to bind any port.

```ini
[Service]
SocketBindDeny=any
```

Uses cgroup v2 BPF. Blocks all `bind()` calls. `connect()` is unaffected. Available on Ubuntu 24.04 (systemd 255+). If a future channel needs an inbound port, add `SocketBindAllow=<port>` for that port only.

### Resource limits

Prevents disk-fill and fork-bomb attacks.

```ini
[Service]
MemoryMax=2G
CPUQuota=200%
TasksMax=64
LimitFSIZE=1G
```

---

## Shared workspace (Phase 3)

The `agents` group makes multi-agent shared workspaces trivial:

```
/var/lib/agents/workspace/    root:agents 2770 (setgid)
```

Setgid means files created inside inherit the `agents` group. Any agent and any tool can read and write. Per-agent home directories remain private for agent-specific state.

One directory. One mode. No new groups, no new ACLs.

---

## Security posture summary

| Threat | Mitigation | Layer |
|---|---|---|
| Agent modifies its own config | Config in `/etc/daemonclaw/` (root-owned dir). Symlink deletable, target untouchable. | Filesystem |
| Agent elevates permissions | Can't modify `/etc/group`, `/etc/passwd`, sudoers, systemd units. Can't `chown`, can't `chattr` (no `CAP_LINUX_IMMUTABLE`). | Filesystem + capabilities |
| Agent uses another agent to escalate | Both in `agents` at same tier. Lateral movement only. | Group model |
| Agent destroys logs | journald is the durable copy. Root-owned, agent can't touch. | systemd/journald |
| Agent destroys state | External backups in `/var/backups/daemonclaw/` (root-owned). 30d TTL via tmpfiles. Agent can read but not modify. | systemd timer + tmpfiles |
| Agent fills disk | `LimitFSIZE=1G` in systemd unit. | systemd |
| Agent forks uncontrollably | `TasksMax=64` in systemd unit. | systemd |
| Agent binds a port | `SocketBindDeny=any` blocks all `bind()`. | systemd (BPF) |
| Agent runs arbitrary programs | Allowed by design. Scoped by `allowed_commands` in config (which it can't modify). | Config policy (secondary) |
| Agent modifies prompt files | `chattr +i` on prompt files. | Filesystem |
| Agent reads other agents' private data | 0700 directories block traversal. | Filesystem |

---

## Installer validation checklist

```bash
# 1. Groups exist
getent group agents daemonclaw-admin

# 2. Service user primary group is agents
id daemonclaw
# Expected: gid=...(agents)

# 3. /etc/daemonclaw exists and is root-owned
stat -c '%U:%G %a' /etc/daemonclaw/
# Expected: root:agents 750

# 4. Config ownership, mode, and ACL
stat -c '%U:%G %a' /etc/daemonclaw/config.toml
# Expected: root:agents 640
getfacl /etc/daemonclaw/config.toml
# Expected: group:daemonclaw-admin:rw-

# 5. Config symlink exists
readlink /var/lib/daemonclaw/.daemonclaw/config.toml
# Expected: /etc/daemonclaw/config.toml

# 6. Private directories are 0700
stat -c '%a' /var/lib/daemonclaw/.daemonclaw/workspace/memory
stat -c '%a' /var/lib/daemonclaw/.daemonclaw/workspace/sessions
stat -c '%a' /var/lib/daemonclaw/tmp
# Expected: 700 for all three

# 7. State/logs group is agents
stat -c '%G' /var/lib/daemonclaw/.daemonclaw/state
stat -c '%G' /var/lib/daemonclaw/.daemonclaw/logs
# Expected: agents for both

# 8. Secret key is immutable
lsattr /var/lib/daemonclaw/.daemonclaw/.secret_key
# Expected: i flag present

# 9. Default ACLs on logs and state
getfacl /var/lib/daemonclaw/.daemonclaw/logs/
getfacl /var/lib/daemonclaw/.daemonclaw/state/
# Expected: default:group:agents:r--

# 10. systemd socket restriction
systemctl show daemonclaw.service -p SocketBindDeny
# Expected: SocketBindDeny=any

# 11. Backup directory exists and is root-owned
stat -c '%U:%G %a' /var/backups/daemonclaw/
# Expected: root:agents 750

# 12. Backup timer is active
systemctl is-active daemonclaw-backup.timer
# Expected: active

# 13. tmpfiles rotation is configured
cat /etc/tmpfiles.d/daemonclaw-backups.conf
# Expected: contains 'e /var/backups/daemonclaw - - - 30d -'
```

---

## Differences from ZeroClaw deployment

| Aspect | ZeroClaw (deployed) | DaemonClaw (this matrix) |
|---|---|---|
| Group model | 4 groups | 2 groups |
| Service primary group | `zeroclaw` (project-specific) | `agents` (system-wide) |
| Config location | `/etc/zeroclaw/` root-owned, symlink from home | `/etc/daemonclaw/` root-owned, symlink from home |
| Config write | sudoers (`sudoedit`) | ACL `g:daemonclaw-admin:rw-` |
| claude-code access | claude-code-sandbox + 5 ACLs | Joins `agents`, done |
| Log durability | Agent-owned logs only | journald primary, agent logs secondary |
| State durability | None | External backups, root-owned, 30d TTL |
| Port binding | Not addressed | `SocketBindDeny=any` |
| Cross-agent sharing | Not possible without ACLs | Automatic via shared `agents` group |
| Total ACLs | 7+ | 1 entry + 2 defaults = 3 total |

---

## How to use this document

**For the installer**: every `fs::set_permissions`, `set_ownership`, `setfacl`, `chattr`, systemd directive, and tmpfiles config must correspond to a row in this document. If you're writing something that doesn't appear here, stop and ask.

**For Claude Code**: this is the source of truth for ownership and permissions. Look up the answer here. Don't reason from first principles.

**For validation**: run the checklist after every installer change.
