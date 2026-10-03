# Persistent session prompts

Persistent session prompts are short, durable instructions attached to one chat
session. They help an agent retain the task or operating constraints that must
survive a long conversation, history compression, or a daemon restart.

They are session-scoped, not agent-scoped: another chat session never sees
them. Resetting or deleting the session removes them atomically with its chat
history. They are stored only by the SQLite session backend.

Each admitted primary turn is bound to the durable owner's incarnation, not
just its reusable session key. Reset or deletion invalidates that binding. A
late prompt read or mutation from the old turn fails instead of recreating a
deleted owner or accessing a replacement session with the same key. An empty
session may acquire its owner at admission before its first message; daemon
restart preserves the stored owner and attachments.

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

Gateway WebSocket chat also requires gateway session persistence:

```toml
[gateway]
session_persistence = true
```

If `channels.session_prompts_enabled = true` but
`gateway.session_persistence = false`, WebSocket chat turns fail closed with
`SESSION_PROMPT_LOAD_FAILED` before provider dispatch; they do not proceed
without the attached prompts. This combination is not rejected globally:
channel-only deployments, such as Matrix, can use persistent session prompts
without gateway session persistence. Enable gateway persistence and restart
the gateway before using the feature through WebSocket chat.

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

Strict confirmations use literal previews on CLI, Matrix, Slack, Telegram,
Signal, Web and ZeroCode. They do not interpret proposed content as attachment
markers or rich-text instructions. Slack and Telegram deny a confirmation that
cannot fit their message limit rather than truncating the proposed content.
Slack escapes mention/link control characters in strict previews. Telegram
uses preformatted text for proposed content, including an explicit preformatted
entity on its HTML-failure fallback. These are display encodings only: the
approved content and its digest remain unchanged. Ordinary approvals retain
their existing presentation.
Discord, Lark, Mattermost, WhatsApp Cloud, WhatsApp Web, ACP clients and channel
plugins cannot currently guarantee that literal preview and therefore deny
mutations when this policy is required. This does not disable ordinary tool
approval or change the separately configurable policy below.

For Matrix, this guarantee covers the daemon's plain-text message: it does not
expand file markers, upload attachments, or format proposed text as rich text.
Client-side URL preview fetching is controlled by the Matrix client, not by
ZeroClaw. Disable link previews in the reviewing client if proposed URLs must
not be fetched before a decision.

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

Approval-policy changes follow the existing configuration-generation boundary:
RPC sessions refresh their configuration at turn entry; existing WebSocket
connections retain their construction generation until reconnect, and channel
runtimes require restart. Reconnect or restart the affected surface after
tightening this policy. WebSocket feature availability is checked separately
on each turn; that check does not refresh its approval policy. Update Web and
ZeroCode clients together with the daemon so strict requests do not display an
obsolete "always approve" action. The daemon rejects that action from older
clients without granting or consuming the pending one-time confirmation.

## Privacy boundary

Prompt content is opaque. It is sent to the model as part of the system prompt
and returned by an explicit `session_prompt_list` call, but it is omitted from
generic tool events, receipts, progress, telemetry, and observer records.
Generic completion records for these mutation tools are intentionally omitted,
including when an operator has selected `session_prompt_approval = "disabled"`
and ordinary auto-approval permits the call; this keeps the opaque content out
of generic sinks at the cost of less detailed completion visibility. The
explicit list result, provider request and dedicated operator confirmation are
the content-bearing surfaces. Durable session transcripts and retained/export
copies also replace
the prompt-mutation tool exchange with a redaction marker rather than storing
the opaque arguments or results. This redaction is the restart boundary: after
loading a retained transcript, the model does not recover the hidden tool
exchange from history, but the attached prompt collection itself remains
available through the session metadata and the next injected system prompt.

Disabling prompt injection does not make previously sensitive exchanges public:
retained/export copies still redact host-marked results and their associated
assistant record. Ordinary unmarked conversation remains intact. Disabling the
feature stops subsequent attachment injection, but does not rewrite the active
provider's working history or retroactively remove earlier exchanges from it.

Export redaction can also omit ordinary tool results that share a result carrier
or call batch with a sensitive prompt tool. Restored transcripts do not recover
that omitted tool plumbing. The active provider history remains intact; this
conservative masking does not reconstruct a separate transcript for each tool.
