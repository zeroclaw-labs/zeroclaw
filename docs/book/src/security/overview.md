# Security — Overview

An agent that can execute shell commands, open URLs, and write files is a privileged process. ZeroClaw's security model sits on top of every tool call and every channel message, gating what the agent is actually allowed to do at runtime.

There are six layers. From outer to inner:

## 1. Channel pairing and access control

Before a message from a channel reaches the agent, the channel's pairing and allow-list are checked. `allowed_users`, `allowed_chats`, IP allowlists for webhooks — all enforced at the channel adapter, before the runtime sees the event.

Docs: each channel's page under [Channels](../channels/overview.md).

## 2. Autonomy level

The coarse-grained knob. Three settings:

- **ReadOnly** — the agent can observe (read files, query memory, fetch URLs it's allowed to fetch) but cannot write or execute commands.
- **Supervised** (default) — low-risk ops run; medium-risk ask the operator; high-risk block.
- **Full** — no approval gates, but the other layers (workspace, sandbox, commands) still enforce.

Docs: [Autonomy levels](./autonomy.md).

## 3. Workspace boundary and path rules

The agent operates within a configured workspace directory. `file_read`, `file_write`, and `shell` (for commands that touch the filesystem) refuse paths outside it unless `workspace_only = false`.

Beyond the workspace, a `forbidden_paths` list (default: `/etc`, `/sys`, `/boot`, `~/.ssh`, …) is always blocked regardless of workspace setting.

## 4. Shell command policy

For shell invocations:

- `allowed_commands` — if non-empty, shell only runs commands whose basename is in this list
- `forbidden_commands` — explicit denylist (`rm -rf /`, `shutdown`, kernel operations)
- `validate_command_execution` — a pattern-matching pass that looks for dangerous flags, pipelines, and argument shapes

The validator runs *before* the command hits the shell. A blocked command surfaces as a tool error the model sees and can react to.

## 5. OS-level sandbox

When a sandbox backend is available, tool invocations run inside it:

| Platform | Default backend |
|---|---|
| Linux | Landlock (kernel) / Bubblewrap / Firejail / Docker — auto-detected |
| macOS | Seatbelt (native) |
| Windows | AppContainer (experimental) |
| Any | Docker (if the daemon is reachable) |

The sandbox confines filesystem access to the workspace, drops network reachability except what the tool explicitly needs, and removes access to the parent process's secrets.

Docs: [Sandboxing](./sandboxing.md).

## 6. Tool receipts

Every tool invocation — whether it executed, was blocked, or required approval — produces a signed receipt in a chain. Each receipt includes the hash of the previous one, so tampering with any receipt invalidates the rest.

Receipts are the source of truth for "what did the agent do yesterday". They're readable, greppable, and durable.

Docs: [Tool receipts](./tool-receipts.md).

## Additional gates

Beyond the six layers:

- **OTP gating** — `[security.otp] gated_actions = ["shell", "browser", "file_write"]` requires a one-time code before each listed action. Useful for remote-access scenarios.
- **Emergency stop** — `zeroclaw estop` halts all in-flight tool calls. With `[security.estop] enabled = true`, resuming requires an OTP.
- **Prompt injection guard** — scans model output for known injection patterns before tool calls are validated.
- **Leak detector** — scans outbound messages for secrets (API key patterns, private keys) and blocks sends that match.
- **Pairing guard** — device pairing for channel auth; prevents stolen credentials from working on a new device.

## When things go wrong

A blocked tool call doesn't silently fail:

1. The security validator returns an error
2. The runtime wraps it as a `ToolResult::Err` and hands it back to the model
3. The model sees "Error: Shell command blocked by policy: forbidden pattern `rm -rf /`" and can retry, apologise, or ask the user

If a channel is in a restricted tool set (`tools_allow = [...]`), the tool simply isn't advertised to the model for that channel. Model never sees a tool it can't use.

## Default posture

Out of the box:

- Autonomy: `Supervised`
- Workspace-only: `true`
- Sandbox: auto-detect (uses whatever the OS provides)
- Audit logging: `false` (enable explicitly)
- OTP: `false`
- E-stop: `false`

This is a reasonable middle ground — safe enough for a laptop, permissive enough to not frustrate. Crank it up for production (OTP, audit, restricted tools) or down to [YOLO](../getting-started/yolo.md) for a dev box.

## See also

- [Autonomy levels](./autonomy.md)
- [Sandboxing](./sandboxing.md)
- [Tool receipts](./tool-receipts.md)
- [YOLO mode](../getting-started/yolo.md)
