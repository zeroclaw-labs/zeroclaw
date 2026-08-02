# Multi-agent setup walkthrough

This is the operator-side companion to the [Agents section](../agents/overview.md). Follow it to add a second agent to an install, configure cross-agent memory access, and put both agents in a peer group on the same channel.

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

> The `zeroclaw agents` lifecycle commands perform the full owned-state cascade only in builds with `gateway` and `agent-runtime` enabled, including the standard distributed binary. A reduced-feature CLI still changes config but prints that owned state was not cascaded. Use the gateway dashboard or a binary with both features enabled for the operations below when owned state exists.

## Rename an agent

Use the rename control for the agent under **Config > Agents** in the gateway dashboard, or run:

```sh
zeroclaw agents rename researcher analyst
```

Both surfaces rewrite references to the alias, persist the config, move the default per-alias workspace, and re-point owned memory, cron, ACP, and session state. Custom workspace paths do not move because they are not derived from the alias. The reserved `default` alias cannot be renamed from or to.

Read any warnings in the response. The config rename commits before workspace and owned-state migration, so warnings identify a side effect that still needs attention. The same gateway API rename request can be reissued to retry residue left under the old alias.

## Delete an agent

Use the delete control under **Config > Agents**, or preview and apply the CLI operation:

```sh
zeroclaw agents delete researcher --dry-run
zeroclaw agents delete researcher --yes
```

1. Review the impact preview and clear every blocker it reports. Common config blockers are an enabled heartbeat owned by the agent and an enabled channel binding that no other enabled agent owns. The preview also lists soft references that the cascade will remove automatically.
2. End any live ACP sessions. The dashboard includes them in its preview; the CLI verifies them when `--yes` executes, after the config-only `--dry-run` preview.
3. Confirm the dashboard deletion or run the CLI command with `--yes`. The operation removes the agent and soft references from config first, then runs the owned-state cascade.
4. Inspect `<data_dir>/agents/_deleted/<alias>-<timestamp>/` and the gateway logs before relying on the archive or cleanup result.

The owned-state cascade attempts to:

- move the configured workspace into the deletion archive;
- write exported memory, cron, and ACP data under `cascade/`;
- purge the agent's memory rows and cron jobs;
- remove its non-live ACP sessions;
- clear agent attribution from retained conversation sessions; and
- write `manifest.json` with counts and surfaced warnings.

These side effects are best-effort. An export or archive-file write can fail while later cleanup continues. Verify the applicable `workspace/`, `cascade/*.json`, and `manifest.json` entries instead of assuming the archive is complete. The CLI prints surfaced cascade warnings. The delete API also returns them, but the dashboard does not currently display them; dashboard operators must check the gateway logs as well.

> Do not replace this flow with direct TOML edits, `zeroclaw config set`, manual workspace deletion, or SQL deletion. Those paths do not run the gateway's reference and owned-state cascade.

There is no automated restore command. Keep the deletion archive until you no longer need it for inspection or manual recovery.

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
