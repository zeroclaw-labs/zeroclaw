# Peer Groups

A **peer group** declares who an agent accepts inbound messages from on a
channel, and which other agents it can exchange messages with there. It is the
inbound gate for chat channels and the routing primitive for cross-agent
dispatch. In config it lives at `[peer_groups.<name>]`.

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
