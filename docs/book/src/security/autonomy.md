# Autonomy Levels

Autonomy is a per-agent setting that lives on a named risk profile: `[risk_profiles.<alias>].level`. Each agent references one risk profile via `agents.<alias>.risk_profile = "<profile-alias>"`. Three settings; `supervised` is the default.

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

`auto_approve`, `always_ask`, and `excluded_tools` live as flat lists of tool names on the risk profile (not nested tables). `excluded_tools` is also available per-channel (`channels.<type>.<alias>.excluded_tools`) to hide tools from specific surfaces without changing the profile.

## Cross-channel approval routing

By default an approval prompt is delivered through whichever channel initiated the conversation. To send a profile's tool approvals to a **distinct** approver channel instead (for example, an agent driven from a public channel whose risky actions must be approved by a separate ops channel, or by a different principal), set `approval_route` on the risk profile:

```toml
[risk_profiles.frontline.approval_route]
approver_channel = "matrix.ops"     # a channel registry key, NOT the originator
on_no_approver   = "deny"           # default; or "inherit-originator"
timeout_secs     = 120              # default; bounds the approver's response window
```

- `approver_channel` is the channel registry key that receives the approval request. Keys are platform-qualified, `<channel>.<alias>` (for example `matrix.ops` or `telegram.default`); a bare platform name (e.g. `matrix`) resolves only when it is the single channel of that platform. An alias on its own is not a registry key and will fail closed. When the route is set, the approval gate asks **only** that channel, not the originating one.
- `on_no_approver` decides what happens when the approver does not answer decisively, is unreachable, is not a registered channel, or times out:
  - `deny` (the default) fails closed and denies the tool call.
  - `inherit-originator` falls back to the originating-channel prompt (today's behavior).
- `timeout_secs` (default 120) bounds how long the gate waits for the approver before applying `on_no_approver`, so a hung approver channel cannot stall a turn.

When `approval_route` is absent (the default), approvals behave exactly as described above: delivered through whichever channel initiated the conversation. The fail-closed default means a misconfigured or unreachable approver denies rather than silently self-approving.

> **Scope.** `approval_route` is honored on both turn paths: the interactive, channel-driven path (a turn that carries a live channel handle, e.g. a streamed agent chat) and the non-interactive path that runs without an originating channel (gateway chat/webhook dispatch and agent-to-agent peer messages). On the non-interactive path the approver must be a **live, registered channel** in the running daemon (it is resolved through the daemon's channel registry); if that registry is unavailable (for example a one-shot CLI run with no channels started) or the named approver is not live, the gate falls back to the profile's non-interactive default, which fails closed (denies) under the default `on_no_approver = "deny"`.

## Command allow list

For the shell tool specifically: if `allowed_commands` is non-empty, it's strict: any command not listed is blocked. The shell-policy validator handles destructive-pattern detection on top of the allowlist.

## Path rules

`workspace_only = true` restricts reads and writes to `<workspace>/**`. `forbidden_paths` always blocks regardless of workspace setting (covers the cases where `workspace_only` is off).

## Sandbox

OS-level sandboxing fields live on the same risk profile. See [Sandboxing](./sandboxing.md) for backend selection per OS.

## Environment passthrough

The shell tool runs in a minimal environment by default; expose specific env vars via the risk profile. Secrets (`API_KEY`, `_TOKEN`, `_SECRET`, `_PASSWORD` patterns) are *never* passed through automatically; list them explicitly or fetch from the secrets store inside the command.

## Per-channel stricter autonomy

Autonomy is per-agent, not per-channel. To run a public-facing channel at a stricter level than your main agent, define a second agent bound to a stricter risk profile and route that channel to it. Per-channel `excluded_tools` (`channels.<type>.<alias>.excluded_tools`) is the cheaper knob when you only need to hide individual tools, no second agent required.

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
