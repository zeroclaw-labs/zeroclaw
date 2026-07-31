# Memory and payload lifecycle

ZeroClaw carries several kinds of "remembered" information during a turn. They
do not all have the same owner, durability, privacy boundary, or review risk.

Use this page when a change touches memory, history, session persistence, tool
results, files, media attachments, summaries, context trimming, or prompt
assembly. The most important question is not "does the agent remember this?"
but "which surface owns this data, and how long does it live?"

## What owns what

| Surface | Owner | Durability | What reviewers should check |
| --- | --- | --- | --- |
| Long-term memory | `zeroclaw-memory` behind `Arc<dyn Memory>` | Backend-specific: SQLite/Postgres/Lucid/Qdrant/shared stores, or per-agent Markdown files | Stores and recalls must stay agent-scoped. A tool result, log line, or session row is not long-term memory unless a memory write happened. |
| Relationship memory | `knowledge` tool and knowledge graph | Graph backend, when enabled | Capture is explicit. Enabling the graph does not automatically ingest conversations, files, or channel data. |
| Session history | `zeroclaw-infra` session backends, ACP store, and live RPC/session maps | Chat/ACP history can persist; live RPC handles are process-local | History preserves conversation continuity. It is not the canonical store for user preferences, config, or files. |
| Current prompt context | Agent loop prompt assembly | Ephemeral provider request | Recalled memory, hardware RAG, current input, system prompt, skills, and tool results may be sent to the provider. This does not make them durable. |
| History trimming | `agent::history` and `agent::history_trim` | Lossy change to the request/session history shape | Trimming must be visible, preserve tool-call/tool-result pairing, and avoid pretending old context is still available. |
| Tool result payloads | `ToolResult`, `ToolResultMessage`, and the tool dispatcher | Current turn and any persisted session history that records the turn | Bound size and provenance. Large outputs should be capped or summarized intentionally; image-path promotion must only happen for producing tools, not path-listing tools. |
| Files and workspaces | Per-agent workspace security policy | Files persist according to the filesystem, not memory | File contents are not memory just because a tool read them. Writes belong in the agent workspace unless policy explicitly allows more. |
| Media attachments | Channel/gateway media pipeline and `MediaAttachment` | Inbound payload by default; persistence depends on the receiving path | Raw bytes should stay bounded and path-validated. Store summaries or references deliberately rather than silently copying media into memory. |
| Logs and observer events | `zeroclaw-log`, `ObserverEvent`, runtime trace | Optional runtime trace and live observers | Logs are evidence and diagnostics, not source-of-truth memory. Scrub or bound user/tool payloads before logging. |
| Cost and usage records | Cost tracker and provider usage events | Cost ledger when enabled | Usage records describe model calls. They should not carry prompt bodies, tool outputs, or memory contents. |

This table complements [Runtime state and persistence](./runtime-state-and-persistence.md).
That page says where state lives; this page says how user-facing payloads move
through memory, history, tools, files, media, and provider requests.

## Long-term memory

An agent receives its memory handle from the memory factory. Shared backends and
Markdown storage have different concrete layouts, but the review rule is the
same: memory access must stay bound to the agent identity and configured peer
allowlist described in [Runtime internals](../agents/internals.md).

There are two normal ways information becomes durable memory:

- the agent calls a memory tool such as `memory_store`;
- runtime code explicitly stores a memory entry, such as the configured
  conversation autosave path.

Do not treat prompt context, tool output, files, or logs as durable memory by
default. A PR that makes one of those surfaces persistent must name the memory
category, session scope, agent scope, retention behavior, and operator-visible
control.

## Prompt context and recall

At turn start, the runtime can recall relevant memories and inject a bounded
`[Memory context]` block into the user-visible prompt context. Related entry
points do not all apply identical filters. The channel/interactive loop filters
generated autosave noise, stale `<tool_result>` blocks, and Conversation entries
when the turn has no safe session scope or is not user-initiated. Generic memory
loading filters autosave noise and relevance, but does not by itself enforce
that channel-loop Conversation exclusion.

The provider request can therefore contain recalled memory without making the
current turn a new memory. Review prompt-assembly changes by asking:

- which memory backend and agent scope were queried;
- whether the query is session-scoped when conversation entries are allowed;
- whether autosave noise, stale tool-result blocks, and low-relevance entries
  remain filtered;
- whether the user or operator can see when older context was removed.

## Session history and trimming

Session history is the continuity record for a conversation. It can include
chat messages, assistant tool calls, and tool results. It is not the same thing
as long-term memory.

[History management](../agents/history-management.md) owns the trimming
mechanics. This page only names the lifecycle boundary: trimming is a lossy
change to provider-visible/session-visible context, not a memory delete, and it
must be visible rather than silently pretending old context remains available.

Tool-call pairing matters more than byte savings. A history change must not
leave a provider request with a dangling `tool_use` without the matching
`tool_result`, or the reverse.

## Tool results

Tools return a small structured result: `success`, `output`, and `error`. The
dispatcher converts those results into provider messages for the next model
call, while streaming clients can receive correlated `ToolCall` and
`ToolResult` events during the turn.

Tool result payloads are easy to over-preserve. Reviewers should check:

- maximum result size, including `max_tool_result_chars`;
- whether truncation preserves structured envelopes and image markers;
- whether search/listing tools avoid turning incidental image paths into media
  payloads;
- whether receipts, logs, and observer events carry bounded, scrubbed evidence
  rather than raw sensitive output;
- whether the result is only in the current turn/session history or is also
  intentionally written to memory.

If a PR says a tool result is "remembered", require it to say whether that means
provider-visible history, persisted session history, a memory backend row, a
file artifact, a receipt, or a log event.

## Files and media

File contents and media bytes are payloads, not memories. The filesystem owner
is the per-agent workspace policy described in
[Filesystem components](../agents/filesystem.md) and
[Runtime internals](../agents/internals.md). A file read can place content in a
tool result or prompt; a file write can create durable filesystem state; neither
automatically creates a memory row.

Inbound channel messages can carry `MediaAttachment` values with a file name,
bytes, and optional MIME type. `MediaKind` is derived from MIME type or file
extension. The low-level attachment loader reads caller-supplied paths verbatim,
so callers that accept untrusted paths must validate or constrain those paths
before loading.

For files and media, reviewers should look for:

- workspace policy enforcement before reads and writes;
- path validation when a path came from a user, HTTP request, channel payload,
  or tool argument;
- bounded byte handling and clear failure behavior for missing or unreadable
  files;
- explicit summaries or references when large/binary payloads enter prompts;
- no silent copy from attachment or file content into long-term memory.

## Logs and observability

Observer events and runtime logs help explain what happened. They should not
become hidden payload stores. Memory recall events carry a scrubbed/truncated
query summary and counts. Memory store events carry bounded category and backend
identifiers.

Tool-call observability needs extra care because the sinks do not share one
payload contract. Current typed tool-call observer events can carry full
arguments and credential-scrubbed full result output, and OTel forwards those
values into span attributes. Do not describe that path as "summaries" unless the
code actually bounds or summarizes it. New telemetry should prefer bounded
identifiers, counts, durations, success flags, and operator-useful summaries.
Put raw content in logs or observer events only when the feature explicitly
requires it and the privacy boundary is documented.

## Reviewer checklist

For memory, payload, history, file, or media changes, answer these before
reviewer sign-off:

- What is the canonical owner of the data?
- Is it current-turn only, session-persistent, filesystem-persistent,
  memory-persistent, or log-persistent?
- Which agent, session, channel, or workspace scope limits access?
- Can a caller widen memory recall beyond the configured allowlist?
- Can autonomous jobs see chat-origin conversation memory?
- What bounds tool output, file bytes, media bytes, and prompt size?
- Does trimming or truncation make loss visible instead of silent?
- Are provider-visible payloads separated from durable memory writes?
- Are logs and observer events scrubbed and bounded?
- If the PR changes a generated or derived payload, does it update the source
  owner rather than hand-editing generated output?

## Source pointers

Canonical docs:

- [Runtime state and persistence](./runtime-state-and-persistence.md)
- [History management](../agents/history-management.md)
- [Runtime internals](../agents/internals.md)
- [Relationship memory](../tools/relationship-memory.md)
- [Tool receipts](../security/tool-receipts.md)

Key code entry points:

- Memory trait and entry shape: `crates/zeroclaw-api/src/memory_traits.rs`
- Memory factory and agent scoping: `crates/zeroclaw-memory/src/lib.rs`,
  `crates/zeroclaw-memory/src/agent_scoped.rs`, and
  `crates/zeroclaw-memory/src/agent_scoped_markdown.rs`
- Memory tool registry and examples: `crates/zeroclaw-tools/src/lib.rs`
  (`MEMORY_TOOL_NAMES`), `crates/zeroclaw-tools/src/memory_store.rs`, and
  `crates/zeroclaw-tools/src/memory_recall.rs`
- Prompt recall and injection: `crates/zeroclaw-runtime/src/agent/memory_inject.rs`
  (recall policy plus the `[Memory context]` renderer), injected engine-side in
  `crates/zeroclaw-runtime/src/agent/turn/mod.rs`; the per-turn memory handle is
  threaded through `crates/zeroclaw-runtime/src/agent/loop_.rs`
- History trimming and tool-result payload shaping:
  `crates/zeroclaw-runtime/src/agent/history.rs`,
  `crates/zeroclaw-runtime/src/agent/history_trim.rs`, and
  `crates/zeroclaw-runtime/src/agent/turn/results_collect.rs`
- Tool and provider message shapes: `crates/zeroclaw-api/src/tool.rs` and
  `crates/zeroclaw-api/src/model_provider.rs`
- Channel attachments: `crates/zeroclaw-api/src/channel.rs` and
  `crates/zeroclaw-api/src/media.rs`
