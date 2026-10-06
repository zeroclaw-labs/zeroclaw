# Colony

Colony is a reusable Queen-led team in the Agent workspace. Switch the left
agent panel to **Colony**, select independent agents on the canvas, and give
them a goal. Setup groups the Queen's questions, prior context, access profiles,
autonomy and start settings into a side panel. The default is supervised work
with review before Start.

This implementation is stacked on the workspace navigation rework in
[PR #11414](https://github.com/zeroclaw-labs/zeroclaw/pull/11414).

## Owners and persistence

| Fact | Canonical owner |
| --- | --- |
| Agent purpose | `agents.<alias>.core_command` |
| Team, Queen, directed grants, rooms, channel scopes, prompt order and canvas positions | `Config.colonies` in `zeroclaw-config` |
| Tools, files, network, approvals and execution limits | Existing risk/runtime profiles and tool configuration |
| Goal lifecycle, child tasks and blockers | Existing task/goal control plane |
| Goal consumption | Existing cost ledger, attributed by task ID |
| Uncertain consumption | Goal-linked usage gap; blocks limited goals until consumption is known |
| Settled assignment position and messages | `zeroclaw-colony` extensions in the control-plane database |
| Pending tool approval and its one-time decision | Colony approval records, consumed by the live owning turn |
| Selected prior context | References to existing memory/session owners, resolved when used |

`zeroclaw-colony` owns the controller and its SQLite extension. The transitional
runtime crate provides integration at its existing execution, approval and
control-plane boundaries. It does not own a second Colony controller. The
generic execution scope carries trusted attribution and budget callbacks;
model text cannot create that scope.

Configuration changes use the shared writer and authorize the complete write
set. Membership changes reserve agent admission through durable publication,
reject active/prepared work, and retire stale producers. Existing idle chats
may need to reconnect after an agent joins or leaves a colony. A failed save
does not advance their generation.

Remove an agent from its colony before deleting or renaming its definition.
Disconnect a channel node before deleting or renaming that configured channel.
Cancel the colony's current goal before deleting its owner, including a paused
goal. Disabling an agent prevents its next Colony turn; an admitted turn can
settle normally.

## Communication

An agent belongs to at most one colony. A directed wire grants permission to
contact its target and describes that collaborator's public purpose. It does
not create a workflow step. A reply or delegated result needs the reverse
grant, which is checked again before delivery. Agents outside the box cannot
be reached through ambient delegation or peer groups.

During an active goal, `send_message_to_peer` can publish through a one-way
wire. The recipient sees the addressed message on its next eligible turn;
publication does not start detached work or grant a reply. Synchronous
delegation requires both the request and return directions.

The Queen's plan names actual configured specialists and a bounded sequence
of assignments. Each settled output returns through its permitted direction
to the Queen; later assignments receive permitted settled context. Worker
turns use the real agent runner, profiles and tool approval gate. Detached
work paths without durable goal ownership are closed for colony agents.

The Queen reviews settled work before finishing. If more work or specialists
are needed, a supervised team pauses for one consolidated plan review.
Missing user facts produce grouped questions. Autonomous teams can adopt a
valid plan within their existing settings. Planning is bounded to eight
refinement rounds, 64 total assignments and eight proposed specialists.
Assignments settle sequentially in this version.

Rooms have separate read and post lists. Addressed mode wakes only mentioned
eligible members, Queen-selected mode chooses an eligible responder, and
open mode has a configured response bound. The human can talk directly to
the Queen, any member, or a room. Busy members serialize their turns; paused
goals retain queued messages.

Agents use `colony_room` to read or post under their live room grants.
Posting preserves the message without automatically waking other agents.

External nodes reference existing channels and an exact conversation/recipient.
Inbound and outbound grants are independent. They drive scoped routing and
delivery while preserving the existing channel allowlists and tool approvals.
Unsupported unscoped communication tools fail closed for colony agents.
Network/file tools retain their configured access; an agent wire grants
neither filesystem nor memory access.

An active goal queues external messages into the same owned inbox. The current
channel turn waits for a related reply for up to 15 minutes and stops waiting
on Pause or Cancel. A restart retains the reply in Colony history without
replaying an external send whose delivery outcome is uncertain.

## Instructions and context

Each admitted run captures its ordered recurring prompt bindings. **Next
admitted run** edits update canonical content for later admissions while
existing runs retain their captured content. **Apply at the next safe point**
advances the colony instruction epoch so active loops refresh their bindings
before another provider request. Collaborator descriptions and communication
policy are always resolved live. Instructions are included once in each
provider request, without accumulating copies in conversation history.

Setup previews memory and conversations and lets the user select references.
The Colony runner suppresses automatic recall, history restoration, persona
bootstrap and memory writes. Only selected baseline context is loaded.
Conversation sources must belong to the selected agent and the native shared
operator (or legacy unscoped owner); another principal's source key is denied.
Hidden reasoning and system messages are excluded from selected transcripts.

**Continue** includes eligible prior goal conversation. **Start fresh** keeps
the team, wires and approved baseline references, excludes earlier goal context,
and preserves the earlier history for the human to inspect.

## Controls and recovery

- **Plan only** allows clarification and proposals without goal worker execution.
- **Supervised** starts reviewed work and requires approval of new specialists.
- **Autonomous** can add proposed specialists from approved team templates,
  with explicit connection directions and existing access limits.
- **Pause** blocks new goal work and retains a resumable checkpoint. A live
  approval wait can remain attached while paused.
- **Cancel goal** terminates owned work and keeps the reusable team.

The controller records a child/checkpoint before starting a turn and advances
only after its output settles. A restart at a settled boundary can resume
autonomous work. Supervised recovery waits. An interrupted child, explicit
pause/cancel, changed permissions, approval or exhausted budget prevents
automatic replay. Review an uncertain turn's effects before choosing Retry
or Skip. A restart cannot reuse a one-time approval for a lost live turn.

## Testing and rollback

Use a testing configuration with native gateway pairing and at least two
configured agents/providers. Build the dashboard with Node 24 LTS:

```sh
cargo web install
cargo web build
cargo run -- daemon
```

Open the dashboard URL printed by the daemon and pair it. In Agent, select
Colony, drag a rectangle around agents and create a team. Enter a goal, answer
the Queen's grouped questions, review context/access and Start. Verify Pause,
Resume, Cancel and Continue/Fresh. Draw a single direction and verify that it
does not authorize a reply. Edit a prompt during work and choose its timing.
An existing tool approval must appear for review rather than being silently
approved by the Queen.

Active goal turns expose exact, one-time approval cards. Direct conversations
outside an active goal preserve the existing noninteractive approval denial.
Goal token/cost caps require durable usage. Missing usage or pricing blocks a
limited goal, including after restart, rather than assuming zero consumption.
Caps stop subsequent admissions and tool effects; an already admitted provider
request can consume more than the remaining budget.
Goal caps cover admitted goal turns. Setup clarification uses the existing
agent/profile limits before a goal record is created.

The browser smoke uses synthetic HTTP/model fixtures with the actual dashboard
renderer. Runtime/controller tests cover durable admission and policy boundaries;
live provider/channel credentials are not required by those fixtures. Saved
teams are local to one installation. Remote/shared agents, nested colonies and
collaborative canvas editing are outside this version.

See the [browser evidence and reproduction steps](../assets/colony/README.md)
for desktop graph, review and mobile captures.

For rollback, pause/cancel goals, remove colonies through the API after their
work settles, and return to the previous binary/branch. Keep backups of config
and the task database. Additive history tables can remain; an older binary
does not execute Colony continuations. Reverting code alone does not remove
saved membership or selected context references.
