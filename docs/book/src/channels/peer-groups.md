# Peer Groups

A **peer group** declares who an agent accepts inbound messages from on a
channel, and which other agents it can exchange messages with there. It is the
inbound gate for chat channels and the routing primitive for cross-agent
dispatch. In config it lives at `[peer_groups.<name>]`. For how peer groups fit
into an agent's wiring, see [Agents](../agents/overview.md).

Inbound senders are gated against the **peer set** resolved for the bound
agent, drawn from every `[peer_groups.<name>]` block the agent belongs to.
Matching strips a leading `@` and is case-insensitive against the channel's
native sender identifier. An **empty** set denies everyone; a set containing
`"*"` accepts anyone; otherwise only the listed `external_peers` (and peer
agents) are accepted. This is separate from gateway pairing
(`[gateway] require_pairing`), which authenticates HTTP/WebSocket clients, not
chat-channel senders.

## Fields

A `[peer_groups.<name>]` block carries:

| Field | Meaning |
|---|---|
| `channel` | A channel type (`"telegram"`, applies to every alias of that type) or a dotted alias (`"telegram.work"`, scopes to that one instance). |
| `agents` | Member agents by alias. Two agents are peers only when both appear in the same group; membership is mutual. |
| `external_peers` | Non-agent members by the channel's native username/ID. `["*"]` accepts anyone; empty accepts no one. |
| `ignore` | Per-group blocklist; subtracts from the resolved peer set. |
| `output_modality` | Preferred reply modality for the group: `mirror` (input-driven, default), `voice` (always reply and deliver proactive messages as TTS notes on audio-capable channels), or `text` (always text). |
| `admin_for_agent_scope` | When `true`, the group's `external_peers` are authorized to issue `/model --agent <model>` on the bound agent. Default `false` (deny-by-default). See [Admin agent-scope authorization](#admin-agent-scope-authorization). |

## Resolution

For a given agent, the runtime walks every group the agent appears in, unions
the other members' aliases (as agent peers) and the group's `external_peers` on
the group's channel, then subtracts the `ignore` list. The agent's own alias is
removed defensively to avoid a self-loop. An agent on no peer group runs solo
with no cross-agent dispatch.

The sender identifier each channel matches against differs by platform (a
Telegram user ID, a Matrix `@user:server`, an E.164 number, a UUID, …). Each
channel page states the identifier shape it expects.

## Example

{{#peer-group-example discord}}

Each channel page shows the directive form with that channel's sender-identifier
shape.

## Admin agent-scope authorization

`admin_for_agent_scope = true` extends the group's privilege boundary: in
addition to being a routable peer, each `external_peers` member is allowed
to issue `/model --agent <model>` on the bound agent, i.e. switch the
agent's *binding* to a model other than the default. The flag is
deny-by-default: every group without it explicitly set to `true` denies the
capability, including groups with non-empty `external_peers`. `/model
--user <model>` (session-only override, no agent re-binding) is **not**
gated by this flag and is available to every accepted peer.

The orchestrator resolves the authorized admin set live from
`Config::channel_agent_scope_admins`, but the dispatch gate reads through
the **config snapshot** the runtime was started with. Operator edits to
`admin_for_agent_scope` (or to a group's `external_peers` / `channel` /
`agents`) therefore take effect on the **next daemon restart**, not on
the running process. Flipping the flag on a live daemon will not, on its
own, authorize any new senders within the current session.

This is deliberate: authorization is computed against the
immutable-on-startup config, so a peer who is *added* to an admin group
mid-flight cannot escalate within the current process lifetime. The set
is re-resolved only on a full config reload, which is itself a restart
path. If a `/model --agent` invocation reports "not authorized" for a
sender you expected to be in scope, restart the daemon after editing
`admin_for_agent_scope` and re-issue the command from a fresh client
session before drawing further conclusions.
