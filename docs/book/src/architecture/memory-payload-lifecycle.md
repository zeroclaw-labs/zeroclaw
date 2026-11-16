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
| Long-term memory | `zeroclaw-memory` behind `Arc<dyn Memory>` | Backend-specific: shared Tachi store (`data/memory/tachi.db`), or per-agent Markdown files | Stores and recalls must stay agent-scoped. A tool result, log line, or session row is not long-term memory unless a memory write happened. |
| Relationship memory | `knowledge` tool and knowledge graph | Graph backend, when enabled | Capture is explicit. Enabling the graph does not automatically ingest conversations, files, or channel data. |
| Session history | `zeroclaw-infra` session backends and ACP store | Chat/ACP history can persist; live session handles are process-local | History preserves conversation continuity. It is not the canonical store for user preferences, config, or files. |
| Current prompt context | Agent loop prompt assembly | Ephemeral provider request | Recalled memory, hardware RAG, current input, system prompt, skills, and tool results may be sent to the provider. This does not make them durable. |
| History trimming | `agent::history` and `agent::history_trim` | Lossy change to the request/session history shape | Trimming must be visible, preserve tool-call/tool-result pairing, and avoid pretending old context is still available. |
| Tool result payloads | `ToolResult`, `ToolResultMessage`, and the tool dispatcher | Current turn and any persisted session history that records the turn | Bound size and provenance. Large outputs should be capped or summarized intentionally; image-path promotion must only happen for producing tools, not path-listing tools. |
| Files and workspaces | Per-agent workspace security policy | Files persist according to the filesystem, not memory | File contents are not memory just because a tool read them. Writes belong in the agent workspace unless policy explicitly allows more. |
| Media attachments | Channel/gateway media pipeline and `MediaAttachment` | Inbound payload by default; persistence depends on the receiving path | Raw bytes should stay bounded and path-validated. Store summaries or references deliberately rather than silently copying media into memory. |
| Logs and observer events | `zeroclaw-log`, `ObserverEvent`, runtime trace | Optional runtime trace and live observers | Logs are evidence and diagnostics, not source-of-truth memory. Scrub or bound user/tool payloads before logging. |
| Cost and usage records | Cost tracker and provider usage events | Cost ledger when enabled | Usage records describe model calls. They should not carry prompt bodies, tool outputs, or memory contents. |
| Companion agent identity | `{companion_store_dir}/agent-identity.json` | Durable mint-once alias→UUID map | First sight of an alias mints a UUID and never rewrites that key. Renaming an `[agents.<alias>]` entry REPOINTS the new alias at the old UUID and preserves the old alias key (both resolve to one identity) when the rename goes through the config rename cascade. Creating a new `[agents.<alias>]` entry through any supported create surface (config API map-key, section select, CLI `agents create`) drops any preserved mapping for that alias so the fresh agent mints a fresh identity. Hand-editing the config file and reloading does NOT pass through those surfaces: to reuse a renamed-away alias by hand, remove its preserved key from `agent-identity.json` first, or the new agent silently inherits the old identity and its durable task list. |
| Companion capture receipts (legacy) | Existing Companion PortableKernel `memories` rows | Historical rows and pending outbox events retained | U4 retired turn-settlement `NotEvaluated` writes. Legacy fixtures retain decoding and outbox assertions; new reflection receipts live in `soul.db`. |
| Reflection input | Original text and source captured at trusted ingress, stored atomically with the session row | SQLite message ingress; legacy or unsupported rows excluded | Enriched chat history is a derived context view. Hooks, media annotations and remote link text never become owner evidence. |
| Owner review inbox | `soul.db` proposals/reflections and `user_model.db` candidates/revisions/receipts | Local canonical records; inbox is an on-demand view | Reflection proposes only. Operator review applies accepted wording atomically within the owning store and preserves original evidence. |

This table complements [Runtime state and persistence](./runtime-state-and-persistence.md).
That page says where state lives; this page says how user-facing payloads move
through memory, history, tools, files, media, and provider requests.

Historical companion receipts stamp `agent_identity_id` from the host-owned
`agent-identity.json` alias mapping. U4 no longer mints that identity or writes
placeholder receipts at turn settlement. Existing rows and their attribution
are retained; the shared outbox observer still reports pending historical events.

Reviewed AgentSoul is a separate protected domain. Its owner-selected minimum
review and presentation contract is recorded in
[ADR-014](./decisions/ADR-014-reviewed-agentsoul-projection.md). The ADR is
accepted. The live Soul model is the Soul profile store (`soul.db`) described
in [ADR-015](./decisions/ADR-015-one-governed-soul.md); the general memory
store reserves the `soul` namespace so ambient memory never hosts Soul rows.

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

At turn start, the common tool loop can recall relevant memories and inject a
bounded `[Memory context]` block. Automatic injection, `memory_recall`, and
model-facing `memory_export` share the same host-origin and canonical-session
admissibility checks. Backend rows are checked before deduplication, reranking,
or output; a backend's session filter alone is not sufficient evidence of scope.
The renderer additionally filters generated autosave noise, stale tool-result
blocks, relevance, and size.

| Host-stamped turn origin | Ambient memory reads |
| --- | --- |
| Interactive, Channel, AgentDirect | Conversation rows require a matching canonical memory session; without one, only unscoped non-Conversation rows qualify. |
| Cron, Daemon | Conversation rows are excluded even when the job reuses a chat session; other rows still require their admitted session or global scope. |
| SubTurn | Automatic injection and both model-facing read tools are disabled, including unscoped durable facts. The child receives its bounded task context. |
| Tool called outside a stamped turn | Conservative non-Conversation fallback; canonical-session, Soul, and backend-attribution checks still apply. |

Each nested tool loop replaces the parent's ephemeral read policy. Model tool
arguments cannot choose origin or authority. `suppress_memory_inject` only opts
out of the automatic preamble; it does not disable otherwise-admitted explicit
recall. Memory-free cron jobs retain their separate backend/tool exclusion.

The provider request can therefore contain recalled memory without making the
current turn a new memory. Review prompt-assembly changes by asking:

- which memory backend and agent scope were queried;
- whether the query is session-scoped when conversation entries are allowed;
- whether autosave noise, stale tool-result blocks, and low-relevance entries
  remain filtered;
- whether the user or operator can see when older context was removed.

## Companion privacy boundaries

Automatic injection and the `memory_recall` / `memory_export` tools derive their
allowed memory sessions from the
executing turn's canonical memory input, not its transport/history key. WebSocket
history and memory IDs differ; channel turns may have multiple sender/group
scopes. Model arguments cannot select another conversation. Results outside
those scopes are excluded, and unattributed Conversation rows are not returned.
Without a session, only unscoped non-Conversation entries remain eligible.
Recall filters a bounded over-fetch window, not an exhaustive scan: higher-ranked
ineligible rows can still crowd eligible rows outside that window. Durable
memory key identity remains `(agent, key)`; this boundary does not create a
per-session key namespace or prevent same-key replacement by another session.
Conversation writes through `memory_store` use the first canonical memory scope
admitted by the turn; without one, the write is refused. Channel groups put
that group's conversation at index zero, with the sanitized sender as a secondary
read scope. This prevents a new group Conversation write from entering a shared
sender scope. Ordered recall deduplication gives group-local entries precedence;
legacy sender-scoped reads remain available. Markdown cannot preserve
conversation scope, so that tool refuses Conversation writes to it, and ordinary
recall omits its unattributed daily-file rows (which mix daily notes and conversation
autosaves). Its Core file remains globally recallable; existing files are not rewritten.
Reserved Soul entries (namespace `soul` or key prefix `soul::`) remain excluded
even if a backend returns them. A model-selected export session/category can
only narrow the host scope. Operator archive exports through owner APIs retain
their existing contract; the model tool is not an authority to export all data.
Standalone hosts that need Conversation reads must explicitly scope a
`MemoryReadPolicy::SessionScoped` policy and the canonical memory sessions.

These checks reuse existing origin/session metadata; they do not add durable
surface or task tags. A chat surface remains a presentation choice, not a new
authorization boundary. More detailed scene taxonomy and installed two-harness
privacy acceptance remain open under issues #380 and #381.

Generic file access cannot read or write `soul.db`, `user_model.db`, or their
SQLite WAL/SHM/journal files (including recased names) in the configured runtime
directories, even when a
broad workspace/root grant includes those directories. Resolved symlinks obey
the same guard. On Unix, multiply-linked files are compared with current store
and sidecar device/inode identities, so alternate hardlink names are also denied.
If a protected file has multiple links, recursive searches use per-file checks
even outside runtime directories. Windows hardlink coverage is not established.
Recursive content searches whose root contains a runtime
directory use per-file checks (and refuse unsupported multiline searches)
instead of giving an external search process unfiltered directory access. Owner APIs and the bounded prompt projection remain the typed
access paths. These application path guards are not an OS sandbox: approved
arbitrary shell commands and external processes still need their own filesystem
isolation. Filesystem changes racing a path check and use are a separate boundary.
Do not treat a shell approval as a proof of protected-store isolation.

Tachi delegation sends the bounded task and protocol routing fields, without
automatically copying Soul, User Model, or parent history. The production
registry fixture seeds both canonical companion stores and checks the captured
outbound requests. An explicitly included task fact is still task content; this
guarantee does not claim semantic prevention of an LLM paraphrasing its context.

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
arguments and credential-scrubbed full result output. Do not describe that path as "summaries" unless the
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
  threaded through `crates/zeroclaw-runtime/src/agent/loop_/`
- History trimming and tool-result payload shaping:
  `crates/zeroclaw-runtime/src/agent/history.rs`,
  `crates/zeroclaw-runtime/src/agent/history_trim.rs`, and
  `crates/zeroclaw-runtime/src/agent/turn/results_collect.rs`
- Tool and provider message shapes: `crates/zeroclaw-api/src/tool.rs` and
  `crates/zeroclaw-api/src/model_provider.rs`
- Channel attachments: `crates/zeroclaw-api/src/channel.rs` and
  `crates/zeroclaw-api/src/media.rs`
