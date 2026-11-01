# Append-only session events

Session events record accepted inputs, execution and observations. Reading
history never executes a provider, tool, child agent or delivery operation.
Event payloads are stored as plaintext and preserve observed content, including
credential-shaped text. Session capture does not filter messages, reasoning,
tool arguments, results or migration input. Existing configuration credential
encryption and provider error, log, UI and telemetry boundaries are unchanged;
their presentation copies do not replace canonical session content.

## Ownership

| Layer | Responsibility | Source |
| --- | --- | --- |
| API | Typed events, codecs, references, pure validation and replay | `zeroclaw-api::session_events` |
| Infrastructure | Atomic append, receipts, payloads, integrity and installation upgrades | `zeroclaw-infra::session_events` |
| Kernel | Admission, current authorization, execution sequencing and recovery | `zeroclaw-kernel::session` |
| Application | Backend selection and the existing engine bridge | `zeroclaw-session-host` and `zeroclaw::session_events` |
| Provider adapter | Protocol content, termination evidence and diagnostics | `zeroclaw-providers::dispatch::capture` |

Live configuration and permissions remain owned by `LiveConfigAuthority` and
the existing authorization resolvers. Task execution ownership remains with
the task registry. A historical approval, policy observation or cursor grants
no permission. Bound effect handles retain identities, not policy snapshots.
History authority retains the admitted alias generation and live resolver,
without retaining a turn lease or configuration snapshot. Execution keeps its
own lease for the actual turn. Idle readers therefore cannot block agent
deletion, and deleted or closing generations still fail authorization.

The synchronous legacy conversation-binding owner refuses native session
access without execution admission. It must not acknowledge a cache-only
exchange that is absent from the event store. Native callers use the session
execution and authorized history ports.

Session inspection tools obtain their reader from the current execution port;
constructing an ACP agent does not grant access to a second history store.
Daemon and standalone channel startup use the same native session host when no
source was injected. Storage initialization and migration failures propagate
instead of falling back to a legacy writer. Channels with persistence disabled
keep their working context in memory.
Gateway HTTP and WebSocket history use this same source. They do not maintain
a legacy backend handle, transcript watermark or post-turn transcript rewrite.
Legacy backend factories are available only to explicit test fixtures.

## Execution and recovery

The coordinator commits input and turn admission before polling the supplied
engine. It commits provider preparation and start before physical dispatch.
Tool approval consumption and effect start share a single append batch. Every
effect has one initial outcome: completed, failed, verified cancelled or unknown.
Progress checkpoints preserve durable partial text, reasoning, media and tool
output. Content still in an uncommitted memory buffer cannot survive a crash.

Live conversation messages and tool results carry media references in their
`attachments` fields, with an explicit kind and target. Channel ingress,
uploads, steering, context transforms, recovery and provider preparation keep
those fields separate from text. Text such as `[IMAGE:/path]` has no attachment
meaning during normal execution. Only the legacy import path translates the
previous text syntax and tool attachment declarations into structured fields.
Runtime messages keep tool calls, result identity/outcome and opaque reasoning
in typed fields; `content` is always literal text. Context replay, budgeting,
trimming and media preparation operate on those fields. Only provider/channel
adapters render their required wire protocol. File registration does not store
or validate a duplicate byte length; payload bytes are the content owner.

Canonical tool exchanges use `AssistantToolCalls` and `ToolResults` for both
native and text-only model protocols. Tool results retain call IDs, tool names,
observed success, body text and attachments. Text-only provider requests render
temporary text tool blocks in the provider adapter; history never uses a `[Tool results]`
header to distinguish tool output from user input. Slack code snippets without
a file source remain ordinary text. Discord embed/component markup remains a
Discord-specific outbound presentation protocol in the message body.

Gemini CLI and Kilo CLI drain stdout and stderr while writing stdin. Received
UTF-8 stdout is checkpointed before process exit, including output received
before a nonzero exit, signal, decode error or timeout. Text is checkpointed
unchanged, including tokens and URLs split across reads.
The transport bounds stdout at 32 MiB and retained stderr at 64 KiB.
Limits stop capture explicitly; retained
stderr reports truncation while the transport continues draining the pipe.
An I/O deadline cannot cancel an observation already awaiting durable commit.
Cleanup reaps the owned child without asserting a remote cancellation.
Actual exit status is recorded when observed; an abnormal process exit leaves
the remote invocation unknown. Unreported token usage remains absent.
Grok ACP explicit error frames retain their observed message, code and data
and settle as failed, preserving earlier thought and tool observations. These
details stay outside the public error source chain. Its request deadline applies
to raw protocol I/O, so an inflight observation finishes before timeout handling.
Bedrock Converse retains the returned `stopReason`, including unfamiliar values;
an absent reason remains absent.

Provider capture currently bounds observed content at 32 MiB and 16,384
blocks per physical attempt. A limit stops the attempt and propagates a capture
error; previously committed checkpoints remain available. Protocol metadata,
including actual finish/stop reasons and adapter-observed completion signals,
is stored separately from conversation content. A final boundary signal never
overrides an already observed finish reason. Each emitted semantic segment has
stable block coordinates and byte references; the final response reuses those
references rather than duplicating accumulated output. Complete protocol
coordinates determine stable ordering; when any segment lacks a coordinate,
capture preserves observation order instead of moving a later suffix ahead
of its prefix. Mixing item-level and indexed part coordinates within one item
is also ambiguous and preserves observation order. Capture and append validation
share this ordering rule. Older committed mixed-coordinate responses remain
readable in their recorded order, without admitting new reordered responses.
Buffered Codex calls
consume their underlying SSE incrementally through the same bound recorder,
including readable reasoning and opaque finalized continuation. The decoder
cannot hide a retry after native uncertainty. Parser futures stay inert until
first poll, after durable admission. Server-side tool observations preserve
actual provider call IDs; a missing ID remains absent rather than becoming a
tool name or a generated local execution identity. Partial signatures remain
reasoning-protected, display-only observations; they never manufacture a signed
replay block. Unsupported multimodal deltas keep their original protocol
bytes and coordinates instead of disappearing before the terminal frame.
Anthropic adapters emit typed signed and redacted reasoning before their
legacy continuation projection. Capture stores the canonical bytes once and
consumes only the immediately adjacent matching projection. Ordinary readable
thinking is preserved unchanged. Signed and redacted continuation is
replayed unchanged; a partial signature never becomes replayable continuation.
Forced non-streaming responses retain the received block order, including
media, unsupported blocks and protocol metadata.
Codex text-completion observations enter capture when received, before an
item or response terminal is available. Per-part progress prevents repeated
completion frames from duplicating text or hiding another part's output.
Later parts and media are compared at their original content positions, so a
previously observed item cannot suppress newly received terminal content.
Responses parsers accept LF and CRLF framing incrementally. An incomplete
frame at EOF is a decode error, even when its JSON resembles a completion;
previously committed complete frames remain available.
OpenAI-compatible streams preserve structured content, audio and unknown delta
extensions before termination. Native legacy text-stream entry points use the
same structured dispatch and recorder, then project chunks for their callers.
Received protocol metadata is stored as an unmodified content leaf. A provider's
ordinary `payload_id` field cannot create an internal storage reference, and
received content remains eligible for authorized erasure. Historical
observation descriptors and lifecycle identities remain structural facts.
Plain reasoning strings and unknown blocks retain their original content;
unknown blocks retain the existing reasoning-protected content codec.
Gemini adapters preserve only
protocol-owned thought signatures as typed continuation metadata, without
copying text, tool arguments or media into a second replay payload.

Provider message identity, status and phase are retained as `provider_item`
evidence. Supplemental fields on known content types (including text annotations,
log probabilities, tool namespaces and generated-media prompts) use
`content_metadata` evidence. Its `position` identifies the provider output and
content indices; `fields` excludes normalized text, arguments and media bytes.
Provider item `id` remains distinct from the executable tool `call_id` and local
`message_id`. Unknown supplemental fields remain original evidence as well.
These leaves are referenced by `provider.content_metadata` response blocks and
stream observations, so complete and interrupted responses retain them without
adding a second transcript, table or mandatory field to older records.

Execution bindings include historical `execution_context`: effective working
directory, workspace roots, permission profile and resolved filesystem, tool,
approval, delegation and sandbox policy. RPC bindings also retain the principal
tool ceiling. The host materializes these facts from a short current-config read
paired with its revision; a child uses its own workspace and policy. Credentials
and environment values are excluded. Replay never installs this historical
policy: every effect still resolves live authority. An entrance supplies its
actual admitted execution directory through `EngineTurnInput`; ACP reads it
from the constructed agent, RPC uses its authorized session workspace, and CLI
uses its effective security workspace. The accepted ingress retains this
optional admission fact, so safe requeue preserves it even before `TurnStarted`,
subject to the entrance's current checks. Maintenance outside an executing turn
uses the admitted session workspace when available, otherwise the configured
workspace; it never infers an executing turn's directory from history. Older
bindings that lack these fields remain readable; unavailable historical facts
are not invented.

An explicit service error frame remains a definite failure; decode errors,
missing terminal signals and deadlines retain their
diagnostics with an unknown outcome when remote completion is unproven.
The diagnostic checkpoint precedes that unknown outcome, which reuses its
payload reference. Diagnostics have a separate 64 KiB text reserve so a full
content budget cannot erase the reason capture stopped. Uncommitted response
staging is discarded on error; partial results reference only durable bytes.

A repeated input returns its original receipt and result without polling the
engine. A started execution without a terminal receipt requires ownership
recovery; retry does not repeat its side effects. Recovery requires proof from
the execution owner that prior writers have stopped. A timeout or viewer
disconnect alone does not prove cancellation or failure. Recovery records
uncertain effects as unknown and invalidates pending or unconsumed approvals.
Normal turn settlement also invalidates unused approvals from that turn.
It resolves that turn's unanswered elicitations in the same terminal batch,
without changing independently owned requests. Explicit answers, declines,
cancellations and timeouts retain distinct outcomes; an unfinished request
records interruption when its executor returns without resolving it.
New turn-bound elicitation requests require a running turn at commit time;
terminal or requeued turns and closed streams reject delayed admission.
Existing history remains replayable. Outbound RPC disconnects report transport
failure promptly, while an already queued response wins over disconnect.

Approval requests are durable before prompting. Their optional absolute expiry
comes from the approval owner, never a fixed interval after tool preparation.
An unknown deadline or unbounded CLI prompt records no expiry; this does not
disable the response owner's existing timer. A supplied deadline remains a
strict execution boundary. Live authority, the running turn and atomic one-use
consumption are checked even when no absolute expiry is known. Legacy numeric
expiry values remain readable and retain the same execution check. Resolution
evidence distinguishes operator decisions, timeouts, unreachable approvers and
unavailable approval surfaces.
An unsupported channel records a distinct `channel_unsupported` resolution.
Only the existing implicit shell backchannel fallback can use this outcome;
explicit `always_ask`, missing approvers, timeouts and transport errors still
fail closed. This outcome is not operator approval: the shell continues under
its ordinary command policy and current authority checks.

Authenticated approval replies are `ControlInputRecorded` events using
`session.approval-reply/v1`. The original request owns the capture capability;
the control event targets its invocation and cites the approval request as its
cause. Its actor is the single source of responder identity. RPC uses its
authenticated principal, trusted local CLI uses the shared operator, and
platform channels record the channel kind, actual alias and authenticated
sender identifier. Channel responders provide attribution only and do not gain
session access. ACP and WebSocket protocols without an authenticated responder
identity explicitly record `approval_identity_unavailable`; pairing tokens and
their hashes never stand in for a person.

Replacement replies retain their original text through an erasable
`replacement_ref` in the original reply payload. Resolution does not copy that
text and checks it against the response actually delivered. Historical replace
replies without text remain readable but cannot authorize a new replacement.

Each observed reply has its own event ID, including distinct fallback replies
to the same request. The request identity is not a reply deduplication key.
The durable reply event ID travels with the original oneshot response, through
routing and fan-out, to approval resolution. `ApprovalResolved` cites only the
reply actually returned by that waiter and validates its exact decision. A
reply written before timeout or failed delivery remains observable but cannot
be adopted as the identity of a fallback responder. Missing or mismatched
receipts fail closed when a previous reply exists. The resolver claims the
original registration before awaiting persistence, so a replacement waiter
cannot consume the old response.

Reply persistence failures reach both the transport callback and its original
waiter as typed capture failures sharing the original error source chain.
Prompt sending polls that same receiver concurrently, so an already queued
reply wins over a delayed acknowledgement, send failure or route deadline.
Routing, fan-out and the gate propagate them to stop execution. They cannot
become ordinary unreachable denials or trigger another approval route. Actual
transport failures and owner timeouts retain their existing outcomes.

RPC approval replies authorize the original pending registration and cannot
consume its replacement after an authorization wait. Invalid, unauthorized,
misrouted and late replies retain distinct rejection observations. A closed
receiver cannot produce an acknowledged reply. RPC and WebSocket cancellation
guards remove only their own registration; disconnect is unreachable rather
than an operator denial or timeout. Unknown RPC requests can be audited only
against an existing session the caller currently owns.

Restartable input is reconstructed from accepted input and verified recovery
events, in admission order. There is no separately persisted recovery queue.
Hosts resolve current routing and reauthorize the original ingress before
dispatch. A previously started turn without effects resumes through a recovery
binding instead of a second start event; uncertain effects are never retried.
Unavailable routes, revoked senders, obsolete scheduled work and erased input
receive an explicit cancellation when recovery cannot safely proceed. Recovered
output retains its original delivery route and is not returned as the result of
a newer request. Only the creator of a fresh admission owns cancellation when
its queued waiter is dropped; cancelling a duplicate waiter cannot cancel that
admission.

Shell timeout cancellation evidence names the owned process that was reaped.
Process-group cleanup is best effort; the evidence does not assert rollback
of external effects or termination of escaped descendants. Unverified wait or
termination errors remain unknown, with diagnostics.

Context changes record transform start and a success, failure or cancellation.
Installing a result checks its original context binding. Model overrides record
a control input and an applied or rejected selection. Rejection leaves the
effective selection unchanged; an override never changes an already prepared
request. Each prepared route retains requested and physical dispatch identity,
actual runtime route origin, and a reference to the visible applied model
selection. Default configuration revisions stay in the execution binding;
summary purpose stays on preparation. Retry chains link actual terminal events
within one logical call.

Snapshot and summary installation share kernel-owned error settlement. Known
failures retain their diagnostic; stale inputs are cancelled. A settlement
failure propagates together with the original error. An uncertain commit or
conflicting receipt must be verified under its original key and cannot be
replaced by an inferred failure or cancellation.

Changing only the provider or model retains the effective session thinking
selection. Route resolution and provider construction failures record rejected
changes without replacing that selection. Per-message thinking directives
remain scoped to their original input and take precedence during execution.
Both Agent turn entry points apply the resolved thinking to actual requests.
An established outer scope takes precedence, including an explicit disabled
value; otherwise inline, session and configuration defaults are resolved in
that order. Thinking and routing use one configuration generation per turn.
The parameters live only in that turn's scope and its immutable request
payload, without a second persistent selection or Agent cache.

Explicit base-model selection and automatic effort routing are independent.
A `Set` selection retains its provider, model, thinking and routing mode;
an `Inherit` selection retains only thinking and resolves the base model from
current configuration. Legacy `Clear` selections remain readable as inheritance
without a thinking override. Changing thinking preserves that distinction;
session reset durably clears thinking before acknowledging success.
Turn-scoped model changes retain an `AppliedForTurn` observation for request
attribution, without replacing the selection restored by later turns or
branches. Persistent predecessor links may cross turns but must name a visible,
applied session selection in the branch's frozen lineage. Provider preparation
also freezes the request's single-tool or batch-tool round policy.
Turn-bound applied model controls require a running turn at commit time;
rejected late controls can still retain diagnostics. A turn-local ownership
guard restores the actual base provider and resolver after a temporary switch,
including failure, cancellation and dropped futures. Channel restoration keeps
an explicit selection even when it equals the configured default and resolves
route credentials from current configuration.
Selection inheritance and switch observations live in kernel's
`session/model_selection.rs`. The provider factory resolves concrete handles,
credentials and route resolvers; runtime's `agent/session_adapter.rs` only
installs those bindings into the Agent and restores turn-local replacements.
The provider handle, canonical route identity and resolver travel together in
one transient binding. This adds no persisted selection or policy cache.
Explicit channel selections also bind the provider's primary model pin and
cache entry, so a refreshed profile default cannot replace the selected model
on the wire. Automatic hint routes retain their existing profile-pin rules.
Channel reset commits the empty context and cleared thinking in order. Failure
of the second commit can leave the context cleared; it does not acknowledge
success or clear the uncommitted in-memory thinking selection.

Model provider identity currently uses its configured `<family>.<alias>`.
Native RPC and HTTP configuration reject model provider alias renames because
configuration publication cannot atomically migrate references in session
history and frozen branches. The offline CLI also rejects the rename when
native or migrating history exists, under configuration ownership and the
installation format guard. Keep the existing alias. This restriction does not apply
to non-model provider aliases.

Authenticated RPC `session/compact` accepts the owning `session_id` for chat
and ACP sessions. It takes the foreground execution lane, records a maintenance
transform, captures the real summary provider attempt and installs a new context
projection. Current execute and update permissions are resolved before dispatch
and again before installation. Failure leaves the prior context installed;
replay never regenerates a summary. The kernel's embedded session capability
also exposes frozen branch creation and hide/replace operations. Branches inherit
context and model selection only through their frozen ancestor cursors.

The kernel takes the execution lane before reading the source projection.
`session/context_summary.rs` derives the retained system messages, assembles
the summary prompt and rejects empty or reasoning-only summary text. The
injected executor receives that assembled conversation and returns only text;
it cannot choose the retained context. Runtime converts the conversation and
calls the current provider through `ProviderDispatch`; providers own no summary
policy. The kernel returns the committed
projection while still holding the lane, so runtime installs that result
without independently rereading the session. Existing transform and provider
events retain success, rejection, failure, panic and cancellation evidence.

Context projections retain the complete conversation, including tool-call and
tool-result pairing, reasoning continuation and opaque provider signatures.
Flat chat lists are disposable compatibility views. Internal executor restore
requires current session and context authority; it does not grant public
reasoning disclosure. New assistant chat items create their identity once in a content event;
context installation and turn settlement reference that creation. Unchanged
context items reuse their identity. Regenerated summaries always create a new
identity, replace the previous summary and retain its provider provenance.
No-op transforms preserve the canonical summary reference.
Host-supplied history gets immutable message identities
that point into one canonical conversation payload, plus import evidence that
earlier execution was not observed.
The host-seed identity skeleton records the original chat role as well as its
source index. Authorized erasure preserves these identities. Message pages and
search retain readable rows and report erased text spans as `unavailable_parts`
with `erasure_pending` or `redacted` disposition. A removed span cannot join two
remaining text fragments into a new search term. Missing or corrupt bytes
without authorized erasure remain errors.

Each derived child stream records its `execution_turn_id` once at creation.
Completion and reconciliation must reference the terminal of that turn on the
initial child branch. A successful later turn on the same stream cannot settle
the original parent invocation. Reconciliation evidence binds both the terminal
event ID and cursor, and its resolution must agree with that terminal outcome.
Older derived roots without this binding remain readable but cannot admit new
settlement based on an inferred turn; new roots require the binding.

Post-turn skill reflection and review register background child executions
before the parent commits. Their provider reasoning, tool activity and review
context belong to those children. They cannot replace the parent's context.
Ordinary optional skill failures retain their existing host behavior, while
capture and storage failures propagate to the caller.
If preparation or execution stops the skill sequence, its registered children
are explicitly interrupted. A child that has not admitted an engine is settled
without polling one. Dropped owner futures attempt the same cleanup; durable
ownership recovery remains responsible after process loss or failed cleanup.
Before turn admission, cancellation closes the child stream and uses that
durable fence to cancel the parent's invocation. An existing unknown outcome
stays unknown: stream closure does not supply a bound turn terminal for
reconciliation. Failure to initialize a child after its parent intent commits
records an unknown outcome with diagnostics; a failed settlement propagates
both errors.
The blocking child initializer owns a cleanup handle until its caller takes
the result. If the waiter is dropped during the commit, the unclaimed result
settles the unstarted child even when the parent turn has already committed.

Cooperative local execution stop records a cancelled turn after settling its
unresolved effects as unknown. It does not assert remote cancellation. An
unexpected exit preserves already committed progress and requires ownership
recovery before any continuation.
Provider cancellation observations share the turn or transform settlement
batch and reference a matching cancellation request. An observed host stop
without a request records a system intent, never an inferred user action.
The attempt retains its durable partial and an unknown remote outcome;
already settled attempts receive no second outcome. Idle summary maintenance
uses the same kernel settlement rule.

## Storage

The event envelope exposes `kind` alongside the session, turn and sequence
coordinates. The body contains `codec_version` and event-specific `fields`.
SQLite indexes the outer kind for latest-event lookup; typed `EventData`
remains the sole source of the kind in memory.

Unknown event types and unsupported body codecs retain the original type,
fields and body extensions when decoded and serialized again. Readers skip
their semantics; they cannot be authored through the typed append interface.
Migration keeps the original event bytes and follows explicit raw references.

SQLite stores canonical event bytes and payloads in one immediate transaction.
Sequence and event ID columns are checked against those bytes. WAL durability,
head comparison and immutable append receipts determine committed visibility.
Erasure disposition and cold append admission derive decisions from committed
events in the same transaction. Recovery uses transaction boundaries, ordered
sequences and published roots. Events and payloads have no content hash chain,
stored digest or stored byte length; payload references contain their identity
and codec. Manual changes outside the program are not a tamper-detection surface.

Both backends apply the shared 64 MiB batch budget to payloads and the final
committed event encoding, including assigned sequence fields and JSONL
framing. An oversized batch publishes neither history nor a receipt.
Reads have no byte budget or total-history count cap. Event pages retain their
per-request limit of 1 through 10,000 events, and message/search pages retain
their own per-page limits. Payload reads return the complete stored body.

JSONL publishes immutable batch segments and commits through an atomic root
manifest. Files and directories are synchronized before returning a receipt.
Staging or orphan files do not become history. A missing committed segment or
payload is an error, never an empty or truncated conversation.
Disposable indexes are reconstructed from committed facts and do not own state.
The native writer currently requires Unix directory synchronization. On other
platforms publication fails explicitly instead of reporting an unproven durable
commit. Process-interruption tests cover publication boundaries; they do not
certify filesystem or hardware behavior after power loss.

The opt-in Linux/ext4 machine-interruption harness boots an isolated QEMU
guest, kills it at five SQLite/JSONL publication boundaries without shutdown,
and boots recovery against the same virtual disk. Recovery checks atomic
event/payload/receipt visibility and frozen-request deduplication. It uses
`cache=none` so guest flush requests reach the host filesystem; it does not
certify a physical drive's flush implementation or other filesystems.

Build the infra library tests, locate the resulting executable under
`target/debug/deps`, then run:

```bash
python3 scripts/test/session_event_power_loss.py \
  --binary /path/to/zeroclaw_infra-test-executable \
  --kernel /path/to/matching-linux-kernel \
  --busybox /path/to/busybox \
  --work-dir /path/on/the/filesystem-under-test
```

The kernel must include virtio-blk and ext4. The harness needs QEMU, cpio,
mkfs.ext4, util-linux mount, ldd and strip; it requires no root access or host filesystem mount
inside the guest. Its work directory retains per-boundary serial logs.

The bounded installation manifest (`sessions/session-format.json`) selects one
active format for every root and descendant. It contains a schema version,
writer generation and either `upgrading`, `switching` or `ready` state, with no
per-session routing records. Before opening execution or history readers, the
host imports all legacy SQLite/JSONL sessions into the configured native backend.
Read-only enumeration therefore includes old history before a session is resumed.
Immutable `family_key` events own family names; catalogs are disposable views.

Legacy writers, including read-side repairs of old ACP derived columns,
share the installation publication lock and are fenced before import starts.
Stable import identities and bounded batches allow restart at
the last committed receipt. Only after every import completes are old history
files renamed with a `.backup` suffix and the ready state published. Sources
are not deleted or reopened for writes. Conflicting source histories fail
explicitly. Migration preserves original payload content without credential
filtering. Configuration credentials retain their existing encryption.
SQLite message timestamps are retained in imported context entries; native
message history uses each content event's observation time. Sources without
message timestamps remain unknown rather than acquiring the migration time.
Channel and sender attribution are derived from accepted ingress in the common
session projection. Later legacy imports cannot replace these native facts.
Channel-specific room and thread details remain in the original reply route;
the generic catalog does not duplicate that adapter-owned schema.

ACP sessions use the same configured session backend. Ownership, workspace,
interaction surface, plans, usage and lifecycle observations are metadata
events in the standard stream. Typed legacy conversations, retained context
and unfinished checkpoints are imported with receipts; checkpoints remain
partial historical evidence and never authorize repeating an effect.
`acp-sessions.db` is renamed as a backup after the installation import finishes.
ACP uses the standard SessionRepository over canonical events; the old SQL
reader exists only inside installation upgrades and explicit test fixtures.
Conditional ownership and surface updates share the installation publication
lock. Resume uses the canonical context and committed model selection; history
queries cannot promote old checkpoints.
Standalone ACP prepares this standard store before binding its repository
and propagates initialization failures. A fresh installation creates no legacy
ACP database. Existing native installations also finish pending ACP imports
before serving history enumeration.

During execution, RPC and standalone ACP submit accepted context-usage and
plan observations through the bound coordinator. These restricted metadata
observations share the execution stream's commit lock and recheck current
authorization; they cannot change identity, ownership or permissions. A failed
observation cancels execution and propagates after the owned task settles.
Protocol adapters do not write a competing projection during execution.
Family resolution reads canonical events on a blocking worker, including any
wait for the installation lock, without blocking the async reactor.

ACP, RPC and CLI use the same API `Session` model. `SessionCatalog` exposes
query and command capabilities over the configured event store; the old ACP
SQL implementation is private to the installation importer and explicit test
fixtures. A shared pure reducer computes timestamps, ownership, message counts
and lifecycle state. A derived stream without a local ownership fact exposes
an absent owner; execution authority still comes from its parent request.
Summary reads resolve only each metadata field's last effective assignment
and structural seed identities. Imported observations fill missing fields;
they cannot override a value already recorded by native execution. Detail reads attach a context
reference without duplicating metadata. These reads still scan the fixed event
prefix; no cache or persistent secondary index is introduced.
Agent rename, deletion and interrupted-rename residue checks use this standard
repository across all families. Deletion appends closure events and retains
historical attribution; these operations never write archived legacy backends.

Production RPC contexts contain only the standard session source. History
search, messages, state, rename, append and deletion use its authorized query
and command ports. The old transcript backend and its fallback behavior are
compiled only for explicit compatibility fixtures, not daemon execution.

Channels restore persisted context through the admitted standard session port
before building a provider request. Their bounded in-memory conversation buffer
only supplies working context; it never rewrites legacy transcripts, guesses
breadcrumb ownership from text, or creates a second persistent history.
Disabling persistence retains this in-memory mode. A missing cache in that mode
has no durable fallback; with persistence enabled, the standard context and its
breadcrumb provenance are restored before the next message is appended.

Changing the configured backend drains all installation executors and escaped
observations, freezes writes, transfers the whole verified image including
shared payloads, original event bytes and append receipts, and verifies the
target. The source is renamed with `.backup-<generation>` before publication.
Interrupted transfers resume the committed image, including after the source
rename. Every mutation checks backend and writer generation, so a handle from
before a switch stays fenced even if the backend is switched back later.

Gateway TTL retention ends an idle execution window without closing the logical
session. A later request using the same session id restores the persisted context
and continues in the same stream. Historical TTL close observations remain audit
evidence but do not make the logical session terminal. Explicit closure and
deletion remain terminal. No successor family or locator is created.

CLI and session-host startup share kernel family recovery. After the host proves
exclusive ownership, recovery settles descendants before their parent, including
child requests committed before child stream creation. It preserves uncertain
outcomes and never replays provider or tool execution.

SQLite control-file publication supports Windows using synced files and the
same directory-flush contract as config publication. Windows does not provide
Unix directory-fsync guarantees; native JSONL is rejected before initialization
on non-Unix platforms rather than failing after partial publication.

## Authorized reads and erasure

Queries check current permission before and after materializing a page. A fixed
head freezes event selection, not authorization or erasure. Subscription wakeups
are hints; subscribers recover durable pages from their cursor and recheck
permission before delivery. Reasoning disclosure is checked separately and
is denied by default. Local operator readers explicitly opt in; ordinary
history grants do not grant access to reasoning leaves or raw provider
checkpoints.

Message pagination binds selection to a durable event prefix and preserves
message IDs across reconnect and reopen. Each page rechecks current permission
and erasure. A cursor cannot authorize a different stream. Native content is
materialized only for the selected message window.

Native search builds a disposable SQLite FTS5 trigram index from the already
authorized message view for either backend. It retains no disk copy or live
permission snapshot. Literal Unicode substring queries preserve transport
compatibility; short terms use a bounded SQL scan. Kernel rechecks authority
and rematerializes selected messages against current erasure before returning
hits. This index is rebuilt per request; it does not claim persistent-index
latency or global search over unauthorized families.

Delivery observations name the boundary actually measured. HTTP records body
production when Hyper polls it, SSE records terminal queue acceptance, and
WebSocket records a successful local socket write. None proves remote reading.
A dropped, unpolled HTTP response does not manufacture a delivery receipt.
Accepted A2A parts retain their original order; unsupported media is recorded
as opaque protocol content without granting a URL-fetch capability.

Only registered content leaves can be erased. Identity and relationship
skeletons remain so replay preserves message IDs, positions and lifecycle
constraints. Erasure checks every affected reference scope and compares its
reference commitment inside the append transaction. New references to pending
or erased content are rejected. Unknown codecs remain readable but prevent
graph mutations whose reachability cannot be established.

Payload lifecycle rules have one registry in
`zeroclaw-api/src/session_events/payloads/codecs.rs`. Each registration declares
either an opaque content leaf or a structure with an optional typed validator.
Reference traversal and erasure use that registry; SQLite derives its content
body projection from the same registrations as JSONL. Adding a codec requires
one registration, with its decoder when it has a typed schema. Persisted codec
names and bytes remain unchanged.

`session.attachment-reference/v1` is an erasable content leaf for a typed
attachment reference already admitted by the Gateway. This change does not
add raw-file registration or history recovery for Email, Matrix, WhatsApp, ACP,
or RPC; those paths remain the #9488 scope. Queries never infer references from
arbitrary file bytes or body markers, nor reuse a later input's metadata for an
earlier message. Erased references or metadata remain unavailable.

Native session deletion appends `SessionClosed`, removes the active session,
and hides it from ordinary lists. RPC and Gateway responses report
`history_retained: true`; deletion does not authorize payload erasure. Closed
identities remain reserved so a reused ID cannot replace their history.

SQLite content erasure clears body bytes with secure deletion enabled and
requires a successful `wal_checkpoint(TRUNCATE)` before acknowledging physical
completion. A pinned raw reader can hold old WAL pages: the result is
`CommitOutcomeUnknown`, and the same receipt must be retried after it releases.
Readers already refuse erased content while physical completion is pending.
JSONL erasure first commits the intent and freezes reads, then removes owned
content files and synchronizes their directory before publishing confirmation.
The authorized erasure request returns its original intent receipt only after
cleanup completes. Family startup resumes existing intents on root and derived
streams. Confirmation identities come from those intents, so a retry completes
the same deletion, including a pending SQLite checkpoint. Ordinary reads never
trigger deletion, and cleanup cannot create a new erasure authorization.

Payload erasure does not certify removal from backups, filesystem snapshots,
or external exports. Back up SQLite through its backup API or capture the
consistent database and WAL set while writers are stopped; copying only the
main database while WAL writers run is not a valid backup. Back up JSONL's
published root, immutable commits, segments, and referenced content under the
installation ownership lock. Operators must apply their retention policy to
all copies and exports before claiming installation-wide content removal.
Append request commitments identify retries, and migration commitments identify
the transferred snapshot; neither forms a content-integrity chain.
