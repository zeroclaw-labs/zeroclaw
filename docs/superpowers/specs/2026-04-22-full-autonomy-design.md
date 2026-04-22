# Full Autonomy for Adi Personas — Design Spec

**Date:** 2026-04-22
**Status:** Draft for review
**Applies to:** `adi-zeroclaw-shane`, `adi-zeroclaw-meg` (Fly.io `iad`, org `personal`)
**Repo:** `c:\git\adi` on `deploy/v0.7.3`

## Problem

Both Adi personas currently run at `[autonomy] level = "supervised"`. Under Supervised, Act-tier tool calls go through an approval gate; in a headless Fly deployment with no interactive operator, this effectively blocks the agent from using any tool not in the `auto_approve` list. Notable tools currently gated and therefore unusable: `model_switch`, `model_routing_config`, `shell`, `cloud_ops`, `claude_code`, `mcp_tool`, `http_request`, `memory_purge`, `memory_forget`, `escalate`, `backup_tool`, and Composio tool writes (Todoist).

The requested behavior is **fully autonomous tool use**: both personas should be able to invoke any tool in their surface area without a human approval step, subject to the remaining structural blast doors (channel allowlist, forbidden paths, high-risk command block, rate/cost limits, gateway pairing for the web dashboard).

## Goal

Flip both personas to `[autonomy] level = "full"` with the smallest possible change surface (approach A from the brainstorming session). No Rust code changes, no Dockerfile changes, no fly.toml changes. One line in one file, plus a live-config edit on each existing volume to reflect the change in the running processes.

## Non-goals

- Adding new tools or extending any tool's capability.
- Widening the `allowed_commands` shell list, disabling `block_high_risk_commands`, widening `forbidden_paths`, or raising rate/cost caps. All of these remain at their hardcoded defaults.
- Changing the gateway `require_pairing = true` setting or channel `allowed_users` gates.
- Changing the "seed once, live config is authoritative" invariant of `deploy/zeroclaw/entrypoint.sh`.
- Auditing `PromptGuard`, `LeakDetector`, Supabase backups, or the exact scope of `require_pairing`. These are listed as follow-up work below.

## What `level = "full"` actually does (ground truth from the code)

Verified in `crates/zeroclaw-config/src/policy.rs` and `crates/zeroclaw-config/src/schema.rs`:

**Full unlocks:**
- Every tool call that gates on `enforce_tool_operation(ToolOperation::Act, …)` now passes the autonomy check without approval. This includes `model_switch`, `model_routing_config` (write actions), `shell`, `cloud_ops`, `claude_code`, `mcp_tool`, `http_request`, `memory_purge`, `memory_forget`, `escalate`, `backup_tool`, Composio writes, and similar.
- `workspace_only` is force-disabled in `SecurityPolicy::from_config` when the level is Full (per comment in `policy.rs` referencing upstream issue #5463). Absolute filesystem paths outside the workspace become reachable, subject to `forbidden_paths`.
- The medium-risk shell command approval gate is skipped.

**Full does NOT unlock:**
- `forbidden_paths` — still enforced. Default list includes `/etc`, `/root`, `/home`, `/usr`, `/bin`, `/sbin`, and other system-critical paths.
- `block_high_risk_commands = true` (default) — still blocks `rm`, `curl`, `wget`, and other high-risk commands unless explicitly named in `allowed_commands`.
- `allowed_commands` default (conservative): `git`, `npm`, `cargo`, `ls`, `cat`, `grep`, `find`, `echo`, `pwd`, `wc`, `head`, `tail`, `date`, `python`, `python3`, `pip`, `node`. No `rm`, no `curl`, no `wget`, no `ssh`, no `docker`, no `fly`.
- `max_actions_per_hour = 100` (default) — sliding-window rate limit.
- `max_cost_per_day_cents = 1000` (default) — $10/day cost ceiling.
- Gateway `require_pairing = true` — continues to gate the web dashboard.
- Channel `allowed_users` — messages from user IDs not in the list are dropped at the channel layer before reaching the agent.
- The `auto_approve` list becomes vestigial at Full (approval gate is skipped for all Act ops, so the list has no additional effect).

## Change surface

**One file, one line:** `deploy/zeroclaw/config.seed.toml`

```diff
 [autonomy]
-level = "supervised"
+level = "full"
 auto_approve = [
     "file_read", "file_write", "file_edit",
     "memory_recall", "memory_store",
     "web_search_tool", "web_fetch",
     "calculator", "glob_search", "content_search",
     "image_info", "weather", "git_operations"
 ]
```

The `auto_approve` list is retained as-is. It is vestigial at Full but removing it is out of scope for this change.

## Rollout

The seed file is a first-boot template only. On existing volumes (shane and meg), `/zeroclaw-data/.zeroclaw/config.toml` already exists and will not be overwritten by the seed. The seed change alone has no runtime effect on already-running personas. To flip live personas, the config on each volume must be edited in place.

### Order of operations

1. Edit `deploy/zeroclaw/config.seed.toml` (the one-line diff above). Commit on branch `deploy/v0.7.3`.
2. `flyctl deploy --config deploy/zeroclaw/fly.shane.toml --dockerfile deploy/zeroclaw/Dockerfile --app adi-zeroclaw-shane --remote-only` followed by the meg deploy (`--config deploy/zeroclaw/fly.meg.toml --image <shane-image-ref> --app adi-zeroclaw-meg`). Purpose: keep the seed in sync with the intended default for any future fresh volume. No runtime effect on the existing shane or meg volumes (seed does not overwrite live config).
3. Back up and edit shane's live config, then restart the container:
   ```bash
   fly ssh console -a adi-zeroclaw-shane -C "cp /zeroclaw-data/.zeroclaw/config.toml /zeroclaw-data/.zeroclaw/config.toml.bak"
   fly ssh console -a adi-zeroclaw-shane -C "sed -i 's/^level = \"supervised\"/level = \"full\"/' /zeroclaw-data/.zeroclaw/config.toml"
   fly ssh console -a adi-zeroclaw-shane -C "kill 1"
   ```
4. Verify shane (see Verification below).
5. Repeat step 3 for `adi-zeroclaw-meg`.
6. Verify meg.
7. Smoke test both personas by sending a channel message asking each to call `model_switch` and confirming no approval prompt blocks the call.

### Verification (per persona)

```bash
fly ssh console -a adi-zeroclaw-shane -C "grep -A1 '\[autonomy\]' /zeroclaw-data/.zeroclaw/config.toml"
# Expect: level = "full"

curl -s https://adi-zeroclaw-shane.fly.dev/api/health
# Expect: all 8 components ok (channels, channel:slack, channel:telegram, daemon,
#         gateway, heartbeat, mqtt, scheduler)
```

### Rollback

Per-persona flip back to Supervised:

```bash
fly ssh console -a adi-zeroclaw-shane -C "sed -i 's/^level = \"full\"/level = \"supervised\"/' /zeroclaw-data/.zeroclaw/config.toml"
fly ssh console -a adi-zeroclaw-shane -C "kill 1"
```

Hard stop (takes the persona fully offline):

```bash
flyctl scale count 0 -a adi-zeroclaw-shane
```

If the config file ends up corrupted (e.g., `sed` interrupted mid-write), restore from the pre-edit backup:

```bash
fly ssh console -a adi-zeroclaw-shane -C "cp /zeroclaw-data/.zeroclaw/config.toml.bak /zeroclaw-data/.zeroclaw/config.toml"
fly ssh console -a adi-zeroclaw-shane -C "kill 1"
```

## Risks

### Runtime risks (what Full unlocks that could hurt)

- **Composio writes.** Composio is enabled in the seed with per-persona `entity_id`. At Full, the persona can create, modify, or delete Todoist tasks without approval. Blast radius: low (Todoist is user-recoverable).
- **HTTP egress.** The `http_request` tool is Act-tier. If its configured `allowed_domains` is `["*"]` (default per `HttpRequestConfig`), the persona can POST to any URL on the internet. A prompt injection that slipped past `PromptGuard` could cause memory exfiltration.
- **Memory purge and forget.** `memory_purge` and `memory_forget` are Act-tier. A prompt injection could trigger memory wipe. Recovery depends on Supabase backups being enabled (not verified — see follow-up work).
- **Self-modifying config.** `model_routing_config` can rewrite `/zeroclaw-data/.zeroclaw/config.toml`. A bad scenario route or a typoed default model can persist across restarts. The `probe_model` check in `handle_set_default` skips probing when no API key is configured (the case for custom CLIProxy URLs), so it cannot catch all invalid changes. Recovery: `fly ssh` and manually edit the config.
- **Rate limits are per-policy, not per-channel.** `max_actions_per_hour = 100` is a single bucket per persona. A runaway loop on one channel eats into the budget for another.

### Rollout risks

- **`sed -i` on a live TOML file.** Idempotent for this single-line flip, but not atomic. A mid-write failure (disk full or pathological interrupt) could leave the file unparseable and the persona would fail to start on next boot. Mitigation: the `cp … .bak` step above.
- **`kill 1` restart.** Relies on Fly's default restart policy bringing the container back. If it does not, `flyctl apps restart adi-zeroclaw-shane` is the fallback.

## Follow-up work: policy contingencies to enforce later

The items below are the blast doors that Full autonomy now leans on. Each is currently an assumption — "this thing is probably working" — rather than an enforced invariant. The intent of this section is not one-time verification, but **tracking each one as future policy work** that should produce enforcement mechanisms (runtime checks, startup assertions, deploy-time gates, or audit alerts) so the invariant cannot silently drift.

Each item is framed as the policy it should become, not the check it starts as.

### Prompt-injection defenses

**Invariant:** `PromptGuard` and `LeakDetector` run on every inbound channel message and every outbound tool argument, and refusing their default action fails closed.
**Current state:** The modules exist in `crates/zeroclaw-runtime/src/security/`. Whether they are wired into the agent loop for this build, and whether they fail closed vs open, is unverified.
**Policy enforcement direction:** Runtime startup assertion that both are registered, plus a config-validation error if either is disabled on a `full`-autonomy persona.

### Channel-level user allowlist

**Invariant:** Messages from user IDs not in `channels.<name>.allowed_users` never reach the agent loop — they are dropped at the channel layer.
**Current state:** Assumed. `allowed_users` is set on both Telegram and Slack for shane and meg, but there is no test confirming the agent is never even invoked for non-allowlisted senders.
**Policy enforcement direction:** An integration test in `crates/zeroclaw-channels/` that sends a message from a non-allowlisted ID and asserts the agent is never invoked. This matters far more under Full than Supervised, because Full removes the approval gate that would otherwise catch a leak from the channel layer.

### Gateway pairing scope

**Invariant:** `require_pairing = true` gates every externally reachable endpoint that can trigger tool calls or mutate state, not just the dashboard UI.
**Current state:** Commit `444819302` describes it as "for dashboards." The flag name is generic, but the scope of enforcement inside the gateway crate is unverified.
**Policy enforcement direction:** An endpoint-level audit inside `crates/zeroclaw-gateway/` that tags each handler as pairing-required or pairing-exempt, with a compile-time or startup assertion that no tool-invocation handler is exempt.

### Memory-table backups

**Invariant:** `zeroclaw.memories_shane` and `zeroclaw.memories_meg` (and any future per-persona memory tables) have Supabase point-in-time recovery enabled.
**Current state:** Backup configuration not verified. A prompt-injection triggering `memory_purge` under Full would be unrecoverable if backups are off.
**Policy enforcement direction:** A deploy-time check (or scheduled cron against Supabase) that fails if any persona's memory table is missing PITR. Alternatively, remove `memory_purge` from the tool surface on `full`-autonomy personas until this invariant is enforced.

### HTTP egress allowlist

**Invariant:** `http_request` can only POST to a bounded allowlist of domains, not `["*"]`.
**Current state:** Default is `["*"]`. The agent can reach any URL on the public internet at Full.
**Policy enforcement direction:** Config-validation error if `http_request.allowed_domains` is `["*"]` while `autonomy.level == "full"`. Force the operator to make the decision explicitly.

### Self-modifying config bounds

**Invariant:** `model_routing_config` cannot set a provider/model combination that would brick the persona on restart.
**Current state:** `probe_model` in `handle_set_default` ping-tests the new model, but skips when no API key is configured — which is exactly the case for the CLIProxy custom URL.
**Policy enforcement direction:** Extend `probe_model` to probe unauthenticated endpoints (or any configured custom URL) via a cheap HEAD/ping, and fail the config write if the endpoint is unreachable. Alternatively, gate `model_routing_config.set_default` behind a dedicated higher autonomy tier.

### Composio write scope

**Invariant:** Composio tool calls that mutate user data (Todoist create/complete/delete) are distinguishable in the audit log from read-only calls, so a runaway loop is detectable after the fact.
**Current state:** Unverified whether audit log granularity separates reads from writes for Composio.
**Policy enforcement direction:** Audit-event classification on the Composio tool adapter, plus an alerting rule on high-rate write bursts.

### Per-channel action budget

**Invariant:** A runaway loop on one channel cannot consume the rate budget that belongs to another channel for the same persona.
**Current state:** `max_actions_per_hour` is a single sliding-window bucket per policy. Today this is shared across Telegram + Slack + gateway.
**Policy enforcement direction:** Extend the rate-limiter to track per-channel buckets, or at minimum emit a warning audit event when one channel dominates the shared bucket.

## Test plan

There are no automated tests for this change — it is a config-data modification, not a code change. Verification is the smoke test described under Rollout:

- `grep` the live config on each persona after `kill 1`: shows `level = "full"`.
- `/api/health`: 8/8 components ok.
- Channel smoke test: ask each persona via Telegram or Slack to call `model_switch` (e.g., list providers). Before: the call is blocked by Supervised. After: the call succeeds.
- Audit log inspection (via `fly logs -a adi-zeroclaw-shane`): confirm no "Security policy: … approval required" lines for the `model_switch` call.

## References

- `crates/zeroclaw-config/src/policy.rs` — `SecurityPolicy::can_act`, `enforce_tool_operation`, `from_config` (workspace_only override at Full).
- `crates/zeroclaw-config/src/schema.rs` — `AutonomyConfig` struct, `default_auto_approve`, hardcoded defaults for `allowed_commands` / `forbidden_paths` / `max_actions_per_hour` / `max_cost_per_day_cents`.
- `crates/zeroclaw-tools/src/model_routing_config.rs` — write-gated on `security.can_act()`; `probe_model` skip condition.
- `crates/zeroclaw-runtime/src/tools/model_switch.rs` — Act-tier tool that sets a pending in-memory switch.
- `deploy/zeroclaw/config.seed.toml` — the file being edited.
- `AGENTS.md` — risk tier classification (this change falls under "access-control boundaries" = high risk).
