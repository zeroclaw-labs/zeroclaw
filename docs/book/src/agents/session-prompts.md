# Persistent session prompts

Persistent session prompts are short, durable instructions attached to one chat
session. They help an agent retain the task or operating constraints that must
survive a long conversation, history compression, or a daemon restart.

They are session-scoped, not agent-scoped: another chat session never sees
them. Resetting or deleting the session removes them atomically with its chat
history. They are stored only by the SQLite session backend.

`/new` attempts that same durable cleanup even when prompt injection is
currently disabled, provided a durable session backend is attached. If that
backend cannot prove that cleanup completed, the command reports failure and
leaves the existing session intact; it never claims a fresh session while the
old attachment rows may still exist. When no durable backend is configured and
prompt injection is disabled, `/new` retains the legacy ephemeral reset
behavior because that runtime has no attached durable owner it can clear. An
operator switching back to durable persistence should reset the session after
the backend is re-enabled before relying on the old durable rows being gone.

## Enable the feature

The feature is off by default. Enable durable SQLite sessions and then opt in:

```toml
[channels]
session_backend = "sqlite"
session_persistence = true
session_prompts_enabled = true
```

An enabled configuration with another session backend is rejected. Prompt
attachments are not available to cron jobs, delegates, subagents, one-shot
requests, or auxiliary calls.

## Agent tools

The current chat session receives three tools when the feature is enabled:

| Tool | Purpose |
| --- | --- |
| `session_prompt_list` | List the current session's attachments, including their contents. |
| `session_prompt_set` | Create or replace an attachment by symbolic ID. |
| `session_prompt_delete` | Remove one attachment by symbolic ID. |

`session_prompt_set` accepts `id` and `content`. IDs must match
`[a-z][a-z0-9_.-]{0,63}`. A session may hold at most four attachments; each
content value is at most 2 KiB and their combined content is at most 8 KiB.

When `max_system_prompt_chars` is finite, `session_prompt_set` first checks
the proposed rendered collection against the largest host prompt that the
current primary turn prepares. It rejects a collection that would not fit,
without writing it; the collection read, check, and update occur in one SQLite
transaction. This is best-effort admission, not the dispatch authority:
configuration or other turn inputs can still change after the snapshot is
computed. In that case, ZeroClaw fails the affected turn before provider
dispatch rather than dropping attachments or truncating host context.

If that final check fails, the affected session cannot use its tools to repair
the collection because the turn never starts. Reset or delete the session,
which atomically removes its attachments; raising `max_system_prompt_chars` is
an alternative when the host-prompt limit is intentionally too small.

Changes take effect on the next top-level turn. The runtime appends a dedicated
`## Session Prompts` section to the host-built system prompt. Entries are JSON
encoded and marked as session continuity context; they cannot override system,
safety, authorization, tool, identity, or host instructions.

## Approval policy

Creating, replacing, and deleting an attachment require a one-time operator
approval by default. This gate is separate from ordinary tool approval and
cannot be bypassed by full autonomy, `auto_approve`, or an "always approve"
session allowlist.

The confirmation identifies the SQLite prompt domain, canonical chat session,
action, attachment ID, and, when setting content, the exact content and its
SHA-256 digest. If the active approval surface cannot show that binding, the
mutation is denied.

The global setting is:

```toml
session_prompt_approval = "required" # default; the other value is "disabled"
```

An operator can override it for an existing risk profile:

```toml
[risk_profiles.trusted]
session_prompt_approval = "disabled"
```

`disabled` turns off only this additional content-bound confirmation. Existing
Read/Act authorization and ordinary risk-profile approval rules still apply.
This is an operator configuration decision; an agent cannot change the active
policy during a turn.

## Privacy boundary

Prompt content is opaque. It is sent to the model as part of the system prompt
and returned by an explicit `session_prompt_list` call, but it is omitted from
generic tool events, receipts, progress, telemetry, and observer records.
Generic completion records for these mutation tools are intentionally omitted,
including when an operator has selected `session_prompt_approval = "disabled"`
and ordinary auto-approval permits the call; this keeps the opaque content out
of generic sinks at the cost of less detailed completion visibility. The
explicit list result and the provider request remain the only content-bearing
surfaces.
