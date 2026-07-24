# Goal Mode

Goal mode lets an agent track a durable objective instead of treating the
request as one ordinary turn. It is anchored in the task control plane, not in
prompt history. That means lifecycle, route, principal, owner, timestamps, and
terminal state live on the canonical task record. Goal-only data, such as the
objective, effective limits, pause reason, and blockers, lives in the goal task
extension. Consumed tokens and cost remain in the cost ledger.

This separation is deliberate: remaining budget is derived from the effective
limit plus recorded usage, never stored as a mutable counter on the goal.

## Supported Entry Points

In this release, goal admission is available on channel runtimes that supply a
trusted route and principal context. Use the channel slash command:

```text
/goal start [--tokens=N|unlimited] [--cost=N|unlimited] <objective>
/goal objective <objective>
/goal status [task_id]
/goal budget [--tokens=N|unlimited] [--cost=N|unlimited]
/goal pause [reason]
/goal resume [reason]
/goal cancel [task_id]
/goal help | /goal --help | /goal -h
```

`/goal` without a subcommand is an error. `/goal help`, `/goal --help`, and
`/goal -h` print the supported command shape. `status` and `cancel` use the
active goal for the same agent, route, and principal when no `task_id` is
provided. Passing a task id to those commands is allowed only when that goal is
visible to the same trusted context. `/goal pause` always targets the active
goal for that trusted context, and any trailing text is recorded as the operator
pause reason. `/goal resume reason text` resumes the current paused goal and
includes the reason as untrusted operator content in the continuation prompt.
The resume reason is free-form text; task-looking ids and flag-looking words are
not parsed as selectors or options. `/goal objective new objective text` amends
the active goal's canonical objective for the same trusted context without
creating a replacement goal and without starting a new model turn by itself.

`/goal start` first admits the durable goal, then the same channel turn
continues with the admitted objective under goal context. The admission
acknowledgement is therefore not the goal's first worker response.

## Configuration

Goal mode is opt-in and disabled by default. The global policy lives under `[goal]`; the
per-agent opt-out lives under `[agents.<alias>.goal]`.

```toml
[goal]
enabled = false
channel_status_updates = true
restart_recovery = "last_state"
allowed_command_surfaces = ["channel"]
allowed_channel_types = ["discord", "irc", "matrix", "mattermost", "slack", "telegram"]
token_budget = 50000
cost_budget_usd = 2.50

[goal.verifier]
# Verifier checks are enabled by default. Set this to false only when an
# operator explicitly wants controller completion without a verifier call.
enabled = false
model_provider = ""
# Omit model to use the provider profile's configured model.
# model = "qwen3-coder-plus"
# temperature = 0.0

[agents.researcher.goal]
enabled = true
```

`allowed_command_surfaces` currently gates admission by runtime surface. The
default is `["channel"]` because channel ingress is the only trusted goal
command surface wired in this branch slice; `web` and `tui` are reserved for
future parser/admission wiring.
`allowed_channel_types` applies when the surface is `channel`; values are bare
channel types such as `matrix` or `telegram`, not aliases like
`matrix.default`. The default includes in-tree channels that expose a canonical
self-addressed mention for the transitional shared command-ingress path.

`channel_status_updates` controls extra in-channel goal state/status messages
emitted by goal admission inside an agent turn, such as a model-initiated
`goal_start`, controller transitions, and verifier progress. When the channel
supports draft edits, verifier progress is shown as a draft and replaced by the
final goal status. Direct `/goal` command replies are still sent as command
responses. Controller-owned goal messages use a short status marker and include
budget usage when accounting is available, so they are distinguishable from
ordinary model-generated replies.

`restart_recovery` controls prior-boot goals that were `running` when the
daemon stopped. The default, `last_state`, keeps the goal running and re-owns
it under the new daemon boot. When the channel and continuation context are
available, recovery queues a trusted continuation prompt built from the durable
objective and stored route/principal context. Set `paused` to pause such goals
with a typed restart blocker instead. Paused goals keep their existing paused
state and blockers.

Budget resolution is per dimension: explicit command value, then `[goal]`
default, then unlimited. Configured budget defaults apply only when a goal is
created. `/goal budget` accepts one or both budget dimensions and mutates the
active goal's persisted effective limits. Consumed and remaining usage are
still derived from the usage ledger. If a budget-paused goal becomes eligible
after a budget update, the controller resumes it through the same trusted
continuation path instead of only flipping its lifecycle state.

The model-facing `goal_start`, `goal_objective`, and `goal_resume` tools are
registered only for tool loops that have trusted goal admission context.
General CLI, gateway, and tool-listing registries do not advertise them yet. If
those surfaces grow a trusted admission context later, they can opt in
explicitly.

## Lifecycle

A goal starts as a running task. The controller can move it through:

- `running`: the goal is active.
- `paused`: work is durable but waiting for user input, human escalation,
  an external dependency, provider recovery, verifier feedback, or restart
  handling.
- `cancelled`, `completed`, `failed`, `lost`, or `timed_out`: terminal states.

Only one non-terminal goal may be active for the same agent, route, and
principal at a time. The control-plane store enforces this in SQLite, so a
race between two starts cannot create two active goals for the same context.

## Trusted Context

Goal objectives, objective amendments, resume reasons, blocker messages, and verifier notes are
untrusted text. The runtime supplies trusted facts:

- agent alias
- originator route
- principal id
- owner process and boot id
- lifecycle status

Neither slash-command arguments nor model tool arguments may set those fields.
The `goal_start`, `goal_objective`, and `goal_resume` model tools accept only
untrusted text inputs; the runtime binds them to the current trusted agent,
route, and principal. This is why goal admission goes through one Rust
controller instead of separate surface-specific start paths.

## Usage Attribution

When a turn runs under goal context, model usage is attributed to the active
goal by resolving the canonical task record at recording time. Synchronous
delegation and verifier calls inherit that attribution. Background delegation
is rejected while a goal is active until parent-linked completion and usage
reporting exist.

Goal usage summaries are derived from task-attributed cost ledger rows whose
task attribution key matches the canonical goal task id. The JSONL
compatibility key may still serialize as `goal_task_id`, but the runtime model
is task-generic attribution. The goal task row stores effective limits, not
consumed or remaining usage totals.

## Verifier

The optional completion verifier is configured globally under
`[goal.verifier]`. When enabled, it asks a model to decide whether a candidate
completion is really complete or should block the goal. A blocked verifier
decision pauses the goal with a verifier blocker.

If `goal.verifier.model_provider` is empty, the verifier uses the owning
agent's configured model provider. If set, it must be a configured
`<provider_type>.<alias>` model provider reference. The verifier may also set a
non-empty model override and temperature.

Verifier model usage is charged to the goal when goal context can be resolved
from the canonical task record.

## Operational Notes

Goal mode requires the control plane to be initialized. If the control plane is
unavailable, `/goal` commands fail instead of creating prompt-only state.

Cancellation is explicit. A final-looking assistant message or a lack of more
tool calls is not enough to mark a durable goal complete.

For implementation rationale, see
[`ADR-008`](../architecture/decisions/ADR-008-goal-mode-control-plane-and-usage-accounting.md).
