# Running agents

Because there is no privileged "the agent," every command that drives an agent
names which one. Agents coexist; you address one by its alias.

## Addressing an agent

On the CLI, the agent alias is required, there is no default agent:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw agent -a <alias> -m "hello"
```

</div>

The alias is the `<alias>` half of an `[agents.<alias>]` block. For the full CLI
surface and every flag, see the generated
[CLI reference](../reference/cli.md).

## Coexistence and isolation

Agents run side by side from one install. Each one keeps its own workspace,
memory, and identity (see [Filesystem components](./filesystem.md)), so by
default nothing one agent does leaks into another. They share only what their
config references share, a provider, a channel, a bundle, and they can address
each other only where they share a [peer group](../channels/peer-groups.md).

When an agent needs a one-off helper, it spawns an ephemeral
[SubAgent](../architecture/subagents.md) that inherits its identity and security
policy for a single task, then disappears.

## Agents in zerocode

[zerocode](../zerocode/overview.md) is the terminal UI for driving agents. Two
panes put an agent in front of you:

- The **Code** pane runs an agent against your working tree for coding tasks.
- The **Chat** pane is a conversational view of an agent.

Both panes drive a specific agent, and zerocode can give each agent its own
colour palette so you can tell them apart at a glance, see
[Per-agent themes](../zerocode/themes.md#per-agent-themes-code--chat-panes). The
**Config** pane is the preferred place to add and wire agents without editing
files by hand.

## Operating multiple agents at once

`zeroclaw daemon` brings up every enabled agent together, each answering on its
own channels. Adding an agent is additive: define a new `[agents.<alias>]`
block, wire its references, and it joins the running set, the existing agents
are untouched.

For the runtime internals, the permission model, the memory model, and the
agent loop, see [Architecture → Multi-agent runtime](../architecture/multi-agent.md).
