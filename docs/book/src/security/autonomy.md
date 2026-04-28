# Autonomy Levels

The coarse-grained knob that decides whether the agent asks before acting. Three settings; `Supervised` is the default.

```toml
[autonomy]
level = "supervised"   # "read_only" | "supervised" | "full"
```

## The three levels

### `read_only`

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

**Timeout:** unanswered approval requests expire after `[autonomy] approval_timeout_secs` (default 300). Timeouts are treated as denials.

### `full`

No approval gates — all tool calls flagged low/medium/high run without asking. Workspace and path rules still enforce; sandbox still applies; forbidden commands still block.

This is appropriate for trusted local dev, CI, or SOPs that need to run end-to-end without a human in the loop. If you need `full` + no workspace constraints + no sandboxing, see [YOLO mode](../getting-started/yolo.md).

## Per-tool overrides

Override the classification or gating on a specific tool:

```toml
[autonomy.auto_approve]
tools = ["browser_open", "http"]       # always allow, even at Supervised
```

```toml
[autonomy.always_ask]
tools = ["file_write", "shell"]        # always ask, even at Full
```

```toml
[autonomy.never_allow]
tools = ["browser_automation"]         # deny regardless of level
```

## Command allow/deny lists

For the shell tool specifically:

```toml
[autonomy]
allowed_commands = ["git", "cargo", "grep", "find", "ls", "cat"]
forbidden_commands = ["shutdown", "reboot", "mkfs"]
```

If `allowed_commands` is non-empty, it's strict — any command not listed is blocked. If empty, only `forbidden_commands` applies and the shell-policy validator handles the rest.

## Path rules

```toml
[autonomy]
workspace_only = true
forbidden_paths = ["/etc", "/sys", "/boot", "~/.ssh", "~/.aws"]
```

`workspace_only = true` restricts reads and writes to `<workspace>/**`. `forbidden_paths` always blocks regardless of workspace setting (covers the cases where workspace_only is off).

## Environment passthrough

The shell tool runs in a minimal environment by default. To expose specific env vars:

```toml
[autonomy]
shell_env_passthrough = ["PATH", "HOME", "USER", "LANG"]
```

Secrets (`API_KEY`, `_TOKEN`, `_SECRET`, `_PASSWORD` patterns) are *never* passed through automatically — list them explicitly or fetch from the secrets store inside the command.

## Per-channel autonomy override

A public-facing channel can run at a stricter level than the default:

```toml
[autonomy]
level = "supervised"             # the default

[channels.bluesky]
autonomy_level = "read_only"     # public channel at read-only
```

## Observability

Approval requests, grants, denials, and timeouts all emit structured events via the infra crate:

```
INFO autonomy:approval_requested tool=file_write path=/tmp/foo.txt channel=discord user=alice
INFO autonomy:approval_granted   tool=file_write path=/tmp/foo.txt channel=discord user=alice
WARN autonomy:approval_timeout   tool=shell command="git push" channel=telegram user=bob
WARN autonomy:blocked            tool=shell command="rm -rf /tmp" reason="forbidden pattern"
```

Receipts for blocked calls are written to the [tool-receipts log](./tool-receipts.md) the same as successful calls — a denial is an event worth auditing.

## Why not just a binary "safe mode"?

Because the useful middle ground is big. A user who wants agents to run scripts automatically but not push to master needs something between "everything's allowed" and "nothing's allowed". Three-level autonomy + per-tool overrides + command lists gives that knob without fragmenting the config.
