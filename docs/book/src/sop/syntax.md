# SOP Syntax Reference

SOP definitions are loaded from subdirectories under `sops_dir`. When `sops_dir` is omitted from config, CLI commands fall back to `<workspace>/sops` for offline inspection, but runtime SOP execution is disabled.

## 1. Directory Layout

```text
<workspace>/sops/
  deploy-prod/
    SOP.toml
    SOP.md
```

Each SOP must have `SOP.toml`. `SOP.md` is optional, but runs with no parsed steps will fail validation.

## 2. `SOP.toml`

`SOP.toml` carries the SOP's identity (`name`, `description`, `version`), its
`triggers`, and its execution knobs. The concurrency-admission fields govern what
happens when a trigger arrives while this SOP's execution slots are full:

| Field | Default | Effect |
|---|---:|---|
| `max_concurrent` | `1` | Maximum runs of this SOP *executing* at once. A run parked at a HITL approval or a deterministic checkpoint releases its slot, so it does not count against this. |
| `admission_policy` | `parallel` | How a trigger that cannot admit right now is handled (see below). |
| `max_pending_approvals` | `0` (unlimited) | Upper bound on runs of this SOP parked at a HITL approval simultaneously. Past the bound, further triggers are deferred (backpressure), never silently dropped (except under `drop`). |

`admission_policy` values (`SopAdmissionPolicy`, snake_case):

- `parallel` (default) - admit up to `max_concurrent`; a trigger that cannot admit
  now is **deferred** (surfaced for backpressure/redelivery on the trigger's
  transport), never silently dropped. Best for independent work (e.g.
  PR-approval SOPs).
- `hold` - serialize: admit only when no run of this SOP is active or parked;
  other triggers are deferred. For pipelines whose pre-approval steps must not
  overlap.
- `coalesce` - collapse a concurrent trigger onto the already-in-flight run (the
  in-flight run's latest state already covers it).
- `drop` - legacy fire-and-forget: a trigger that cannot admit is dropped.
  Explicit opt-in only; never the default.

A deferred trigger's recovery is transport-dependent - there is no in-engine
durable pending-trigger queue in this version (that is a separate follow-up):

- **AMQP** (`durable_ack = true`, SOP-only dispatch): the delivery is nacked
  (`requeue = true`) so the broker retries it once there is room.
- **AMQP combined `sop_and_agent_loop`**: the agent side already consumed the
  delivery, so a backpressured SOP overflow is logged loudly and ACKed (not
  redelivered), to avoid double-running the agent side.
- **MQTT / cron / filesystem / channel-router** (and any other headless source
  that only logs its dispatch results): no per-message redelivery, so a
  deferred trigger is dropped after a loud log (the next
  scheduled/published/observed trigger is the only recovery).

```toml
[sop]
name = "deploy-prod"
description = "Production deploy with approval"
version = "1.0.0"
max_concurrent = 1
admission_policy = "hold"
max_pending_approvals = 8

[[triggers]]
type = "manual"
```

## 3. `SOP.md` Step Format

Steps are parsed from the `## Steps` section.

```md
## Steps

1. **Preflight** — Check service health and release window.
   - tools: http_request

2. **Deploy** — Run deployment command.
   - tools: shell
   - requires_confirmation: true
   - input: {"type":"object","required":["version"],"properties":{"version":{"type":"string"}}}
   - output: {"type":"object","required":["digest"],"properties":{"digest":{"type":"string"}}}
   - next: 3
```

Parser behavior:

- Numbered items (`1.`, `2.`, ...) define step order.
- Leading bold text (`**Title**`) becomes step title.
- `- tools:` maps to `suggested_tools`.
- `- requires_confirmation: true` enforces approval for that step.
- `- allow-tools:` and `- deny-tools:` define an explicit per-step tool scope.
- `- input:` and `- output:` attach JSON Schema-like step boundary contracts.
- `- next:` and `- depends_on:` route non-linear runs. Ineligible routed steps
  are marked `skipped` and leave the run `pending` instead of dispatching.
- `- on_failure:` accepts `fail`, `retry:<count>`, or `goto:<step>` and is
  enforced for reported step failures and output schema failures.
- `- mode:` overrides the SOP execution mode for that step.
- `- policy:` names an approval-broker policy (a key in `[sop.approval].policies`)
  that gates this step's approval with required-group membership and quorum. Omit
  it for an unpoliced gate. A step that names a policy absent from
  `[sop.approval].policies` fails closed (the gate stays waiting) rather than
  clearing on a single approval.

### `[sop.approval]` policies and route delivery

A policy may also route its approval out of band to a channel, so an approver can
act without watching the surface that started the run:

```toml
[sop.approval.policies.prod]
required_group = "release"
quorum = 2
# Delivered when a run PARKS at a gate this policy governs.
request_route = "discord.ops:123456789012345678"
# Delivered only if that gate later TIMES OUT (a distinct second route).
escalation_route = "discord.oncall:987654321098765432"
```

Both routes are `channel:recipient`: `channel` is a configured channel's map key
(`<channel>.<alias>`, or bare `<channel>` for a singleton) and `recipient` is that
channel's addressee (a Discord channel id, a chat id, ...). Delivery is best-effort
and never blocks or clears the gate - the approval itself still comes back through
the normal approve/deny surfaces (`zeroclaw sop approve|deny`, the gateway
approve/deny route, or the `sop_approve` agent tool). Routes fire only in the daemon
(where channels are configured); leave them unset (or empty) to notify only the
originating surface, which is the default.

### Deterministic checkpoints: approval and resume

A deterministic run paused at a `kind: checkpoint` step is resolved by the SAME
approve/deny surfaces as an approval gate (`zeroclaw sop pending` lists both,
distinguished by `kind`). On approve, the engine resumes the run and drives any
following `kind: capability` steps headlessly to the next pause or completion -
so a `checkpoint -> capability` tail (e.g. posting an approved draft) executes
without a live agent turn. On deny, the run is cancelled. Both resolutions are
recorded in the approval ledger. A checkpoint step may carry `- policy:` to select
a `request_route`/`escalation_route` for its out-of-band notice; checkpoint
resolution itself is not membership/quorum-gated (it is an in-band pause, not a
policied approval gate).

Two further checkpoint resolutions let a reviewer shape the draft instead of
just gating it (both ledger-audited like approve/deny):

- **Edit (amend)** - opt-in via an `- edit: <field>` bullet on the checkpoint:
  the approver may replace that field of the piped value with their own text
  before the run resumes (on Discord, an Edit button opens a modal pre-filled
  with the current value). The checkpoint's recorded output carries the
  human-approved text; the predecessor step keeps the model's original for the
  audit trail. The ledger row records `decision: amend`.
- **Revise** - offered automatically when the checkpoint's predecessor is an
  `llm.generate` step: the approver sends guidance, the engine re-runs that
  step with the guidance framed as reviewer feedback (`revision_feedback`,
  carried in the step's static config plane - the untrusted payload framing is
  unchanged), replaces the draft, and re-presents the gate. Every gate
  presentation the run makes carries a unique revision (each revise bumps it,
  and so does each later checkpoint's first park); prompt references become
  `<run_id>#<rev>`, and an answer on a superseded prompt - an older draft, or
  an earlier gate's leftover buttons - is refused. Capped at 3 revisions per
  gate; a failed re-draft keeps the previous draft parked and answerable. The
  ledger records `decision: revise` with the guidance as the reason.

### Injected-adapter capabilities

Two `kind: capability` steps perform real side effects through adapters the daemon
injects at engine build; without a daemon (CLI validation, tests) they fail closed
with a clear message, like `shell.exec`:

- **`llm.generate`** - one bounded model call as a pipeline step (no tools, no
  agent loop), on the default agent's resolved model provider. Authored fields in
  `with:` - `instruction` (required), `system`, `output_key` (default `text`),
  `echo` (payload fields copied into the output for downstream piping). The piped
  event payload is delivered inside an explicit untrusted-content frame and is
  never read as configuration.
- **`forge.comment`** - posts a comment to a git-forge issue/PR through the git
  channel's outbound path (provider-agnostic: GitHub / Gitea / Forgejo). Input
  fields: `repo` (`owner/repo`), `number`, `body`, and optional `channel`
  (`git.<alias>`; defaults to the single configured git channel).

Together with a checkpoint they form a headless review pipeline:

```md
1. **Draft** - kind: capability / capability: llm.generate
   - with: { instruction = "...", output_key = "body", echo = ["repo", "number"] }
2. **Approve** - kind: checkpoint / policy: triage
3. **Post** - kind: capability / capability: forge.comment
```

### Step Contract Enforcement

Step contracts are optional. When present, `input` and `output` accept a compact
JSON object with `type`, `required`, `properties`, and `items` fields. The
supported primitive types are `object`, `array`, `string`, `number`, `integer`,
`boolean`, and `null`.

The `[sop]` config controls enforcement:

| Field | Default | Effect |
|---|---:|---|
| `step_schema_enforce` | `true` | Validate declared step input/output schemas at engine boundaries. |
| `step_scope_enforce` | `false` | Treat per-step tool scopes as enforced filters instead of advisory hints. |
| `step_mandatory_tools` | `["sop_advance", "sop_approve", "sop_status"]` | Keep lifecycle tools available while scope enforcement is enabled. |
| `max_step_visits` | `256` | Stop routed runs that revisit one step too many times. |
| `max_step_retries` | `2` | Limit retries requested by a step failure policy. |
| `untrusted_payload_max_bytes` | `8192` | Cap untrusted trigger topic/payload text at a UTF-8 character boundary; `0` disables the cap. |
| `untrusted_input_guard` | `"warn"` | Prompt-guard action for untrusted trigger input: `warn`, `block`, or `sanitize`. |
| `untrusted_guard_sensitivity` | `0.7` | Sensitivity used by prompt-guard screening and outbound redaction. |
| `untrusted_frame_warning` | `true` | Include explanatory warning text in the untrusted-content frame. Frame boundaries remain enabled. |
| `untrusted_outbound_redact` | `true` | Enable shared outbound redaction for SOP content-safety consumers. |
| `procedural_memory_enabled` | `false` | Register the `sop_workshop` tool for proposal capture, review, and explicit SOP write-back. |

Schema enforcement fails closed: invalid step input prevents the step from
starting, and invalid step output is routed through the step's `on_failure`
policy. Routing enforcement replaces linear `current_step + 1` advancement in
LLM and deterministic runs. Tool-scope enforcement narrows the live step turn's
available tools and blocks scoped-out calls at dispatch.

Untrusted trigger topic and payload text is capped, normalized, screened, and
framed before it reaches step context. Framing is always on; the warning text can
be hidden, but raw external trigger text is not interpolated into the model
context.

Procedural memory is opt-in. When enabled, `sop_workshop` can create and inspect
stored SOP proposals, capture completed run context into a candidate procedure,
and apply an approved proposal to `SOP.toml`/`SOP.md`. Write-back only happens
through the explicit `apply` action.

### Run Durability

The `[sop]` config also controls whether run state survives a daemon restart:

| Field | Default | Effect |
|---|---:|---|
| `persist_runs` | `true` | Persist run state - including runs parked at a HITL approval or a deterministic checkpoint - so they survive a restart. Set `false` for an in-memory-only, non-durable engine. |
| `run_store_backend` | `"sqlite"` | Durable backend when `persist_runs` is true. `sqlite` writes `runs.db` under the run-state dir. |

`persist_runs = true` is the default so a parked HITL approval is not lost on
restart (`build_sop_engine` falls back to an in-memory store with a loud log if
the durable backend cannot open, so this is default-safe); `persist_runs = false`
is the documented opt-out for an ephemeral engine.

## 4. Trigger Types

{{#sop-trigger-index}}

For the live-versus-unwired status of each source and the transport details, see [SOP Fan-In](./fan-in/overview.md).

## 5. Condition Syntax

`condition` is evaluated fail-closed (invalid condition/payload => no match).

- JSON path comparisons: `$.value > 85`, `$.status == "critical"`
- Direct numeric comparisons: `> 0` (useful for simple payloads)
- Operators: `>=`, `<=`, `!=`, `>`, `<`, `==`

## 6. Validation

Use:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw sop validate
zeroclaw sop validate <name>
```

</div>

Validation warns on empty names/descriptions, missing triggers, missing steps, and step numbering gaps.
