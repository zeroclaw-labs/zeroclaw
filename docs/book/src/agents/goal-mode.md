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
/goal start <objective>
/goal status [task_id]
/goal pause [reason]
/goal resume [task_id]
/goal cancel [task_id]
```

`status`, `pause`, `resume`, and `cancel` use the active goal for the same
agent, route, and principal when no `task_id` is provided. Passing a task id is
allowed only when that goal is visible to the same trusted context.

The model-facing `goal_start` tool is registered only for tool loops that have
trusted goal admission context. General CLI, gateway, and tool-listing
registries do not advertise it yet. If those surfaces grow a trusted admission
context later, they can opt in explicitly.

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

Goal objectives, resume answers, blocker messages, and verifier notes are
untrusted text. The runtime supplies trusted facts:

- agent alias
- originator route
- principal id
- owner process and boot id
- lifecycle status

Neither slash-command arguments nor model tool arguments may set those fields.
This is why goal admission goes through one Rust controller instead of separate
surface-specific start paths.

## Usage Attribution

When a turn runs under goal context, model usage is attributed to the active
goal by resolving the canonical task record at recording time. Synchronous
delegation and verifier calls inherit that attribution. Background delegation
is rejected while a goal is active until parent-linked completion and usage
reporting exist.

Goal usage summaries are derived from ledger rows whose `goal_task_id` matches
the goal task id. The goal task row stores effective limits, not consumed or
remaining usage totals.

## Verifier

The optional completion verifier is configured globally under
`[goal.verifier]`. When enabled, it asks a model to decide whether a candidate
completion is really complete or should block the goal. A blocked verifier
decision pauses the goal with a verifier blocker.

If `goal.verifier.model_provider` is empty, the verifier uses the owning
agent's configured model provider. If set, it must be a configured
`<provider_type>.<alias>` model provider reference. The verifier may also set a
model override and temperature.

Verifier model usage is charged to the goal when goal context can be resolved
from the canonical task record.

## Operational Notes

Goal mode requires the control plane to be initialized. If the control plane is
unavailable, `/goal` commands fail instead of creating prompt-only state.

Cancellation is explicit. A final-looking assistant message or a lack of more
tool calls is not enough to mark a durable goal complete.

For implementation rationale, see
[`ADR-008`](../../../architecture/decisions/ADR-008-goal-mode-control-plane-and-usage-accounting.md).
