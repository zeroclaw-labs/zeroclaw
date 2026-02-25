# ZeroClaw i18n Coverage and Structure

This document defines the localization structure for ZeroClaw docs and tracks current coverage.

Last refreshed: **February 24, 2026**.

Execution guide: [i18n-guide.md](i18n-guide.md)
Gap backlog: [i18n-gap-backlog.md](i18n-gap-backlog.md)

## Canonical Layout

Use these i18n paths:

- Root language landing: `README.md` (language switch links to localized hubs)
- Full localized docs tree: `docs/i18n/<locale>/...`
- Optional compatibility shims at docs root:
  - `docs/SUMMARY.<locale>.md`
  - `docs/vi/**`

## Locale Coverage Matrix

| Locale | Root README | Canonical Docs Hub | Commands Ref | Config Ref | Troubleshooting | Status |
|---|---|---|---|---|---|---|
| `en` | `README.md` | `docs/README.md` | `docs/commands-reference.md` | `docs/config-reference.md` | `docs/troubleshooting.md` | Source of truth |
| `zh-CN` | `docs/i18n/zh-CN/README.md` | `docs/i18n/zh-CN/README.md` | `docs/i18n/zh-CN/commands-reference.md` | `docs/i18n/zh-CN/config-reference.md` | `docs/i18n/zh-CN/troubleshooting.md` | Full top-level parity (bridge + localized) |
| `ja` | `docs/i18n/ja/README.md` | `docs/i18n/ja/README.md` | `docs/i18n/ja/commands-reference.md` | `docs/i18n/ja/config-reference.md` | `docs/i18n/ja/troubleshooting.md` | Full top-level parity (bridge + localized) |
| `ru` | `docs/i18n/ru/README.md` | `docs/i18n/ru/README.md` | `docs/i18n/ru/commands-reference.md` | `docs/i18n/ru/config-reference.md` | `docs/i18n/ru/troubleshooting.md` | Full top-level parity (bridge + localized) |
| `fr` | `docs/i18n/fr/README.md` | `docs/i18n/fr/README.md` | `docs/i18n/fr/commands-reference.md` | `docs/i18n/fr/config-reference.md` | `docs/i18n/fr/troubleshooting.md` | Full top-level parity (bridge + localized) |
| `vi` | `docs/i18n/vi/README.md` | `docs/i18n/vi/README.md` | `docs/i18n/vi/commands-reference.md` | `docs/i18n/vi/config-reference.md` | `docs/i18n/vi/troubleshooting.md` | Full tree localized |
| `el` | `docs/i18n/el/README.md` | `docs/i18n/el/README.md` | `docs/i18n/el/commands-reference.md` | `docs/i18n/el/config-reference.md` | `docs/i18n/el/troubleshooting.md` | Full tree localized |

## Top-Level Parity Snapshot

Baseline on 2026-02-24 uses 40 top-level English docs (`docs/*.md`, locale root variants excluded).

| Locale | Missing top-level parity count |
|---|---:|
| `zh-CN` | 0 |
| `ja` | 0 |
| `ru` | 0 |
| `fr` | 0 |
| `vi` | 0 |
| `el` | 0 |

## Narrative Depth Snapshot

As of 2026-02-24:

| Locale | Enhanced bridge pages | Notes |
|---|---:|---|
| `zh-CN` | 33 | Bridge pages include topic positioning + source section map + execution hints |
| `ja` | 33 | Bridge pages include topic positioning + source section map + execution hints |
| `ru` | 33 | Bridge pages include topic positioning + source section map + execution hints |
| `fr` | 33 | Bridge pages include topic positioning + source section map + execution hints |
| `vi` | N/A | Existing localization style kept as full localized tree |
| `el` | N/A | Existing localization style kept as full localized tree |

## Localized Landing Completeness

Not all localized landing pages are full translations of `README.md`:

| Locale | Style | Approximate Coverage |
|---|---|---|
| `en` | Full source | 100% |
| `zh-CN` | Hub-style entry point | ~26% |
| `ja` | Hub-style entry point | ~26% |
| `ru` | Hub-style entry point | ~26% |
| `fr` | Near-complete translation | ~90% |
| `vi` | Near-complete translation | ~90% |
| `el` | Near-complete translation | ~90% |

Hub-style entry points provide quick-start orientation and language navigation but do not replicate the full English README content. This is an accurate status record, not a gap to be immediately resolved.

For `zh-CN`, `ja`, `ru`, and `fr`, canonical `docs/i18n/<locale>/` hubs include full top-level parity coverage and maintain language navigation through canonical i18n paths.

## Collection Index i18n

Localized category index files now exist for all supported locales under:

- `docs/i18n/<locale>/getting-started/README.md`
- `docs/i18n/<locale>/reference/README.md`
- `docs/i18n/<locale>/operations/README.md`
- `docs/i18n/<locale>/security/README.md`
- `docs/i18n/<locale>/hardware/README.md`
- `docs/i18n/<locale>/contributing/README.md`
- `docs/i18n/<locale>/project/README.md`

This closes collection-index localization parity for supported locales.

## Localization Rules

- Keep technical identifiers in English:
  - CLI command names
  - config keys
  - API paths
  - trait/type identifiers
- Prefer concise, operator-oriented localization over literal translation.
- Update "Last refreshed" / "Last synchronized" dates when localized pages change.
- Ensure every localized hub has an "Other languages" section.
- Follow [i18n-guide.md](i18n-guide.md) for mandatory completion and deferral policy.

## Adding a New Locale

1. Add locale entry to `README.md` language switch pointing to `docs/i18n/<locale>/README.md`.
2. Create canonical docs tree under `docs/i18n/<locale>/` (at least `README.md`, `commands-reference.md`, `config-reference.md`, `troubleshooting.md`).
3. Add locale links to:
   - localized hubs line in `docs/README.md`
   - "Other languages" section in every `docs/i18n/*/README.md`
   - language entry section in `docs/SUMMARY.md`, `docs/i18n/*/SUMMARY.md`, and docs-root `docs/SUMMARY.<locale>.md` shims if present
4. Optionally add docs-root shim files for backward compatibility.
5. Update this file (`docs/i18n-coverage.md`) and run link validation.

## Review Checklist

- Links resolve for all localized entry files.
- No locale references stale filenames (for example `README.vn.md`).
- TOC (`docs/SUMMARY.md`) and docs hub (`docs/README.md`) include the locale.
