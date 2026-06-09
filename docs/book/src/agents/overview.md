# Agents

Agents are the star of a ZeroClaw deployment. Everything else in this book, the
providers, the channels, the security profiles, the skills, the memory, exists
so that an agent can use it. This section is the showcase; the rest of the docs
are the credits.

{{#include ../_snippets/concept-multi-agent.md}}

## An agent is a join

An agent is not a program you install. It is a named row, `[agents.<alias>]`,
that **joins** two halves:

- **Config references** (the relational side): pointers to things configured
  elsewhere, a model provider, a risk profile, a runtime profile, channels,
  skill / knowledge / MCP bundles, cron jobs. Each is a dotted alias. The agent
  owns none of these; it points at them, and many agents can point at the same
  one or diverge freely.
- **Filesystem components** (the on-disk side): a per-agent workspace directory,
  a memory backend, and an identity (personality) source. This is where the
  relational graph meets a concrete directory tree.

```text
  Config references (relational)              Filesystem (on-disk)
  ──────────────────────────────             ──────────────────────
  - model provider                           - workspace/
  - risk profile                             - memory store
  - runtime profile          agents.<alias>  - identity / personality
  - channels             ──▶  (the join)  ◀──
  - peer groups
  - skill / knowledge / MCP bundles
  - cron jobs
```

The agent points at the references on the left, owning none of them: many agents
may share one or diverge freely. It owns the filesystem half on the right.

Each reference is a link back to its own section, the credits: model providers
live in [Model Providers](../providers/overview.md), the profiles in
[Security & Autonomy](../security/autonomy.md), channels in
[Channels](../channels/overview.md), peer groups in
[Peer Groups](../channels/peer-groups.md), bundles in [Tools](../tools/overview.md).

## Multi-agent from the jump

There is no privileged "the agent." The runtime holds a map of agents keyed by
alias; a single-agent install is just a map of size one. You do not start with
one bot and bolt on more later, you add agents and wire each one, and they
coexist from the first line of config.

Because each agent joins its own references and its own filesystem, agents can
share some axes and diverge on others independently. Two agents might share one
model provider but run under different risk profiles, answer on different
channels, and keep entirely separate memory.

Agents reach each other two ways, each gated separately: they can **message**
on a channel where they share a [peer group](../channels/peer-groups.md), and
they can **delegate** a task to one another only when the caller's risk profile
permits delegation and both agents share the same risk profile (see
[Running agents](./operating.md#coexistence-and-isolation)).

```text
                    agents.researcher          agents.support
                    ─────────────────          ──────────────
  model provider     openrouter.prod ◀───────── openrouter.prod   (same one)
  risk profile       hardened                   permissive        (diverge)
  channel            discord.main               slack.helpdesk    (diverge)
  peers              └──────▶ peer group on discord.main ◀───────┘
```

Two agents share one model provider, run under different risk profiles, answer
on different channels, and meet only where they share a peer group.

A SubAgent is the short-lived exception to coexistence: an agent can spawn an
ephemeral SubAgent that inherits the parent's identity and security policy for a
single task. See [SubAgents](./delegation.md).

## Where to go next

- [Anatomy of an agent](./anatomy.md): every field on `[agents.<alias>]`, and
  what each reference points at.
- [Filesystem components](./filesystem.md): the workspace, memory, and identity
  that live on disk per agent.
- [Running agents](./operating.md): addressing agents, coexistence, and how an
  agent surfaces in the zerocode Code and Chat panes.

For the runtime internals, the permission model, the memory model, and the
agent loop, see [Runtime internals](./internals.md).
