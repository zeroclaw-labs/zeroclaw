# CI Cost Optimization — February 2026

> **Date:** 2026-02-18
> **Status:** Implemented
> **Impact:** ~60-65% reduction in estimated monthly GitHub Actions billable minutes

---

## Executive Summary

On February 17, 2026, the ZeroClaw repository consumed **400+ workflow runs** in a single day, totaling an estimated **398 billable minutes** (~6.6 hours). At this rate, monthly costs were projected at **~200 hours/month** (~12,000 billable minutes). This document describes the analysis performed, optimizations implemented, and the revised CI/CD architecture.

---

## Analysis Methodology

A Python script (`scripts/ci/fetch_actions_data.py`) was created to programmatically fetch and analyze all GitHub Actions workflow runs from the GitHub API for February 17, 2026. The script:

1. Fetched all completed workflow runs for the date via the GitHub REST API
2. Grouped runs by workflow name
3. Sampled job-level timing (up to 3 runs per workflow) to compute per-job durations
4. Extrapolated to estimate total billable minutes per workflow

### Raw Data Summary (February 17, 2026)

| Rank | Workflow | Runs/Day | Est. Minutes/Day | Primary Trigger |
|------|----------|----------|-------------------|-----------------|
| 1 | Rust Package Security Audit | 57 | 102 | Every PR + push |
| 2 | CI Run | 57 | 70 | Every PR + push |
| 3 | Performance Benchmarks | 15 | 63 | Every push to main |
| 4 | Docker | 20 | 63 | PR + push |
| 5 | PR Labeler | 69 | 20 | Every PR event |
| 6 | Feature Matrix | 3 | 19 | Push to main |
| 7 | Integration / E2E Tests | 15 | 17 | Every push to main |
| 8 | Workflow Sanity | 31 | 16 | Push + PR |
| 9 | Copilot Code Review | 6 | 14 | Dynamic |
| 10 | PR Intake Checks | 70 | 7 | Every PR event |
| 11 | PR Auto Responder | 47 | 4 | PR + issues |
| | **Total** | **400+** | **~398** | |

### Key Findings

- **15 pushes to main in ~2 hours** on Feb 17, each triggering 6-8 parallel workflows
- **Security Audit** was the single largest cost driver (102 min/day) with no path filtering
- **PR Auto Responder** had an **81% failure rate** (38/47 runs failing) — wasting runner time
- **CodeQL** runs twice daily (not captured in Feb 17 data since it's schedule-only) — adding ~3.5h/week
- **Benchmarks** ran on every push to main (15x in one day) despite being regression-focused
- **Dependabot** could generate up to 11 PRs/week, each triggering the full CI cascade

---

## Changes Implemented

### 1. Security Audit — Path Filters Added

**File:** `.github/workflows/sec-audit.yml`

**Before:** Ran on every PR and every push to main, regardless of what files changed.

**After:** Only runs when dependency or source files change:
- `Cargo.toml`, `Cargo.lock`, `src/**`, `crates/**`, `deny.toml`

**Weekly schedule retained** as a safety net for advisory database updates.

**Estimated savings:** ~60-70% of security audit runs eliminated (~30-35 hours/month)

### 2. Performance Benchmarks — Moved to Weekly Schedule

**File:** `.github/workflows/test-benchmarks.yml`

**Before:** Ran on every push to main (15x/day on Feb 17).

**After:** Runs weekly (Monday 3am UTC) + on-demand via `workflow_dispatch`.

**Artifact retention** reduced from 30 days to 7 days to lower storage costs.

**Rationale:** Benchmark regressions don't need per-commit detection. Weekly cadence catches regressions within one development cycle.

**Estimated savings:** ~90% reduction (~28 hours/month)

### 3. Docker PR Smoke Builds — Tightened Path Filters

**File:** `.github/workflows/pub-docker-img.yml`

**Before:** PR smoke builds triggered on any change to `src/**`, `crates/**`, `benches/**`, `firmware/**`, etc.

**After:** PR smoke builds only trigger on Docker-specific files:
- `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `rust-toolchain.toml`, `dev/config.template.toml`, `.github/workflows/pub-docker-img.yml`

**Push-to-main triggers unchanged** — production Docker images still rebuild on source changes.

**Estimated savings:** ~40-50% fewer Docker smoke builds (~12-15 hours/month)

### 4. CodeQL — Reduced from Twice-Daily to Weekly

**File:** `.github/workflows/sec-codeql.yml`

**Before:** Ran twice daily at 6am and 6pm UTC (14 runs/week), each performing a full `cargo build --workspace --all-targets`.

**After:** Runs weekly (Monday 6am UTC) + on-demand.

**Rationale:** CodeQL for Rust is still maturing. Weekly scans are standard practice for security-focused projects. On-demand dispatch available for urgent scans.

**Estimated savings:** ~12 hours/month

### 5. CI Run — Merged Lint Jobs + Dropped `--release` Build

**File:** `.github/workflows/ci-run.yml`

**Changes:**
1. **Merged `lint` and `lint-strict-delta` into a single job** — Previously these were two separate parallel jobs, each requiring a full runner spin-up, Rust toolchain install, and cache restore. Now they run sequentially in one job.
2. **Dropped `--release` flag from smoke build** — `cargo build --release` is 2-3x slower than debug due to optimizations. For a smoke check validating compilation, debug mode is equivalent.

**Estimated savings:** ~1 runner job per CI invocation + faster build times

### 6. Feature Matrix — Weekly-Only + Check-Only

**File:** `.github/workflows/feature-matrix.yml`

**Before:** Ran on every push to main touching `src/**` (3x on Feb 17) with 4 matrix entries, each running both `cargo check` AND `cargo test`.

**After:**
1. **Removed push trigger** — Now weekly-only (Monday 4:30am UTC) + on-demand
2. **Removed `cargo test`** — Only runs `cargo check --locked` per feature combination. Tests are already covered by the main CI Run workflow.

**Estimated savings:** ~50-75% of feature matrix compute eliminated

### 7. Lightweight Jobs Moved to `ubuntu-latest`

**Files affected:**
- `.github/workflows/pr-check-stale.yml`
- `.github/workflows/pr-check-status.yml`
- `.github/workflows/pr-auto-response.yml`
- `.github/workflows/pr-intake-checks.yml`
- `.github/workflows/pr-labeler.yml`
- `.github/workflows/sync-contributors.yml`

**Before:** All jobs used `blacksmith-2vcpu-ubuntu-2404` runners, even for lightweight API-only operations (labeling, stale checks, greetings).

**After:** Moved to `ubuntu-latest` (GitHub-hosted runners). These jobs only make API calls and run JavaScript scripts — they don't need Rust toolchains or specialized runners.

**Additional change:** `pr-check-status.yml` schedule reduced from every 12 hours to once daily (8:15am UTC).

### 8. Dependabot — Reduced Frequency and PR Limits

**File:** `.github/dependabot.yml`

**Before:**
- Cargo: weekly, 5 open PRs max
- GitHub Actions: weekly, 3 open PRs max
- Docker: weekly, 3 open PRs max
- Total: up to 11 Dependabot PRs/week, each triggering full CI

**After:**
- Cargo: **monthly**, 3 open PRs max, all deps grouped into single PR
- GitHub Actions: **monthly**, 1 open PR max, all grouped
- Docker: **monthly**, 1 open PR max, all grouped
- Total: up to 5 Dependabot PRs/month

**Rationale:** Each Dependabot PR triggers the full CI pipeline. Reducing from weekly to monthly and grouping updates into fewer PRs dramatically reduces CI cascade costs while still keeping dependencies current.

---

## Known Issues to Investigate

### PR Auto Responder — 81% Failure Rate

The `pr-auto-response.yml` workflow had 38 failures out of 47 runs on Feb 17. The `contributor-tier-issues` job fires on every issue `labeled`/`unlabeled` event, even when the label is not contributor-tier related. While the JavaScript handler exits early for non-tier labels, the runner still spins up and checks out the repository.

**Recommendations for further investigation:**
1. Add more specific event filtering at the workflow level to reduce unnecessary runs
2. Check if the failures are related to GitHub API rate limiting on the search endpoint
3. Consider whether `continue-on-error: true` should be added to non-critical jobs

---

## Revised Workflow Architecture

### Workflow Frequency Overview

| Workflow | Trigger | Runner |
|----------|---------|--------|
| **CI Run** | Push to main + PR | Blacksmith |
| **Sec Audit** | Push/PR (path-filtered) + weekly schedule | Blacksmith |
| **Sec CodeQL** | Weekly schedule | Blacksmith |
| **Test E2E** | Push to main | Blacksmith |
| **Test Benchmarks** | Weekly schedule | Blacksmith |
| **Test Fuzz** | Weekly schedule | Blacksmith |
| **Feature Matrix** | Weekly schedule | Blacksmith |
| **Docker Publish** | Push to main (broad paths) + PR (Docker-only paths) | Blacksmith |
| **Release** | Tag push only | GitHub-hosted |
| **Workflow Sanity** | Push/PR (workflow paths only) | Blacksmith |
| **Label Policy** | Push/PR (policy paths only) | Blacksmith |
| **PR Labeler** | PR events | **ubuntu-latest** |
| **PR Intake Checks** | PR events | **ubuntu-latest** |
| **PR Auto Responder** | PR + issue events | **ubuntu-latest** |
| **PR Check Stale** | Daily schedule | **ubuntu-latest** |
| **PR Check Status** | Daily schedule | **ubuntu-latest** |
| **Sync Contributors** | Weekly schedule | **ubuntu-latest** |

### Weekly Schedule Summary

| Day | Time (UTC) | Workflow |
|-----|-----------|----------|
| Monday | 03:00 | Test Benchmarks |
| Monday | 04:30 | Feature Matrix |
| Monday | 06:00 | Sec Audit (schedule) |
| Monday | 06:00 | Sec CodeQL |
| Sunday | 00:00 | Sync Contributors |
| Sunday | 02:00 | Test Fuzz |
| Daily | 02:20 | PR Check Stale |
| Daily | 08:15 | PR Check Status |

### CI Run Job Dependency Graph

```
changes ──┬── lint (Format + Clippy + Strict Delta)
           │     └── test
           ├── build (Smoke, debug mode)
           ├── docs-only (fast path)
           ├── non-rust (fast path)
           ├── docs-quality
           └── workflow-owner-approval

All above ──── ci-required (final gate)
```

### Push-to-Main Trigger Cascade

When code is pushed to `main`, the following workflows trigger:

1. **CI Run** — Always (change-detection gates individual jobs)
2. **Sec Audit** — Only if `Cargo.toml`, `Cargo.lock`, `src/**`, `crates/**`, or `deny.toml` changed
3. **Test E2E** — Always
4. **Docker Publish** — Only if broad source paths changed
5. **Workflow Sanity** — Only if workflow files changed

**No longer triggered on push:**
- ~~Performance Benchmarks~~ → Weekly only
- ~~Feature Matrix~~ → Weekly only

---

## Estimated Impact

| Metric | Before | After | Savings |
|--------|--------|-------|---------|
| Daily workflow runs | 400+ | ~150-180 | ~55-60% |
| Daily billable minutes | ~400 min | ~120-150 min | ~60-65% |
| Monthly billable hours | ~200 hours | ~60-75 hours | ~60-65% |
| Dependabot PRs/month | ~44 | ~5 | ~89% |
| CodeQL runs/week | 14 | 1 | ~93% |
| Benchmark runs/day | ~15 | 0 (weekly: ~1) | ~99% |

---

## Rollback Strategy

Each change is isolated to a single workflow file. To rollback any specific optimization:

1. **Revert the specific file** using `git checkout <commit>^ -- <file-path>`
2. Changes are backward-compatible — no downstream code or configuration depends on the CI schedule/trigger changes
3. All workflows retain `workflow_dispatch` triggers for manual invocation when needed

---

## Validation Checklist

- [ ] Verify CI Run workflow passes on next PR with Rust changes
- [ ] Verify Security Audit skips docs-only PRs
- [ ] Verify Docker smoke build only triggers on Dockerfile changes in PRs
- [ ] Verify weekly schedules fire correctly (check after first Monday)
- [ ] Monitor PR Auto Responder failure rate after switching to `ubuntu-latest`
- [ ] Verify Dependabot respects new monthly schedule and limits

---

## Files Modified

| File | Change Summary |
|------|---------------|
| `.github/workflows/sec-audit.yml` | Added path filters for push and PR triggers |
| `.github/workflows/test-benchmarks.yml` | Changed to weekly schedule; reduced artifact retention to 7 days |
| `.github/workflows/pub-docker-img.yml` | Tightened PR path filters to Docker-specific files |
| `.github/workflows/sec-codeql.yml` | Changed from twice-daily to weekly schedule |
| `.github/workflows/ci-run.yml` | Merged lint jobs; dropped `--release` from smoke build |
| `.github/workflows/feature-matrix.yml` | Removed push trigger; removed `cargo test` step |
| `.github/workflows/pr-check-stale.yml` | Switched to `ubuntu-latest` |
| `.github/workflows/pr-check-status.yml` | Switched to `ubuntu-latest`; reduced to daily schedule |
| `.github/workflows/pr-auto-response.yml` | Switched all jobs to `ubuntu-latest` |
| `.github/workflows/pr-intake-checks.yml` | Switched to `ubuntu-latest` |
| `.github/workflows/pr-labeler.yml` | Switched to `ubuntu-latest` |
| `.github/workflows/sync-contributors.yml` | Switched to `ubuntu-latest` |
| `.github/dependabot.yml` | Changed to monthly schedule; reduced PR limits; grouped all deps |
| `scripts/ci/fetch_actions_data.py` | New: cost analysis script for GitHub Actions runs |
