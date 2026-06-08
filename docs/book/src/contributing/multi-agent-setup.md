# Multi-agent setup walkthrough

This is the operator-side companion to the [multi-agent architecture page](../architecture/multi-agent.md). Follow it to add a second agent to an install, configure cross-agent memory access, and put both agents in a peer group on the same channel.

Background: each agent has its own workspace dir at `<install>/agents/<alias>/workspace/`, picks one memory backend at creation (immutable), and is gated by a `[risk_profiles.<profile>]` entry.

Throughout this walkthrough the existing single agent is called `primary` (substitute whatever your install actually uses) and the new agent being added is `researcher`.

## Prerequisites

- A configured `[agents.primary]` entry with a working `model_provider`, `risk_profile`, and at least one channel binding.
- A `[risk_profiles.<name>]` entry the new agent will inherit. Reusing `primary`'s profile is fine for most uses; pick a stricter alias (e.g. `hardened`) if the new agent has a different trust surface.

## Add a second agent

Add another agent through the gateway dashboard, zerocode, or `zeroclaw config set`. The runtime creates `<install>/agents/<alias>/workspace/` on first agent-loop entry. On every start the agent loop injects the workspace identity files that exist into the system prompt: `AGENTS.md`, `SOUL.md`, `TOOLS.md`, `IDENTITY.md`, `USER.md`, then `BOOTSTRAP.md` (first run only) and `MEMORY.md` (main session only). `HEARTBEAT.md` is also a workspace personality file but it is read by the heartbeat engine, not injected into the prompt. The dashboard's personality editor exposes `SOUL.md`, `IDENTITY.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, `HEARTBEAT.md`, and `MEMORY.md` for editing. Create and edit those files to give the agent its persona. (`BOOTSTRAP.md` is a first-run scaffold the agent reads once and removes; the editor does not expose it.)

{{#config-where agents}}

## Bind a channel

Without a channel the agent has nowhere to listen. Bind one via the agent's `channels` list, then restart the daemon. The agent picks up its channel on next start.

## Cross-agent file access

By default, an agent can only read and write within its own workspace dir. You can grant one agent read or write access into another agent's workspace (configured via the gateway, zerocode, or `zeroclaw config set`). Effective behavior, e.g. `researcher` granted write to `primary` and read to `archivist`:

- `file_read` from `researcher` can read both `<install>/agents/primary/workspace/` and `<install>/agents/archivist/workspace/`.
- `file_write` and `file_edit` from `researcher` can write into `<install>/agents/primary/workspace/` but **not** `<install>/agents/archivist/workspace/`.

POSIX device files (`/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`) are always readable, no per-agent config needed.

## Cross-agent memory access

Same-backend only. To let `researcher` recall memories that `primary` wrote, both agents must use the same memory backend (e.g. both `sqlite`). The schema validator rejects entries that point at a sibling on a different backend; the runtime never sees a cross-backend allowlist by the time it builds the per-agent memory wrapper.

The bound agent always sees its own rows; the allowlist is purely additive. There is no way to *hide* an agent's own rows from itself.

## Peer group on a shared channel

Two agents become "peers" (each can address the other on a channel) only when **both** appear in the same peer group. See [Peer Groups](../channels/peer-groups.md).

`external_peers` lists humans or external bots the group expects on the same channel; the runtime accepts inbound from those usernames as cross-agent traffic. `ignore` is a per-group blocklist that subtracts from the resolved peer set every member sees, useful for excluding a specific bot account that's noisy.

The schema validator at config load enforces:

1. Every member's `channels` list includes the group's `channel` (an agent that doesn't listen there can't peer there).
2. Every member is a configured agent (no dangling references).
3. `read_memory_from` does not point at the agent itself.

## Inspect the install

Every configured agent lives under an `agents.<alias>` entry with its risk profile, model provider, memory backend, and channel set.

{{#config-where agents}}

## Delete an agent

1. Remove the `agents.<alias>` entry (and any nested `workspace` / `memory` tables) through the gateway, zerocode, or `zeroclaw config set`.
2. Strip the alias from every `[peer_groups.<name>]` block's `agents` list.
3. Remove the workspace dir: `rm -rf <install>/agents/<alias>/workspace/`.
4. Optional cleanup of the agent's memory rows (they retain `agent_id = <alias-uuid>` attribution but no live agent maps to that UUID anymore):

```sql
DELETE FROM memories WHERE agent_id = (SELECT id FROM agents WHERE alias = 'researcher');
DELETE FROM agents WHERE alias = 'researcher';
```

The schema validator will refuse to load if a `[peer_groups.<name>]` still lists the deleted alias, so step 2 is required before the daemon will start cleanly.

## Verify

Look at the merged log stream; every line should now carry `[<alias>]` or `[system]` prefixes:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw daemon 2>&1 | grep '\[researcher\]'   # researcher's lines only
zeroclaw daemon 2>&1 | grep '\[system\]'       # boot/migration/scheduler lines only
```

</div>

If the boundary checks are working, `file_read /dev/null` from any agent succeeds (POSIX device-file allowlist), `file_read` outside the workspace + access list fails with `Path escapes workspace directory`, and `file_write` to a read-only allowlisted sibling fails with the same message.
