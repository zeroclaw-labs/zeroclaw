# Documentation System Contract

Treat documentation as a first-class product surface, not a post-merge artifact.

## Canonical Entry Points

- root READMEs: `README.md`, `README.zh-CN.md`, `README.ja.md`, `README.ru.md`, `README.fr.md`, `README.vi.md`
- docs hubs: `docs/README.md`, `docs/README.zh-CN.md`, `docs/README.ja.md`, `docs/README.ru.md`, `docs/README.fr.md`, `docs/README.vi.md`
- unified TOC: `docs/SUMMARY.md`

## Supported Locales

`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`

## Collection Indexes

- `docs/setup-guides/README.md`
- `docs/reference/README.md`
- `docs/ops/README.md`
- `docs/security/README.md`
- `docs/hardware/README.md`
- `docs/contributing/README.md`
- `docs/maintainers/README.md`

## Governance Rules

- Keep README/hub top navigation and quick routes intuitive and non-duplicative.
- Keep entry-point parity across all supported locales when changing navigation architecture.
- If a change touches docs IA, runtime-contract references, or user-facing wording in shared docs, perform i18n follow-through for supported locales in the same PR:
  - Update locale navigation links (`README*`, `docs/README*`, `docs/SUMMARY.md`).
  - Update localized runtime-contract docs where equivalents exist.
  - For Vietnamese, treat `docs/vi/**` as canonical.
- Keep proposal/roadmap docs explicitly labeled; avoid mixing proposal text into runtime-contract docs.
- Keep project snapshots date-stamped and immutable once superseded by a newer date.
