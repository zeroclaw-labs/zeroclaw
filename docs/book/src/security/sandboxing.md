# Sandboxing

The runtime executes tool invocations inside an OS-level sandbox when one is available. The sandbox restricts filesystem access to the workspace, limits network reachability, and removes access to the parent process's secrets.

This is distinct from the autonomy system and command allow-lists. Those are *policy* layers that decide whether a tool may run; the sandbox is a *mechanism* layer that confines what a running tool can reach if it does run.

## Auto-detection

ZeroClaw picks the best available backend at startup:

| Platform | Preferred order |
|---|---|
| Linux | Landlock (kernel 5.13+) → Bubblewrap → Firejail → Docker → none |
| macOS | Seatbelt (native) → Docker → none |
| Windows | AppContainer (experimental) → Docker → none |
| Any | Docker (if daemon reachable) → none |

You can force a backend:

```toml
[security.sandbox]
backend = "bubblewrap"        # or "landlock", "firejail", "docker", "seatbelt", "noop"
```

Set `backend = "noop"` to disable sandboxing entirely (part of [YOLO mode](../getting-started/yolo.md)).

## What the sandbox confines

### Filesystem

- **Read access** — restricted to the workspace, `/usr`, `/lib`, `/etc` (read-only), and explicitly-listed extra paths
- **Write access** — restricted to the workspace and `/tmp`
- **Forbidden paths** — `~/.ssh`, `~/.aws`, `~/.config` (except ZeroClaw's own), anything in `[autonomy] forbidden_paths`

### Network

By default, sandboxed tools have full network egress but no inbound listening.

For tighter control:

```toml
[security.sandbox]
network = "allowed-domains"
allowed_domains = ["api.openai.com", "api.anthropic.com", "api.github.com"]
```

Or `network = "none"` to block network entirely (useful for pure-local tools).

### Environment

The sandbox passes through only the env vars listed in `[autonomy] shell_env_passthrough`. Inherited secrets do not reach sandboxed tools unless explicitly passed.

### Process limits

- CPU: soft-limited to the ZeroClaw service's share
- Memory: capped at `[security.sandbox] memory_limit_mb` (default unset — no cap)
- Subprocesses: capped at `[security.sandbox] max_subprocesses` (default unset)
- Wall time: tool-specific timeout (default 300 seconds for `shell`)

## Per-backend notes

### Landlock

The Linux-native path. Zero setup, kernel-enforced, very low overhead. Requires kernel 5.13+.

Limitations:

- No network confinement — Landlock only controls filesystem access
- `forbidden_paths` enforced via path-based rules, not inode-based, so a clever symlink can sometimes escape (we resolve links before handing to Landlock to mitigate this)

### Bubblewrap (`bwrap`)

User-namespace-based sandbox from Flatpak. Confines filesystem and can block network. Require `bubblewrap` installed.

```bash
# install
sudo apt install bubblewrap       # Debian/Ubuntu
sudo pacman -S bubblewrap         # Arch
sudo dnf install bubblewrap       # Fedora
```

### Firejail

SUID-based sandbox. Older but widely available.

```bash
sudo apt install firejail
```

Firejail's default profile is fairly permissive; we apply a custom profile bundled with ZeroClaw.

### Docker

Works anywhere Docker does. Runs each tool invocation in an ephemeral container (the `zeroclawlabs/tool-runner` image).

```toml
[security.sandbox]
backend = "docker"
image = "zeroclawlabs/tool-runner:latest"
```

Pros: strong isolation, works on any OS. Cons: per-invocation container startup cost (100–500 ms). Best for production deployments where the overhead is acceptable.

### Seatbelt (macOS)

Native macOS sandbox (`sandbox-exec`). Profiles are SBPL — we bundle one for tool runs. Works transparently on macOS 10.11+.

Limitation: some CLI tools (older versions of `git`, some Homebrew-linked binaries) don't cooperate with Seatbelt's file-access rules. If you see "Operation not permitted" errors from the agent's shell calls on macOS, check if the tool needs broader filesystem access and consider switching to Docker.

### `noop`

No sandboxing. Tools run with the full privileges of the ZeroClaw service user. This is what YOLO mode enables. Loud, obvious, intentional.

## Interaction with the hardware subsystem

Hardware tools (GPIO, I2C, SPI, USB) need device access that most sandboxes block by default. When hardware features are enabled, the sandbox profile is relaxed for specific device paths:

```toml
[security.sandbox]
allow_devices = ["/dev/gpiochip0", "/dev/i2c-1", "/dev/spidev0.0", "/dev/ttyUSB0"]
```

Lock this down to only the devices your agent actually needs.

## Troubleshooting

- **"Sandbox backend unavailable"** on startup — check `zeroclaw service status` and the journal; the auto-detect logs which backends it tried.
- **Tools working on dev, failing in service** — the service user often differs from the CLI user. Verify both have whatever sandbox-adjacent permissions are needed (Landlock: nothing; Bubblewrap: userns enabled; Docker: service user in `docker` group).
- **Slow tool invocations** on Docker — first invocation pulls the image, subsequent are fast. Pre-pull with `docker pull zeroclawlabs/tool-runner`.

## Code reference

- Detection: `crates/zeroclaw-runtime/src/security/detect.rs`
- Backends: `crates/zeroclaw-runtime/src/security/sandbox/` (one file per backend)
- Config: `[security.sandbox]` block in `crates/zeroclaw-config/src/schema.rs`
