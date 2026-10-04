# Peer Groups

A **peer group** declares who an agent accepts inbound messages from on a
channel, and which other agents it can exchange messages with there. It is the
inbound gate for chat channels and the routing primitive for cross-agent
dispatch. In config it lives at `[peer_groups.<name>]`. For how peer groups fit
into an agent's wiring, see [Agents](../agents/overview.md).

Inbound senders are gated against the **peer set** resolved for the channel
instance, drawn from every `[peer_groups.<name>]` block whose `channel` matches
either the channel type or its dotted alias.
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
| `ignore` | Per-group blocklist; travels with the resolved peer set and overrides any grant, including a wildcard. Applied by the channel, under that channel's identity rules. |
| `output_modality` | Preferred reply modality for the group: `mirror` (input-driven, default), `voice` (always reply and deliver proactive messages as TTS notes on audio-capable channels), or `text` (always text). |
| `admin_for_agent_scope` | When `true`, the group's `external_peers` are authorized to issue `/model --agent <model>` on the bound agent. Default `false` (deny-by-default). See [Admin agent-scope authorization](#admin-agent-scope-authorization). |

## Resolution

External sender authorization is channel-scoped: the runtime unions
`external_peers` from every group matching the channel type or dotted alias,
and carries every matching group's `ignore` list alongside it as reserved
`!name` entries. Both halves reach the channel, which applies denies before
grants.

**The deny is applied by the channel, not by the resolver.** Whether two
entries name the same account is a question only the channel can answer:
Reddit reads `u/alice` and `alice` as one user, WhatsApp reads `+1555…` and
`1555…` as one number, Nostr rewrites an npub to hex. A resolver that removed
ignored entries itself had to guess at that, and a guess that came out wrong
dropped the deny before the channel could apply it, admitting a sender the
operator had written down. So `ignore` is not subtracted anywhere except in the
channel that understands the identity.

`ignore` wins over a grant, including a wildcard. With `external_peers = ["*"]`
on one matching group and `ignore = ["alice"]` on another, everyone except
`alice` is authorized. `ignore = ["*"]` is the mirror image and denies every
sender. A deny matches whatever the same string would have granted on that
channel, and additionally ignores case and a leading `@`, because a blocklist
should err toward denying. On a channel that accepts several identifiers for one
account (Bluesky takes a handle or a DID), ignoring any one of them denies the
account.

A wildcard is recognized with surrounding whitespace trimmed, so `[" * "]` and
`["*"]` mean the same thing to both halves of the rule.

`!` therefore opens a deny marker in the resolved list. An identity may legally
begin with `!`, since RFC 5322 allows it in an email local-part, so the *count*
of leading markers carries the decision rather than merely its presence. An
identity opening with `k` markers is emitted with `2k` as a grant and `2k + 1`
as a deny, so an even run is a grant, an odd run is a deny, and halving the run
recovers the identity:

| identity | as `external_peers` | as `ignore` |
| --- | --- | --- |
| `alice` | `alice` | `!alice` |
| `!user@example.com` | `!!user@example.com` | `!!!user@example.com` |

An identity that does not begin with `!` encodes exactly as it always did, so no
existing config reads differently. Nothing an operator can write is silently
inverted in either direction. You write the address as it is; the doubling is
internal to the resolved list.

A channel written more than one way in `channel` (WeCom WebSocket answers to
both `wecom-ws` and `wecom_ws`) resolves as one set. Resolving each spelling on
its own and joining the results would leave a wildcard under one spelling and an
`ignore` under the other unaware of each other. Every startup path must use the
same resolution: the daemon's normal channel construction and the one-shot
single-channel path both go through one named helper for exactly that reason.

Because the deny travels inside the resolved list, a channel authorizes a sender
through `allowlist::is_user_allowed`, `is_user_allowed_by` or, where a sender has
several identifiers, `is_identity_allowed`. Testing the list for `"*"` directly,
or asking one identifier at a time, admits a sender the operator ignored.

The resolved list is an authorization input, not a list of addresses: it holds
wildcards and deny markers. Code that needs somewhere to send a message asks
`Config::channel_addressable_peers`, which returns only entries naming one
reachable account.

For the same reason, "is the list empty" is not "has anybody been authorized". A
group with `ignore` and no `external_peers` resolves to a non-empty list that
grants no one, so a channel deciding whether it is still unpaired asks
`allowlist::grants_anyone` rather than testing the list for emptiness. A grant
its own `ignore` cancels does not count either: `external_peers = ["alice"]`
with `ignore = ["alice"]`, or `["*"]` with `ignore = ["*"]`, authorizes nobody,
so the channels that gate pairing on that answer keep offering their bind code.

Pairing then writes into `external_peers`, which means the same `ignore` can
shadow what pairing just persisted. Rather than report a bind that cannot work,
the write is refused and the operator is told to remove the `ignore` entry
first. An explicit blocklist entry stays authoritative over a pairing attempt.
This holds for every writer: an in-channel `/bind` exchange, `zeroclaw channel
bind-telegram`, and `POST /api/channels/bind` all refuse it, and the refusal
covers an identity already listed as a grant, because a grant its own `ignore`
shadows is not a usable binding either.

Cross-agent routing is agent-scoped: for a given agent, the runtime walks every
group the agent appears in, unions the other members' aliases on the group's
channel, then subtracts `ignore`. The agent's own alias is removed defensively
to avoid a self-loop. An agent on no peer group runs solo with no cross-agent
dispatch.

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
