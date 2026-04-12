# Sam Pod RCA Remediation Plan

**Overall Progress:** `0%`

## TLDR

Post-RCA remediation for 5 issues found in the Sam (zeroclaw) pod on 2026-03-31. The critical issue (dead API key causing provider 500s) was resolved by key rotation. This plan covers the remaining code-level and IaC fixes: SQLite startup race condition, cron frequency false-positive warning, and manifest version drift.

## Critical Decisions

- Decision 1: Add `PRAGMA busy_timeout = 5000` to SQLite init — eliminates the race without adding connection pooling or startup serialization complexity.
- Decision 2: Fix cron frequency heuristic by comparing two *distinct* next-run times — minimal change that eliminates false positives for multi-time and wide-interval cron expressions.
- Decision 3: Bump manifest image tags from v1.4.15 to v1.5.0 — aligns IaC with live state to prevent accidental downgrade on next apply.

## RCA Findings Reference

| # | Issue | Severity | Status |
|---|---|---|---|
| F0 | IaC drift (v1.4.15 manifest vs v1.5.0 live) | Medium | Needs fix |
| F1 | SQLite "database is locked" startup race | Error | Needs fix |
| F2 | Signal SSE startup race (container ordering) | Warn | Accept (self-healing) |
| F3 | LiteLLM provider 500s (dead API key) | Critical | **Resolved** (key rotated) |
| F4 | Cron frequency warning false positive | Warn | Needs fix |
| F5 | Serena project path missing | Warn | Accept (cosmetic) |

## Tasks

- [ ] 🟥 **Step 1: Fix SQLite busy_timeout in memory backend**
  - [ ] 🟥 In `src/memory/sqlite.rs` ~line 112, add `PRAGMA busy_timeout = 5000;` to the `execute_batch` block, after `PRAGMA synchronous = NORMAL;`
  - [ ] 🟥 In `src/cron/store.rs` ~line 549, add `PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;` before `CREATE TABLE IF NOT EXISTS` in `with_connection()`
  - [ ] 🟥 Verify with `cargo clippy` and `cargo test`

- [ ] 🟥 **Step 2: Fix cron frequency heuristic false positive**
  - [ ] 🟥 In `src/cron/scheduler.rs` ~line 282, replace the `Schedule::Cron` branch: instead of computing `next_run(now)` vs `next_run(now + 1s)`, compute `next_run(now)` then `next_run(next_run_result + 1s)` to get two *consecutive distinct* run times
  - [ ] 🟥 Add a unit test: `warn_if_high_frequency_agent_job` should NOT warn for `0 12,17 * * 1-5` or `0 */3 * * *`, and SHOULD warn for `* * * * *` (every minute)

- [ ] 🟥 **Step 3: Update IaC manifests to match live state**
  - [ ] 🟥 In `k8s/sam/04_zeroclaw_sandbox.yaml`, update both image tags from `v1.4.15` → `v1.5.0` (lines ~89 and ~228)

- [ ] 🟥 **Step 4: Validate**
  - [ ] 🟥 Run `cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test`
  - [ ] 🟥 Dry-run the manifest: `kubectl diff -f k8s/sam/04_zeroclaw_sandbox.yaml`
