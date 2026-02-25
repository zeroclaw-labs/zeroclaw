# ZeroClaw i18n Completion Guide

This guide defines how to keep multilingual documentation complete and consistent when docs change.

## Scope

Use this guide when a PR touches any user-facing docs navigation, shared docs wording, runtime-contract references, or top-level docs governance.

Primary docs surfaces:

- Root landing: `README.md` (language switch links to `docs/i18n/<locale>/README.md`)
- Docs hubs: `docs/README.md`, `docs/i18n/<locale>/README.md`
- Unified TOC: `docs/SUMMARY.md`, `docs/SUMMARY.<locale>.md` (compatibility shims, if present)
- i18n index and coverage: `docs/i18n/README.md`, `docs/i18n-coverage.md`, `docs/i18n-gap-backlog.md`

Supported locales:

- `en` (source of truth)
- `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el` (full top-level parity in `docs/i18n/<locale>/`)

## Canonical Layout

Required structure:

- Root language landing: `README.md`
- Canonical localized docs hub: `docs/i18n/<locale>/README.md`
- Canonical localized summary: `docs/i18n/<locale>/SUMMARY.md`
- Optional compatibility shims: `docs/SUMMARY.<locale>.md` (if kept for backward links)

Compatibility shims may exist at docs root (for example `docs/SUMMARY.zh-CN.md`) and must remain aligned when touched.

## Trigger Matrix

Use this matrix to decide required i18n follow-through in the same PR.

| Change type | Required i18n follow-through |
|---|---|
| Root README language switch line changed | Update language switch line in `README.md` and verify all localized links resolve to `docs/i18n/<locale>/README.md` |
| Docs hub language links changed | Update localized hub links in `docs/README.md` and every `docs/i18n/*/README.md` with an "Other languages" section |
| Unified TOC language entry changed | Update `docs/SUMMARY.md`, every `docs/i18n/*/SUMMARY.md`, and docs-root `docs/SUMMARY*.md` shims if present |
| Collection index changed (`docs/<collection>/README.md`) | Update matching localized collection indexes under `docs/i18n/<locale>/<collection>/README.md` for locales that keep localized collection trees |
| Any top-level runtime/governance/security doc changed under `docs/*.md` | Update corresponding file under every `docs/i18n/<locale>/` in the same PR |
| Locale added/removed/renamed | Update `README.md`, docs hubs, summaries, `docs/i18n/README.md`, `docs/i18n-coverage.md`, and `docs/i18n-gap-backlog.md` |

## Completion Checklist (Mandatory)

Before merge, verify all items:

1. Locale navigation parity
- Root language switch line includes all supported locales.
- Docs hubs include all supported locales.
- Summary language entry includes all supported locales.

2. Canonical path consistency
- Non-English hubs point to `docs/i18n/<locale>/README.md`.
- Non-English summaries point to `docs/i18n/<locale>/SUMMARY.md`.
- Compatibility shims do not contradict canonical entries.

3. Top-level docs parity
- If any file under `docs/*.md` changes, sync localized equivalents for all supported locales.
- If full narrative translation is not feasible in the same PR, provide a bridge update (with source link) rather than leaving missing files.
- Bridge pages must include a source section map (at least level-2 headings) and practical execution hints.

4. Coverage metadata
- Update `docs/i18n-coverage.md` if support status, canonical path, or coverage level changed.
- Update `docs/i18n-gap-backlog.md` if baseline count changed.
- Keep date stamps current for changed localized hubs/summaries.

5. Link integrity
- Run markdown/link checks (or equivalent local relative-link existence check) on changed docs.

## Deferred Translation Policy

If full narrative localization cannot be completed in the same PR:

- Keep file-level parity complete (never leave locale file missing).
- Use localized bridge pages with clear source links to English normative docs.
- Keep bridge pages actionable: topic positioning + source section map + execution hints.
- Add explicit deferral note in PR description with owner and follow-up issue/PR.

Do not silently defer user-facing language parity changes.

## Agent Workflow Contract

When an agent touches docs IA or shared docs wording, the agent must:

1. Apply this guide and complete i18n follow-through in the same PR.
2. Update `docs/i18n-coverage.md`, `docs/i18n-gap-backlog.md`, and `docs/i18n/README.md` when locale topology or parity state changes.
3. Include i18n completion notes in PR summary (what was synced, what was bridged, why).

## Gap Tracking

- Track baseline parity closure and reopen events in [i18n-gap-backlog.md](i18n-gap-backlog.md).
- Update [i18n-coverage.md](i18n-coverage.md) after each localization wave.

## Quick Validation Commands

Examples:

```bash
# search locale references
rg -n "docs/i18n/(zh-CN|ja|ru|fr|vi|el)/README\.md|SUMMARY\.(zh-CN|ja|ru|fr|vi|el)\.md" README.md docs/SUMMARY*.md docs/i18n/*/README.md docs/i18n/*/SUMMARY.md

# check changed markdown files
git status --short

# quick parity count against top-level docs baseline
base=$(find docs -maxdepth 1 -type f -name '*.md' | sed 's#^docs/##' | \
  rg -v '^(README(\..+)?\.md|SUMMARY(\..+)?\.md|commands-reference\.vi\.md|config-reference\.vi\.md|one-click-bootstrap\.vi\.md|troubleshooting\.vi\.md)$' | sort)
for loc in zh-CN ja ru fr vi el; do
  c=0
  while IFS= read -r f; do
    [ -f "docs/i18n/$loc/$f" ] || c=$((c+1))
  done <<< "$base"
  echo "$loc $c"
done
```

Use repository-preferred markdown lint/link checks when available.
