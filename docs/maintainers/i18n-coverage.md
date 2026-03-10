# ZeroClaw i18n Coverage and Structure

This document defines the localization structure for ZeroClaw docs and tracks current coverage.

Last refreshed: **February 21, 2026**.

## Canonical Layout

Use these i18n paths:

- Root language landing: `README.<locale>.md`
- Full localized docs tree: `docs/i18n/<locale>/...`
- Optional compatibility shims at docs root:
  - `docs/README.<locale>.md`
  - `docs/commands-reference.<locale>.md`
  - `docs/config-reference.<locale>.md`
  - `docs/troubleshooting.<locale>.md`

## Locale Coverage Matrix

| Locale | Root README | Canonical Docs Hub | Commands Ref | Config Ref | Troubleshooting | Status |
|---|---|---|---|---|---|---|
| `en` | `README.md` | `docs/README.md` | `docs/commands-reference.md` | `docs/config-reference.md` | `docs/troubleshooting.md` | Source of truth |
| `zh-CN` | `README.zh-CN.md` | `docs/README.zh-CN.md` | - | - | - | Hub-level localized |
| `ja` | `README.ja.md` | `docs/README.ja.md` | - | - | - | Hub-level localized |
| `ru` | `README.ru.md` | `docs/README.ru.md` | - | - | - | Hub-level localized |
| `fr` | `README.fr.md` | `docs/README.fr.md` | - | - | - | Hub-level localized |
| `vi` | `README.vi.md` | `docs/i18n/vi/README.md` | `docs/i18n/vi/commands-reference.md` | `docs/i18n/vi/config-reference.md` | `docs/i18n/vi/troubleshooting.md` | Full tree localized |

## Root README Completeness

Not all root READMEs are full translations of `README.md`:

| Locale | Style | Approximate Coverage |
|---|---|---|
| `en` | Full source | 100% |
| `zh-CN` | Hub-style entry point | ~26% |
| `ja` | Hub-style entry point | ~26% |
| `ru` | Hub-style entry point | ~26% |
| `fr` | Near-complete translation | ~90% |
| `vi` | Near-complete translation | ~90% |

Hub-style entry points provide quick-start orientation and language navigation but do not replicate the full English README content. This is an accurate status record, not a gap to be immediately resolved.

## Collection Index i18n

Localized `README.md` files under collection directories (`docs/getting-started/`, `docs/reference/`, `docs/operations/`, `docs/security/`, `docs/hardware/`, `docs/contributing/`, `docs/project/`) currently exist only for English and Vietnamese. Collection index localization for other locales is deferred.

## Localization Rules

- Keep technical identifiers in English:
  - CLI command names
  - config keys
  - API paths
  - trait/type identifiers
- Prefer concise, operator-oriented localization over literal translation.
- Update "Last refreshed" / "Last synchronized" dates when localized pages change.
- Ensure every localized hub has an "Other languages" section.

## Adding a New Locale

1. Create `README.<locale>.md`.
2. Create canonical docs tree under `docs/i18n/<locale>/` (at least `README.md`, `commands-reference.md`, `config-reference.md`, `troubleshooting.md`).
3. Add locale links to:
   - root language nav in every `README*.md`
   - localized hubs line in `docs/README.md`
   - "Other languages" section in every `docs/README*.md`
   - language entry section in `docs/SUMMARY.md`
4. Optionally add docs-root shim files for backward compatibility.
5. Update this file (`docs/i18n-coverage.md`) and run link validation.

## Review Checklist

- Links resolve for all localized entry files.
- No locale references stale filenames (for example `README.vn.md`).
- TOC (`docs/SUMMARY.md`) and docs hub (`docs/README.md`) include the locale.
