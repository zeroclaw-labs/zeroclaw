# Full Autonomy for Adi Personas — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Flip both `adi-zeroclaw-shane` and `adi-zeroclaw-meg` from `[autonomy] level = "supervised"` to `level = "full"` so the personas can invoke any Act-tier tool (including `model_switch` and `model_routing_config`) without a human approval step.

**Architecture:** One-line change to `deploy/zeroclaw/config.seed.toml` (keeps fresh volumes aligned with the intended default), followed by a live-config `sed` + `kill 1` per existing volume since the seed does not overwrite `/zeroclaw-data/.zeroclaw/config.toml` on volumes that already have it. Verification is via `fly ssh` grep + `/api/health` + a channel smoke test.

**Tech Stack:** Rust zeroclaw daemon on debian:trixie-slim / Fly.io Machines, TOML config seeded from the repo on first boot, `fly ssh console` for in-container ops.

**Spec:** [`docs/superpowers/specs/2026-04-22-full-autonomy-design.md`](../specs/2026-04-22-full-autonomy-design.md) (commit `976b37471`)

**Risk tier:** High (access-control boundary change per AGENTS.md). This plan intentionally keeps the change surface to a single line and adds a per-persona backup + staged rollout so rollback is one `sed` command.

---

## File Structure

**Files touched by this plan:**

- `deploy/zeroclaw/config.seed.toml` — modified (one line)

**Files created per persona at runtime (not in repo, on Fly volumes):**

- `/zeroclaw-data/.zeroclaw/config.toml.bak` — created by Task 2 as a pre-edit backup, used by the rollback path. Lives on the persona's Fly volume, not in the repo.

No new Rust code, no Dockerfile change, no fly.toml change, no entrypoint.sh change. This plan intentionally does not touch the "seed once, live config is authoritative" invariant.

---

## Task 1: Edit and commit the seed change

**Files:**
- Modify: `deploy/zeroclaw/config.seed.toml:38`

- [ ] **Step 1: Read the current seed to confirm starting state**

Run:
```bash
grep -n -A 10 '\[autonomy\]' deploy/zeroclaw/config.seed.toml
```

Expected output:
```
37:[autonomy]
38:level = "supervised"
39:auto_approve = [
40:    "file_read", "file_write", "file_edit",
41:    "memory_recall", "memory_store",
42:    "web_search_tool", "web_fetch",
43:    "calculator", "glob_search", "content_search",
44:    "image_info", "weather", "git_operations"
45:]
```

If line 38 is not `level = "supervised"`, stop and reconcile — someone else has already changed the seed.

- [ ] **Step 2: Edit the single line**

Change line 38 from:
```toml
level = "supervised"
```
to:
```toml
level = "full"
```

Leave `auto_approve` untouched (vestigial at Full but out of scope to remove).

- [ ] **Step 3: Verify the diff is exactly one line**

Run:
```bash
git diff --stat deploy/zeroclaw/config.seed.toml
git diff deploy/zeroclaw/config.seed.toml
```

Expected: `1 file changed, 1 insertion(+), 1 deletion(-)` and a diff that shows only the `level` line changing from `"supervised"` to `"full"`.

If the diff shows more than one line changed, `git checkout deploy/zeroclaw/config.seed.toml` and redo Step 2.

- [ ] **Step 4: Commit on branch `deploy/v0.7.3`**

Run:
```bash
git status
```

Expected: On branch `deploy/v0.7.3`, one modified file (`deploy/zeroclaw/config.seed.toml`). Untracked files (`id_staging`, `id_staging.pub`, `walmart/`) are pre-existing and must not be staged.

Run:
```bash
git add deploy/zeroclaw/config.seed.toml
git commit -m "$(cat <<'EOF'
deploy: flip adi personas to autonomy level = "full"

Adi personas (adi-zeroclaw-{shane,meg}) run headless on Fly with no
interactive operator, so the supervised approval gate effectively blocks
any Act-tier tool not in auto_approve (model_switch,
model_routing_config, shell, cloud_ops, claude_code, mcp_tool,
http_request, memory_purge, etc.). Flip the seed to level = "full" so
the intended default for future fresh volumes matches the runbook.

Existing volumes (shane, meg) keep their live config on /zeroclaw-data;
a separate live-config sed + kill 1 is required to flip running
personas. See docs/superpowers/specs/2026-04-22-full-autonomy-design.md.

Blast doors still in force at Full: forbidden_paths (default),
block_high_risk_commands = true (default), allowed_commands (default
conservative list with no rm/curl/wget), max_actions_per_hour = 100,
max_cost_per_day_cents = 1000, channels.*.allowed_users,
gateway.require_pairing = true.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Verify commit landed cleanly**

Run:
```bash
git log --oneline -1
git show --stat HEAD
```

Expected: newest commit is the autonomy flip, touches exactly one file with one insertion and one deletion.

---

## Task 2: Deploy seed to both personas (no runtime effect)

Purpose of this deploy: keep the `/app`-embedded seed in sync with the intended default so any future fresh volume (rebuild, scale-up to a new machine with a new volume) picks up `full` automatically. Has **zero effect on the currently running shane and meg processes** because their volumes already have `/zeroclaw-data/.zeroclaw/config.toml` from their initial deploy.

**Files:** No repo changes beyond Task 1. Purely a deploy action.

- [ ] **Step 1: Confirm both apps are currently healthy before deploy**

Run:
```bash
curl -s https://adi-zeroclaw-shane.fly.dev/api/health
curl -s https://adi-zeroclaw-meg.fly.dev/api/health
```

Expected: Both return JSON with all 8 components `ok` (channels, channel:slack, channel:telegram, daemon, gateway, heartbeat, mqtt, scheduler). If either is already unhealthy, stop and investigate — don't overlay a deploy on top of an existing failure.

- [ ] **Step 2: Deploy shane with the new seed**

Run:
```bash
flyctl deploy --config deploy/zeroclaw/fly.shane.toml --dockerfile deploy/zeroclaw/Dockerfile --app adi-zeroclaw-shane --remote-only
```

Expected: Build completes, image pushes, a new machine boots and becomes healthy. `flyctl` exit code 0.

If the build fails: do not proceed to meg. Investigate the build failure. The flip is not safe to partially apply only on one persona.

- [ ] **Step 3: Deploy meg using the same image**

Per the memory convention (`project_zeroclaw_adi.md`), meg reuses shane's image to guarantee binary parity. Capture shane's image ref first:

```bash
flyctl image show --app adi-zeroclaw-shane
```

Expected output includes a line like:
```
Image: registry.fly.io/adi-zeroclaw-shane:deployment-01HX...
```

Copy that full image reference (`registry.fly.io/adi-zeroclaw-shane:deployment-…`). Then deploy meg with that exact ref:

```bash
flyctl deploy --config deploy/zeroclaw/fly.meg.toml --image registry.fly.io/adi-zeroclaw-shane:deployment-01HX... --app adi-zeroclaw-meg
```

Expected: meg pulls the image (no rebuild), a new machine boots, `flyctl` exits 0.

- [ ] **Step 4: Re-confirm both apps healthy post-deploy**

Run:
```bash
curl -s https://adi-zeroclaw-shane.fly.dev/api/health
curl -s https://adi-zeroclaw-meg.fly.dev/api/health
```

Expected: Both still report 8/8 components `ok`.

- [ ] **Step 5: Confirm the live config on shane is STILL `supervised`**

This is the key assertion for this task — the deploy must not have flipped the live config, because the seed does not overwrite `/zeroclaw-data/.zeroclaw/config.toml` when it already exists. If this check shows `full`, the "seed once" invariant has been accidentally broken by some other change and Task 3 would be redundant.

Run:
```bash
fly ssh console -a adi-zeroclaw-shane -C "grep -A1 '\[autonomy\]' /zeroclaw-data/.zeroclaw/config.toml"
```

Expected:
```
[autonomy]
level = "supervised"
```

If this shows `level = "full"`, stop and read `deploy/zeroclaw/entrypoint.sh` to understand what overwrote the live config. Skip Task 3.

Repeat for meg:
```bash
fly ssh console -a adi-zeroclaw-meg -C "grep -A1 '\[autonomy\]' /zeroclaw-data/.zeroclaw/config.toml"
```

Expected: `level = "supervised"`.

---

## Task 3: Flip shane to Full

**Files:**
- Modify (on shane's volume): `/zeroclaw-data/.zeroclaw/config.toml`
- Create (on shane's volume): `/zeroclaw-data/.zeroclaw/config.toml.bak`

- [ ] **Step 1: Create a pre-edit backup on the volume**

Run:
```bash
fly ssh console -a adi-zeroclaw-shane -C "cp /zeroclaw-data/.zeroclaw/config.toml /zeroclaw-data/.zeroclaw/config.toml.bak"
```

Expected: command exits 0, no output.

Verify the backup exists and is non-empty:
```bash
fly ssh console -a adi-zeroclaw-shane -C "ls -la /zeroclaw-data/.zeroclaw/config.toml /zeroclaw-data/.zeroclaw/config.toml.bak"
```

Expected: two files listed, both non-zero size, both owned by the zeroclaw user.

- [ ] **Step 2: Flip the autonomy level via `sed`**

Run:
```bash
fly ssh console -a adi-zeroclaw-shane -C "sed -i 's/^level = \"supervised\"/level = \"full\"/' /zeroclaw-data/.zeroclaw/config.toml"
```

Expected: command exits 0, no output.

- [ ] **Step 3: Verify the edit before restarting**

Run:
```bash
fly ssh console -a adi-zeroclaw-shane -C "grep -A1 '\[autonomy\]' /zeroclaw-data/.zeroclaw/config.toml"
```

Expected:
```
[autonomy]
level = "full"
```

If this still shows `supervised`, the `sed` substitution did not match. Possible causes: the line has leading whitespace, or the TOML parser rewrote the file with different quoting since the seed was applied. Inspect the raw line:

```bash
fly ssh console -a adi-zeroclaw-shane -C "grep -n 'level' /zeroclaw-data/.zeroclaw/config.toml | head -5"
```

Adjust the `sed` pattern (e.g., single quotes vs double, leading whitespace) and redo Step 2. Do **not** proceed to Step 4 until the grep above shows `level = "full"`.

- [ ] **Step 4: Restart the container by killing PID 1**

Run:
```bash
fly ssh console -a adi-zeroclaw-shane -C "kill 1"
```

Expected: SSH session terminates (because the machine is restarting). This is normal.

- [ ] **Step 5: Wait for restart and verify health**

Run (wait 10–20 seconds for the machine to come back, then):
```bash
curl -s https://adi-zeroclaw-shane.fly.dev/api/health
```

Expected: JSON with 8 components all `ok`. If the response is a connection error or a non-200 status, wait another 15 seconds and retry. If it still fails after 60 seconds, go to the rollback steps below.

- [ ] **Step 6: Confirm the running process loaded `full`**

The config change is only meaningful if the daemon re-read the file. Verify:

```bash
fly ssh console -a adi-zeroclaw-shane -C "grep -A1 '\[autonomy\]' /zeroclaw-data/.zeroclaw/config.toml"
```

Expected: `level = "full"` (sanity check that `kill 1` didn't somehow revert the file).

Check the logs for the boot-time config load:
```bash
fly logs -a adi-zeroclaw-shane | tail -50
```

Expected: a recent log line confirming config was loaded from `/zeroclaw-data/.zeroclaw/config.toml`. No `Security policy` errors, no TOML parse errors, no panics.

**If Step 5 or 6 fails — rollback shane:**

```bash
fly ssh console -a adi-zeroclaw-shane -C "cp /zeroclaw-data/.zeroclaw/config.toml.bak /zeroclaw-data/.zeroclaw/config.toml"
fly ssh console -a adi-zeroclaw-shane -C "kill 1"
```

Then wait 20 seconds and re-check `/api/health`. Stop this plan and investigate the failure before continuing to meg.

---

## Task 4: Smoke-test shane's autonomous tool use

Before flipping meg, prove that the flip on shane actually achieves the goal — `model_switch` can run without an approval prompt.

**Files:** None — this is an operational test via a channel message.

- [ ] **Step 1: Send shane a channel message that exercises `model_switch`**

Via Telegram (as user ID `7559901218` — Shane — to the shane bot) or Slack (as user ID `U0ATMP4SGDD` to the shane app), send:

> Please run the `model_switch` tool with action `list_providers` and show me the output.

- [ ] **Step 2: Observe the response**

Expected:
- Shane responds within normal latency (under 30 seconds).
- The response contains a JSON-ish list of providers (openai, anthropic, groq, ollama, etc.).
- No message along the lines of "I cannot do that because approval is required" or "Security policy blocked."

If shane refuses with a security/approval message, the flip did not take effect. Re-run the grep from Task 3 Step 6 — if it still shows `full`, the issue is elsewhere (tool registration, policy caching on an in-memory copy of the config). Stop and investigate.

- [ ] **Step 3: Spot-check the audit log**

Run:
```bash
fly logs -a adi-zeroclaw-shane | grep -i 'model_switch\|security.*policy\|approval' | tail -20
```

Expected: a log entry for the `model_switch` tool call, no "approval required" or "read-only" errors for it.

---

## Task 5: Flip meg to Full (identical to Task 3, parameters substituted)

Only proceed if Task 3 completed cleanly AND Task 4 passed. If either failed, stop.

**Files:**
- Modify (on meg's volume): `/zeroclaw-data/.zeroclaw/config.toml`
- Create (on meg's volume): `/zeroclaw-data/.zeroclaw/config.toml.bak`

- [ ] **Step 1: Create a pre-edit backup on meg's volume**

Run:
```bash
fly ssh console -a adi-zeroclaw-meg -C "cp /zeroclaw-data/.zeroclaw/config.toml /zeroclaw-data/.zeroclaw/config.toml.bak"
fly ssh console -a adi-zeroclaw-meg -C "ls -la /zeroclaw-data/.zeroclaw/config.toml /zeroclaw-data/.zeroclaw/config.toml.bak"
```

Expected: two files listed, both non-zero size.

- [ ] **Step 2: Flip the autonomy level**

Run:
```bash
fly ssh console -a adi-zeroclaw-meg -C "sed -i 's/^level = \"supervised\"/level = \"full\"/' /zeroclaw-data/.zeroclaw/config.toml"
```

- [ ] **Step 3: Verify the edit before restarting**

Run:
```bash
fly ssh console -a adi-zeroclaw-meg -C "grep -A1 '\[autonomy\]' /zeroclaw-data/.zeroclaw/config.toml"
```

Expected:
```
[autonomy]
level = "full"
```

Do not proceed to Step 4 until this check passes.

- [ ] **Step 4: Restart the container**

Run:
```bash
fly ssh console -a adi-zeroclaw-meg -C "kill 1"
```

Expected: SSH session terminates.

- [ ] **Step 5: Wait for restart and verify health**

Wait 10–20 seconds, then:
```bash
curl -s https://adi-zeroclaw-meg.fly.dev/api/health
```

Expected: 8/8 components `ok`.

- [ ] **Step 6: Confirm meg loaded `full`**

```bash
fly ssh console -a adi-zeroclaw-meg -C "grep -A1 '\[autonomy\]' /zeroclaw-data/.zeroclaw/config.toml"
fly logs -a adi-zeroclaw-meg | tail -50
```

Expected: `level = "full"`, clean boot logs, no TOML parse or policy errors.

**If Step 5 or 6 fails — rollback meg:**

```bash
fly ssh console -a adi-zeroclaw-meg -C "cp /zeroclaw-data/.zeroclaw/config.toml.bak /zeroclaw-data/.zeroclaw/config.toml"
fly ssh console -a adi-zeroclaw-meg -C "kill 1"
```

Note: shane remains at Full. An asymmetric state (shane=full, meg=supervised) is acceptable — document it in a follow-up and investigate meg separately. Do not roll back shane unless there is a reason tied to shane specifically.

---

## Task 6: Smoke-test meg's autonomous tool use

**Files:** None — operational test.

- [ ] **Step 1: Send meg a channel message that exercises `model_switch`**

Via Telegram (as user ID `8636712032` — Meg — to the meg bot) or Slack (as user ID `U0ATQKX5ZGD` to the meg app), send:

> Please run the `model_switch` tool with action `list_providers` and show me the output.

- [ ] **Step 2: Observe the response**

Expected: meg responds with a provider list, no approval-refusal message.

- [ ] **Step 3: Spot-check meg's audit log**

Run:
```bash
fly logs -a adi-zeroclaw-meg | grep -i 'model_switch\|security.*policy\|approval' | tail -20
```

Expected: `model_switch` tool-call entry present, no approval-required errors.

---

## Task 7: Push the commit and close out

**Files:** None beyond the commit from Task 1.

- [ ] **Step 1: Push the branch**

Run:
```bash
git push origin deploy/v0.7.3
```

Expected: push succeeds. This publishes the seed change to `github.com/srmcguirt/zeroclaw` so the intended-default lives alongside the deploy history.

- [ ] **Step 2: Update auto-memory with the outcome**

Edit `C:\Users\srmcg\.claude\projects\c--git-adi\memory\project_zeroclaw_adi.md` to reflect the new runtime state: both personas at `autonomy.level = "full"` as of 2026-04-22, rollback path is `sed` + `kill 1` with `/zeroclaw-data/.zeroclaw/config.toml.bak` as the restore source on each volume.

The relevant existing memory file already describes the personas' deploy state; append a paragraph rather than rewriting the whole file. Keep the memory under the 200-line `MEMORY.md` index limit.

- [ ] **Step 3: Final state-of-the-world check**

Run:
```bash
curl -s https://adi-zeroclaw-shane.fly.dev/api/health | grep -o '"status":"ok"' | wc -l
curl -s https://adi-zeroclaw-meg.fly.dev/api/health | grep -o '"status":"ok"' | wc -l
```

Expected: `8` for each persona (all 8 components reporting status "ok").

Plan complete.

---

## Rollback reference (all personas back to Supervised)

If a reason emerges to revert both personas entirely:

```bash
# shane
fly ssh console -a adi-zeroclaw-shane -C "sed -i 's/^level = \"full\"/level = \"supervised\"/' /zeroclaw-data/.zeroclaw/config.toml"
fly ssh console -a adi-zeroclaw-shane -C "kill 1"

# meg
fly ssh console -a adi-zeroclaw-meg -C "sed -i 's/^level = \"full\"/level = \"supervised\"/' /zeroclaw-data/.zeroclaw/config.toml"
fly ssh console -a adi-zeroclaw-meg -C "kill 1"

# (Optional) also revert the seed in the repo
git revert <commit-from-task-1>
git push origin deploy/v0.7.3
```

Hard stop for one persona (takes it fully offline):
```bash
flyctl scale count 0 -a adi-zeroclaw-shane   # or adi-zeroclaw-meg
```

Restore from the on-volume backup (if `sed` corrupted the file mid-write):
```bash
fly ssh console -a adi-zeroclaw-shane -C "cp /zeroclaw-data/.zeroclaw/config.toml.bak /zeroclaw-data/.zeroclaw/config.toml"
fly ssh console -a adi-zeroclaw-shane -C "kill 1"
```
