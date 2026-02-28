# i18n Gap Backlog

This file tracks localization parity gaps and closure state.

Last updated: **2026-02-24**.

## Baseline Definition

Gap baseline = top-level English docs set under `docs/*.md` (excluding README/SUMMARY locale variants and legacy `*.vi.md` shims) compared against `docs/i18n/<locale>/`.

## Current Gap Counts

| Locale | Missing top-level docs parity count | Current status |
|---|---:|---|
| `zh-CN` | 0 | Full top-level parity (bridge + localized) |
| `ja` | 0 | Full top-level parity (bridge + localized) |
| `ru` | 0 | Full top-level parity (bridge + localized) |
| `fr` | 0 | Full top-level parity (bridge + localized) |
| `vi` | 0 | Full top-level parity |
| `el` | 0 | Full top-level parity |

## Closure Record (2026-02-24)

Completed in this PR stream:

- Wave 1 runtime localization for `zh-CN`/`ja`/`ru`/`fr`:
  - `commands-reference.md`
  - `providers-reference.md`
  - `channels-reference.md`
  - `config-reference.md`
  - `operations-runbook.md`
  - `troubleshooting.md`
- Full closure for remaining top-level docs in `zh-CN`/`ja`/`ru`/`fr` via localized bridge pages.
- Full top-level parity already maintained for `vi` and `el`.

## Remaining Gaps (Baseline Scope)

- None. Top-level baseline gaps are closed for all supported locales.

## Optional Next Depth

These are not baseline blockers, but can be advanced in future waves:

- fuller narrative translation depth for the 33 enhanced bridge pages in each of `zh-CN`/`ja`/`ru`/`fr`
- locale-specific examples (commands/config snippets) where operational behavior differs by provider/channel environment

## Tracking Rules

1. Keep this file date-stamped and append major closure checkpoints.
2. If new top-level English docs are added, re-run parity count and update this file in the same PR.
3. Keep locale navigation parity complete when adding/removing locales.
