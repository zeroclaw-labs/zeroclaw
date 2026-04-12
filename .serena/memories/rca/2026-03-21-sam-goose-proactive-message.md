# RCA: Sam's Daily 3pm PDT "Goose" + Todolist Status Message (2026-03-21)

## Symptom
Sam sends a proactive message at ~3pm PDT daily reporting on the todolist project and referencing "Goose" (Walter's former name).

## Root Causes

### RC1: Self-created cron job with stale prompt (primary)
Sam has `cron_add` access and likely created a recurring cron job before the Goose→Walter rename. Cron jobs persist in SQLite at `/data/.zeroclaw/workspace/cron/jobs.db` on a PVC (`rook-ceph-block`), surviving pod restarts indefinitely. The stored prompt still references "Goose."

### RC2: Stale SQLite memories
Sam's `memory_store` (SQLite, also on PVC) contains old observations from the Goose era. When the cron's isolated session runs, `memory_recall` for todolist context pulls up entries referencing "Goose."

## Why config cleanup didn't fix it
Static identity/config files (AGENTS.md, TOOLS.md, k8s-delegation skill) were updated to say "Walter," but runtime-persisted state on the PVC (cron prompts, SQLite memories) was never migrated.

## Remediation Applied (2026-03-21)

### Phase 1 — Static config (done)
- `k8s/walter/03_sandbox.yaml`: removed Goose references from header comments, added TODO for SA rename
- `k8s/walter/05_networkpolicy.yaml`: removed Goose reference from comment
- `k8s/sam/06_zeroclaw_networkpolicy.yaml`: replaced dead `goose-subagent:8080` and `goose-acp:8080` egress rules with `zeroclaw-k8s-agent:3000` + HBONE 15008
- Serena memory `skills/sam-walter-k8s-delegation`: annotated legacy goose paths

### Phase 2 — Runtime (requires Dan to instruct Sam)
1. `cron_list` → find offending 3pm job → `cron_remove` or `cron_update` to fix prompt
2. `memory_recall` for "Goose" → `memory_forget` stale entries
3. Verify no messages for 2-3 days

### Phase 2.5 — Structural fixes (done, runtime)
- MEMORY.md open loops: updated Vikunja status from "Milestone 1 done, waiting" to "DEPLOYED, 2 replicas running"
- Hourly self-reflection cron (`32a1f6be`): added two new behaviors:
  1. **Live state validation** — prompt now instructs Sam to verify project status with tools (vikunja CLI, acp-client) instead of relying solely on stale memory
  2. **Daily cron hygiene** — once per day, Sam audits her cron_list, assesses relevance, and disables/removes stale jobs

## Pattern
Same class as Walter cron loop postmortem: stale runtime state diverging from static config. PVC-backed SQLite state requires explicit migration during renames.

Additional pattern: **memory-only reflection loops** are inherently fragile. Agents with access to live tools (CLI, APIs) should cross-reference memory against live state before reporting.
