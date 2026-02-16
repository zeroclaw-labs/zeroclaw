# CI Workflow Map

This document explains what each GitHub workflow does, when it runs, and whether it should block merges.

## Merge-Blocking vs Optional

Merge-blocking checks should stay small and deterministic. Optional checks are useful for automation and maintenance, but should not block normal development.

### Merge-Blocking

- `.github/workflows/ci.yml` (`CI`)
  - Purpose: Rust validation (`fmt`, `clippy`, `test`, release build smoke)
  - Merge gate: `CI Required Gate`
- `.github/workflows/workflow-sanity.yml` (`Workflow Sanity`)
  - Purpose: lint GitHub workflow files (`actionlint`, tab checks)
  - Recommended for workflow-changing PRs

### Non-Blocking but Important

- `.github/workflows/docker.yml` (`Docker`)
  - Purpose: PR docker smoke check and publish images on `main`/tag pushes
- `.github/workflows/security.yml` (`Security Audit`)
  - Purpose: dependency advisories (`cargo audit`) and policy/license checks (`cargo deny`)
- `.github/workflows/release.yml` (`Release`)
  - Purpose: build tagged release artifacts and publish GitHub releases

### Optional Repository Automation

- `.github/workflows/labeler.yml` (`PR Labeler`)
  - Purpose: path labels + size labels
- `.github/workflows/auto-response.yml` (`Auto Response`)
  - Purpose: first-time contributor onboarding messages
- `.github/workflows/stale.yml` (`Stale`)
  - Purpose: stale issue/PR lifecycle automation

## Trigger Map

- `CI`: push to `main`/`develop`, PRs to `main`
- `Docker`: push to `main`, tag push (`v*`), PRs touching docker/workflow files, manual dispatch
- `Release`: tag push (`v*`)
- `Security Audit`: push to `main`, PRs to `main`, weekly schedule
- `Workflow Sanity`: PR/push when `.github/workflows/**` changes
- `PR Labeler`: `pull_request_target` lifecycle events
- `Auto Response`: issue opened, `pull_request_target` opened
- `Stale`: daily schedule, manual dispatch

## Fast Triage Guide

1. `CI Required Gate` failing: start with `.github/workflows/ci.yml`.
2. Docker failures on PRs: inspect `.github/workflows/docker.yml` `pr-smoke` job.
3. Release failures on tags: inspect `.github/workflows/release.yml`.
4. Security failures: inspect `.github/workflows/security.yml` and `deny.toml`.
5. Workflow syntax/lint failures: inspect `.github/workflows/workflow-sanity.yml`.

## Maintenance Rules

- Keep merge-blocking checks deterministic and reproducible (`--locked` where applicable).
- Prefer explicit workflow permissions (least privilege).
- Use path filters for expensive workflows when practical.
- Avoid mixing onboarding/community automation with merge-gating logic.
