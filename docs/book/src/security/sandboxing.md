# Sandboxing

The runtime can wrap tool invocations in an OS-level sandbox that restricts filesystem access to the workspace and removes access to the parent process's secrets. This is distinct from the autonomy system and command allow-list: those are *policy* layers that decide whether a tool may run; the sandbox is a *mechanism* layer that confines what a running tool can reach if it does run.

Sandbox settings live on a risk profile. Each agent points at a risk profile via `agents.<alias>.risk_profile`; the agent's sandbox enable/backend are read from that profile.

`sandbox_enabled = false` (or `sandbox_backend = "none"`) disables sandboxing for tools running under this profile. See the canonical [Minimal working example](../providers/configuration.md#minimal-working-example) for how a risk profile slots into the rest of the config.

## Auto-detection

`sandbox_backend = "auto"` picks the best available backend at startup:

| Platform | Preferred order |
|---|---|
| Linux | Landlock (kernel 5.13+) → Bubblewrap → Firejail → Docker → none |
| macOS | Seatbelt (`sandbox-exec`, native) → Docker → none |
| Windows | AppContainer (experimental) → Docker → none |
| Any | Docker (if daemon reachable) → none |

To force a specific backend, set `sandbox_backend` to one of the literal values listed above.

## What the sandbox confines

### Filesystem

- **Read access**: restricted to the workspace, `/usr`, `/lib`, `/etc` (read-only), and explicitly-listed extra paths.
- **Write access**: restricted to the workspace and `/tmp`.
- **Forbidden paths**: anything listed in `[risk_profiles.<alias>].forbidden_paths`, or the newer `sandbox_policy` table below.

## Sandbox policy (`sandbox_policy`)

`[risk_profiles.<alias>.sandbox_policy]` is the canonical model for filesystem and network restrictions, layered on top of the legacy `workspace_only`/`forbidden_paths`/`allowed_roots` fields on the same risk profile:

```toml
[risk_profiles.assistant.sandbox_policy]
# Read: deny-then-allow (default allow everywhere)
deny_read  = ["~/.ssh", "~/.gnupg", "~/.aws"]
allow_read = []               # re-allows within denied regions; takes precedence over deny_read

# Write: allow-only (default deny everywhere)
allow_write = [".", "/tmp"]
deny_write  = [".env"]        # exceptions within allowed; takes precedence over allow_write

# Network: allow-only via proxy (empty list = no network); not yet enforced, see below
allowed_domains = ["api.anthropic.com", "*.github.com"]
denied_domains  = []          # checked first, overrides allowed_domains

# Unix sockets (macOS: per-path allowlist; Linux: seccomp handles it, field ignored)
allow_unix_sockets = []

# Raw extra bwrap flags (append-only escape hatch, bubblewrap backend only)
bubblewrap_args = []

# Escape hatch for the mandatory deny-write guardrail list below. Emits a WARN when false.
mandatory_deny_write_enabled = true
```

**Precedence**: `allow_read` overrides `deny_read`; `deny_write` overrides `allow_write`; `denied_domains` is checked before `allowed_domains`.

**Field presence matters.** Each of `deny_read`/`allow_read`/`allow_write`/`deny_write` is presence-preserving: omitting a field falls back to the corresponding legacy field (`forbidden_paths` maps to `deny_read`, `allowed_roots` maps to `allow_read`/`allow_write`); an explicit value, even an empty list, or one shaped like the schema default (`allow_write = [".", "/tmp"]`), always wins outright and is never merged with the legacy fallback. `workspace_only = true` on the risk profile always overrides `allow_write` regardless of what `sandbox_policy` sets.

**Mandatory deny-write guardrail.** Regardless of `allow_write`, a default list of paths is always blocked for writes when `mandatory_deny_write_enabled` is `true` (the default): shell configs (`.bashrc`, `.zshrc`, `.profile`, etc.), git hooks and config (`.git/hooks/`, `.git/config`, `.gitconfig`), `.env`, `.mcp.json`, and editor/agent config directories (`.vscode/`, `.idea/`, `.claude/agents/`). Set `mandatory_deny_write_enabled = false` per profile to disable this merge; it emits a WARN log when disabled.

**No config keys are removed.** `forbidden_paths`/`allowed_roots` remain valid indefinitely as compatibility aliases; no action is required for existing configs that never touch `sandbox_policy`.

### Enforcement matrix: what actually enforces each field, today

This is the part operators most often get wrong: setting a `sandbox_policy` field does not by itself mean an OS-level sandbox is confining that access.

| Field | Enforced by | NOT enforced against |
|---|---|---|
| `deny_read` / `allow_read` / `allow_write` / `deny_write` | Application-layer path guard (`SecurityPolicy`), covering `file_write`, `file_edit`, `git_operations`, and other `PathGuardedTool` read paths, regardless of which OS sandbox backend (if any) is active | Arbitrary shell/script child-process I/O. A permitted `shell`/`python`/`node` invocation is not confined by these lists on any backend today. No OS sandbox backend (Landlock, Bubblewrap, Seatbelt, Docker, Firejail) receives the resolved policy yet; that is follow-up work per [RFC #6996](https://github.com/zeroclaw-labs/zeroclaw/issues/6996) Phase 2, one PR per backend. |
| `allowed_domains` / `denied_domains` / `allow_unix_sockets` / `bubblewrap_args` | Nothing yet | Fully inert: accepted by the schema and carried into the resolved policy, but no enforcement surface (application-layer or OS backend) consumes them. Proxy-based network filtering is RFC #6996 Phase 3. |

The runtime logs a one-time WARN, `sandbox_policy denials are enforced for file tools only; shell child processes are not confined`, whenever `deny_read`/`deny_write` are configured, independent of which backend is selected, precisely because no backend forwards the policy yet.

### Network

By default, sandboxed tools have full network egress but no inbound listening. Per-backend caveats:

- Landlock does not control network, it is filesystem-only.
- Bubblewrap and Firejail can block network when configured.
- Docker container network mode follows `[runtime.docker].network` when `[runtime].kind = "docker"`.

Tool-specific network gates (browser, HTTP, web_fetch) live on those tools' own config blocks (`[browser].allowed_domains`, `[http_request].allowed_domains`, `[web_fetch].allowed_domains`).

For `http_request`, private/local targets remain blocked by default. Use `[http_request].allowed_private_hosts` to allow only named private/local hosts such as `localhost` or `10.0.0.1` while keeping `[http_request].allowed_domains` non-empty; `allowed_domains = []` still disables requests. The existing `[http_request].allow_private_hosts = true` setting remains a broader compatibility opt-in.

### Environment

The sandbox passes through only the env vars listed in `[risk_profiles.<alias>].shell_env_passthrough`. Inherited secrets do not reach sandboxed tools unless explicitly passed.

### Process limits

Per-tool wall-time timeouts live on the tool's own config block (`[shell_tool].timeout_secs`, etc.). Docker-specific limits (memory, CPU) live on `[runtime.docker]` when the agent's runtime kind is set to `docker`:

### Shell binary

By default, the native runtime invokes commands via `/bin/sh`. Set `[runtime].shell` to use a different shell:

```toml
[runtime]
shell = "bash"      # resolves through PATH, or use an absolute path
```

The shell is called as `<shell> -c "<command>"`, so any POSIX-compatible shell works. The value must be either a bare command name found on `PATH` (e.g. `"bash"`) or an absolute path to an executable (e.g. `"/bin/bash"`); relative paths with separators (e.g. `"./sh"`, `"bin/sh"`) are rejected. It is validated when the runtime starts, so an empty, missing, non-executable, or malformed shell fails fast with a clear error instead of breaking the first command. Defaults to `"sh"` when unset.

Only applies to the native runtime kind. Docker uses its container's shell, and Windows (always `cmd.exe`) and Android (always `/system/bin/sh`) ignore the setting and do not validate it.

## Per-backend notes

### Landlock

The Linux-native path. Zero setup, kernel-enforced, very low overhead. Requires kernel 5.13+.

Limitations:

- No network confinement: Landlock only controls filesystem access.
- `forbidden_paths` is enforced via path-based rules, not inode-based, so a clever symlink can sometimes escape (we resolve links before handing to Landlock to mitigate this).

### Bubblewrap (`bwrap`)

User-namespace-based sandbox from Flatpak. Confines filesystem and can block network. Requires `bubblewrap` installed.

<div class="os-tabs-src">

#### Debian/Ubuntu

```sh
sudo apt install bubblewrap
```

#### Arch

```sh
sudo pacman -S bubblewrap
```

#### Fedora

```sh
sudo dnf install bubblewrap
```

</div>

### Firejail

SUID-based sandbox. Older but widely available.

<div class="os-tabs-src">

#### sh

```sh
sudo apt install firejail
```

</div>

Firejail's default profile is fairly permissive; ZeroClaw applies a custom profile. Pass extra args with `firejail_args` on the risk profile.

### Docker

Works anywhere Docker does. The Docker runtime kind (`[runtime] kind = "docker"`) runs each shell invocation in an ephemeral container; see the `[runtime.docker]` block above for image and resource controls.

<div class="os-tabs-src">

#### sh

```sh
docker build -t zeroclaw-sandbox:local dev/sandbox/   # build the bundled toolkit image
```

</div>

Pros: strong isolation, works on any OS. Cons: per-invocation container startup cost (100–500 ms). Best for production deployments where the overhead is acceptable.

### Seatbelt (macOS)

Native macOS sandbox (`sandbox-exec`). Profiles are SBPL: ZeroClaw bundles one for tool runs. Works on macOS 10.11+.

Limitation: some CLI tools (older `git`, some Homebrew-linked binaries) don't cooperate with Seatbelt's file-access rules. If you see "Operation not permitted" errors from the agent's shell calls on macOS, the tool needs broader filesystem access: consider switching to Docker.

### `none`

No sandboxing. Tools run with the full privileges of the ZeroClaw service user. This is what YOLO mode enables. Loud, obvious, intentional.

## Troubleshooting

- **"Sandbox backend unavailable"** on startup: check `zeroclaw service status` and the journal; the auto-detect logs which backends it tried.
- **Tools working on dev, failing in service**: the service user often differs from the CLI user. Verify both have whatever sandbox-adjacent permissions are needed (Landlock: nothing; Bubblewrap: userns enabled; Docker: service user in `docker` group).
- **Slow tool invocations** on the Docker runtime: first invocation pulls the image, subsequent are fast. Pre-pull with `docker pull <image>`.

## Code reference

- Detection: `crates/zeroclaw-runtime/src/security/detect.rs`
- Backends: `crates/zeroclaw-runtime/src/security/sandbox/` (one file per backend)
- Schema: `RiskProfileConfig` and `DockerRuntimeConfig` in `crates/zeroclaw-config/src/schema.rs`
