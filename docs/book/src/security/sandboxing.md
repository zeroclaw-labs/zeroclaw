# Sandboxing

The runtime can wrap tool invocations in an OS-level sandbox that restricts filesystem access to the workspace and removes access to the parent process's secrets. This is distinct from the autonomy system and command allow-list: those are *policy* layers that decide whether a tool may run; the sandbox is a *mechanism* layer that confines what a running tool can reach if it does run.

Sandbox settings live on a risk profile. Each agent points at a risk profile via `agents.<alias>.risk_profile`; the agent's sandbox enable/backend are read from that profile.

**CLI model providers (for example `grok_cli`):** the external CLI is outside
ZeroClaw's native tool-approval path. Risk-profile sandboxing above does not
confine it. The `grok_cli` ACP provider therefore injects `--sandbox strict`,
`--permission-mode dontAsk`, and an empty built-in tool set by default, and it
rejects ACP permission requests (selecting `reject_once` when the CLI offers
it, otherwise cancelling the request). Explicit bypass flags in alias
`extra_args` instead select the request's `allow_once` option; this does not
disable Grok's active OS sandbox or override its deny rules. Other permission
modes remain fail closed. See
[Catalog → Grok Build CLI](../providers/catalog.md#grok-build-cli-slot-grok_cli).

`sandbox_enabled = false` (or `sandbox_backend = "none"`) disables the
profile's additional OS-level sandbox wrapper. Under the native runtime, that
leaves tools without an OS sandbox. Under `[runtime] kind = "docker"`, the
Docker runtime remains the container boundary and is reported as
`docker-runtime`; these settings prevent a second sandbox container from
wrapping the runtime's own `docker run`. See the canonical
[Minimal working example](../providers/configuration.md#minimal-working-example)
for how a risk profile slots into the rest of the config.

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

### File access

- **Read access**: restricted to the workspace, `/usr`, `/lib`, `/etc` (read-only), and explicitly-listed extra paths.
- **Write access**: restricted to the workspace and `/tmp`.
- **Forbidden paths**: absolute component-prefix rules from
  `[risk_profiles.<alias>].forbidden_paths`. Competing allow and deny prefixes
  use most-specific-match precedence, with deny winning ties; see
  [Autonomy path rules](./autonomy.md#path-rules).

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

On Unix, POSIX-compatible shells are called as `<shell> -c "<command>"`. `powershell`/`pwsh` select PowerShell syntax and policy on every supported desktop host and run as `<interpreter> -NoProfile -NonInteractive -Command <command>`, so profile scripts cannot redefine commands behind policy's back and prompts cannot block execution. The value must be either a bare command name found on `PATH` (e.g. `"bash"` or `"pwsh"`) or an absolute path to an executable (e.g. `"/bin/bash"`); relative paths with separators (e.g. `"./sh"`, `"bin/sh"`) are rejected. It is validated when the runtime starts, so an empty, missing, non-executable, or malformed shell fails fast with a clear error instead of breaking the first command. Defaults to `"sh"` when unset.

On **Windows**, the value selects the interpreter family by its file name:

```toml
[runtime]
shell = "pwsh"        # PowerShell 7+   -> pwsh -NoProfile -NonInteractive -Command <cmd>
# shell = "powershell"  # Windows PowerShell 5.x
# shell = "cmd"         # or leave unset -> cmd.exe /C "<cmd>"   (default)
```

`powershell` and `pwsh` (as a bare name resolved via `PATH`, or an absolute path such as `"C:\\Program Files\\PowerShell\\7\\pwsh.exe"`) run through PowerShell; any other value (including the default `sh` and an explicit `cmd`) runs through `cmd.exe /C`, matching the historical behaviour. Only an empty/whitespace value is rejected; the interpreter is located at spawn time.

The shell tool, shell-backed skill tools, and cron/schedule shell jobs all use this runtime selection. The runtime also reports the shell dialect to security policy, so policy validates the same language that will execute the command.

The same runtime selection is reported to the model. The system prompt's `## Runtime` line carries a `Shell:` field naming the configured interpreter (`bash`, `zsh`, `pwsh`, `powershell`, `cmd`), and when a registered tool takes a model-authored command (`shell`, `cron_add`, `cron_update`, `schedule`) a `## Shell` section lists the command forms that dialect accepts, so the model writes `Get-ChildItem` under PowerShell and `dir /a` under `cmd.exe` instead of guessing from the OS name. Both come from the same adapter that builds the command, so the reported shell cannot drift from the executed one. Runtimes without shell access (such as WASM) omit both. Deletion advice in the safety section follows the dialect too: `trash` is only suggested where it exists.

PowerShell policy accepts a bounded grammar: simple command invocations, plain or quoted arguments, and pipelines. Simple variable reads such as `$PSHOME` and `$PSVersionTable.PSVersion` are limited to a standalone `Write-Output`/`echo` command so they cannot hide filesystem paths from later commands. Expressions and alternate invocation forms, including subexpressions, parentheses, script blocks, type literals/static method calls, call operators, redirection, statement separators, backtick escapes, scoped variables such as `$env:NAME`, PowerShell provider paths, direct script execution, and nested command interpreters, are classified as high risk. PowerShell-only command names are not added to the cross-dialect default allowlist; add the cmdlets you need to `allowed_commands`, or opt into `"*"` with the corresponding approval and high-risk settings. Known mutation cmdlets follow the medium/high-risk approval gates; unknown bare commands and `Verb-Noun` cmdlets are high risk by default.

Cron shell jobs inherit the global runtime boundary at both validation and execution time. Native jobs use the configured native shell, while Docker jobs run through the configured image, mount, network, CPU, memory, and read-only-root settings. A cron row stores the command, not a copied runtime or dialect. After a daemon reload recreates the scheduler and tool registry, existing jobs therefore use the newly loaded `[runtime]` configuration on their next run. Scheduled cron runs are revalidated and are never pre-approved.

Only applies to the native runtime kind. Docker uses its container's shell, and Android (always `/system/bin/sh`) ignores the setting and does not validate it.

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
