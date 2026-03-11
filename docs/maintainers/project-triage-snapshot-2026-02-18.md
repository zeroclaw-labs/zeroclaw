# ZeroClaw Project Triage Snapshot (2026-02-18)

As-of date: **February 18, 2026**.

This snapshot captures open PR/issue signals to guide docs and information-architecture work.

## Data Source

Collected via GitHub CLI against `zeroclaw-labs/zeroclaw`:

- `gh repo view ...`
- `gh pr list --state open --limit 500 ...`
- `gh issue list --state open --limit 500 ...`
- `gh pr/issue view <id> ...` for docs-relevant items

## Repository Pulse

- Open PRs: **30**
- Open Issues: **24**
- Stars: **11,220**
- Forks: **1,123**
- Default branch: `main`
- License metadata on GitHub API: `Other` (not MIT-detected)

## PR Label Pressure (Open PRs)

Top signals by frequency:

1. `risk: high` — 24
2. `experienced contributor` — 14
3. `size: S` — 14
4. `ci` — 11
5. `size: XS` — 10
6. `dependencies` — 7
7. `principal contributor` — 6

Implication for docs:

- CI/security/service changes remain high-churn areas.
- Operator-facing docs should prioritize “what changed” visibility and fast troubleshooting paths.

## Issue Label Pressure (Open Issues)

Top signals by frequency:

1. `experienced contributor` — 12
2. `enhancement` — 8
3. `bug` — 4

Implication for docs:

- Feature and performance requests still outpace explanatory docs.
- Troubleshooting and operational references should be kept near the top navigation.

## Docs-Relevant Open PRs

- [#716](https://github.com/zeroclaw-labs/zeroclaw/pull/716) — OpenRC support (service behavior/docs impact)
- [#725](https://github.com/zeroclaw-labs/zeroclaw/pull/725) — shell completion commands (CLI docs impact)
- [#732](https://github.com/zeroclaw-labs/zeroclaw/pull/732) — CI action replacement (contributor workflow docs impact)
- [#759](https://github.com/zeroclaw-labs/zeroclaw/pull/759) — daemon/channel response handling fix (channel troubleshooting impact)
- [#679](https://github.com/zeroclaw-labs/zeroclaw/pull/679) — pairing lockout accounting change (security behavior docs impact)

## Docs-Relevant Open Issues

- [#426](https://github.com/zeroclaw-labs/zeroclaw/issues/426) — explicit request for clearer capabilities documentation
- [#666](https://github.com/zeroclaw-labs/zeroclaw/issues/666) — operational runbook and alert/logging guidance request
- [#745](https://github.com/zeroclaw-labs/zeroclaw/issues/745) — Docker pull failure (`ghcr.io`) suggests deployment troubleshooting demand
- [#761](https://github.com/zeroclaw-labs/zeroclaw/issues/761) — Armbian compile error highlights platform troubleshooting needs
- [#758](https://github.com/zeroclaw-labs/zeroclaw/issues/758) — storage backend flexibility request impacts config/reference docs

## Recommended Docs Backlog (Priority Order)

1. **Keep docs IA stable and obvious**
   - Maintain `docs/SUMMARY.md` + collection indexes as canonical nav.
   - Keep localized hubs aligned with the same top-level doc map.

2. **Protect operator discoverability**
   - Keep `operations-runbook` + `troubleshooting` linked in top-level README/hubs.
   - Add platform-specific troubleshooting snippets when issues repeat.

3. **Track CLI/config drift aggressively**
   - Update `commands/providers/channels/config` references when PRs touching these surfaces merge.

4. **Separate current behavior from proposals**
   - Preserve proposal banners in security roadmap docs.
   - Keep runtime-contract docs (`config/runbook/troubleshooting`) clearly marked.

5. **Maintain snapshot discipline**
   - Keep snapshots date-stamped and immutable.
   - Create a new snapshot file for each docs sprint instead of mutating historical snapshots.

## Snapshot Caveat

This is a time-bound snapshot (2026-02-18). Re-run the `gh` queries before planning a new documentation sprint.
