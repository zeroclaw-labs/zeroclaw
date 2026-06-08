# Autonomy Levels

Autonomy is a per-agent setting that lives on a named risk profile: `[risk_profiles.<alias>].level`. Each agent references one risk profile via `agents.<alias>.risk_profile = "<profile-alias>"`. Three settings; `supervised` is the default.

```toml
[risk_profiles.assistant]   # alias = assistant (must match an agents.<alias>.risk_profile)
level = "supervised"        # "readonly" | "supervised" | "full"
```

`readonly` / `supervised` / `full` are the only accepted values; `read_only` (with an underscore) is rejected at config load. See the canonical [Minimal working example](../providers/configuration.md#minimal-working-example) for how the profile slots into a complete config.

## The three levels

### `readonly`

The agent can observe but not change anything. Permitted tools are the ones with no side effects:

- `file_read`, `file_list`
- `memory_search`
- `http` (GET only; POSTs blocked)
- `web_search`
- `time`

Useful for: a public-facing Q&A agent, an analysis-only deployment, or as a way to vet a new tool configuration before letting it write anything.

### `supervised` (default)

Low-risk tools run automatically. Medium-risk tools trigger an operator approval prompt. High-risk tools are blocked.

Risk classification:

| Risk | Examples | Behaviour |
|---|---|---|
| Low | `file_read`, `http GET`, `memory_search`, `web_search`, `time` | Runs |
| Medium | `file_write` within workspace, `shell` with allowed commands, `http POST` to allowed domains | Asks operator |
| High | `shell` with unknown/denied commands, `file_write` outside workspace, destructive patterns | Blocks |

**Approval channel:** the approval prompt is delivered through whichever channel initiated the conversation. Telegram uses inline keyboard buttons; Slack Socket Mode uses Block Kit buttons; Discord, Signal, Matrix, and WhatsApp embed a short token in the prompt and wait for a `<token> approve|deny|always` reply. In the CLI, it's an inline prompt. In ACP, the agent issues a `session/request_permission` JSON-RPC *request* from agent to client (not a `session/update` notification); the client responds with `{"outcome": {"outcome": "selected", "optionId": "allow-once|allow-always|reject-once"}}` or `{"outcome": {"outcome": "cancelled"}}` to approve, always-approve, or deny. See [ACP → `session/request_permission`](../channels/acp.md#sessionrequest_permission-agent--client-outbound-request).

**Timeout:** unanswered approval requests expire after the channel's `approval_timeout_secs` (default 120 for most channels; see each channel's config block). Timeouts are treated as denials.

### `full`

No approval gates; all tool calls flagged low/medium/high run without asking. `workspace_only` is implicitly disabled (the agent can access paths outside the workspace); `forbidden_paths` still blocks; the OS-level sandbox (`sandbox_enabled` + `sandbox_backend`) still applies.

This is appropriate for trusted local dev, CI, or SOPs that need to run end-to-end without a human in the loop. If you need `full` + no workspace constraints + no sandboxing, see [YOLO mode](../getting-started/yolo.md).

## Per-tool overrides

`auto_approve`, `always_ask`, and `excluded_tools` live as fields on the risk profile; they're flat lists of tool names, not nested tables:

```toml
[risk_profiles.assistant]
level = "supervised"
auto_approve   = ["browser_open", "http"]        # always allow, even at supervised
always_ask     = ["file_write", "shell"]         # always ask, even at full
excluded_tools = ["browser_automation"]          # deny regardless of level
```

`excluded_tools` is also available per-channel (`channels.<type>.<alias>.excluded_tools`) to hide tools from specific surfaces without changing the profile.

## Command allow list

For the shell tool specifically:

```toml
[risk_profiles.assistant]
allowed_commands = ["git", "cargo", "grep", "find", "ls", "cat"]
```

If `allowed_commands` is non-empty, it's strict: any command not listed is blocked. The shell-policy validator handles destructive-pattern detection on top of the allowlist.

## Path rules

```toml
[risk_profiles.assistant]
workspace_only  = true
forbidden_paths = ["/etc", "/sys", "/boot", "~/.ssh", "~/.aws"]
```

`workspace_only = true` restricts reads and writes to `<workspace>/**`. `forbidden_paths` always blocks regardless of workspace setting (covers the cases where `workspace_only` is off).

## Sandbox

OS-level sandboxing fields live on the same risk profile:

```toml
[risk_profiles.assistant]
sandbox_enabled = true
sandbox_backend = "auto"     # "auto" | "landlock" | "firejail" | "bubblewrap" | "docker" | "sandbox-exec" | "none"
firejail_args   = []          # extra args when sandbox_backend = "firejail"
```

See [Sandboxing](./sandboxing.md) for backend selection per OS.

## Environment passthrough

The shell tool runs in a minimal environment by default. To expose specific env vars:

```toml
[risk_profiles.assistant]
shell_env_passthrough = ["PATH", "HOME", "USER", "LANG"]
```

Secrets (`API_KEY`, `_TOKEN`, `_SECRET`, `_PASSWORD` patterns) are *never* passed through automatically; list them explicitly or fetch from the secrets store inside the command.

## Per-channel stricter autonomy

Autonomy is per-agent, not per-channel. To run a public-facing channel at a stricter level than your main agent, define a second agent bound to a stricter risk profile and route that channel to it:

```toml
[agents.public]
model_provider = "anthropic.home"
risk_profile   = "public"
channels       = ["bluesky.home"]

[risk_profiles.public]
level = "readonly"
```

Per-channel `excluded_tools` (`channels.<type>.<alias>.excluded_tools`) is the cheaper knob when you only need to hide individual tools, no second agent required.

## Observability

Approval requests, grants, denials, and timeouts all emit structured events via the infra crate:

```
INFO autonomy:approval_requested tool=file_write path=/tmp/foo.txt channel=discord user=alice
INFO autonomy:approval_granted   tool=file_write path=/tmp/foo.txt channel=discord user=alice
WARN autonomy:approval_timeout   tool=shell command="git push" channel=telegram user=bob
WARN autonomy:blocked            tool=shell command="rm -rf /tmp" reason="forbidden pattern"
```

Receipts for blocked calls are written to the [tool-receipts log](./tool-receipts.md) the same as successful calls; a denial is an event worth auditing.

## Why not just a binary "safe mode"?

Because the useful middle ground is big. A user who wants agents to run scripts automatically but not push to master needs something between "everything's allowed" and "nothing's allowed". Three-level autonomy + per-tool overrides + command allowlists gives that knob without fragmenting the config.
