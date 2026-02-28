# Documentation Audit Snapshot (2026-02-24)

This snapshot records a deep documentation audit focused on completeness, navigation clarity, and i18n structure.

Date: **2026-02-24**
Scope: repository docs (`docs/**`) + root README locale entry points.

## 1) Audit Method

- Ran structural inventory over all markdown docs.
- Checked README presence for doc directories.
- Checked relative-link integrity across all docs markdown files.
- Reviewed canonical vs compatibility locale path usage.
- Reviewed TOC/inventory/structure-map consistency.

## 2) Findings

### A. Structural clarity gaps

- Canonical locale trees existed under `docs/i18n/<locale>/`, but some governance docs still described older hub layout.
- `docs/vi/**` compatibility tree coexisted with `docs/i18n/vi/**`, creating maintenance ambiguity.
- `datasheets` directories lacked explicit index files (`README.md`), reducing discoverability.

### B. Completeness gaps

- Several operational/reference docs were not clearly surfaced in inventory/summary pathways (for example `audit-event-schema`, `proxy-agent-playbook`, `cargo-slicer-speedup`, `sop/*`, `operations/connectivity-probes-runbook`).
- Locale coverage status existed, but there was no explicit time-bound audit snapshot documenting current gaps and priorities.

### C. Integrity issues

- Link check found broken relative links:
  - `docs/i18n/el/cargo-slicer-speedup.md` -> workflow path depth issue
  - `docs/vi/README.md` -> missing `SUMMARY.md` in compatibility path
  - `docs/vi/reference/README.md` -> missing `../SUMMARY.md`

## 3) Remediation Applied

### 3.1 Navigation and governance

- Added and linked i18n completion contract in previous phase: `docs/i18n-guide.md`.
- Refreshed structure map with canonical layers and compatibility boundaries: `docs/structure/README.md`.
- Refreshed inventory to include canonical locale hubs, SOP, CI/security references, and audit snapshot: `docs/docs-inventory.md`.

### 3.2 Directory completeness

Added missing datasheet indexes:

- `docs/datasheets/README.md`
- `docs/i18n/vi/datasheets/README.md`
- `docs/i18n/el/datasheets/README.md`
- `docs/vi/datasheets/README.md` (compatibility redirect)

### 3.3 Compatibility cleanup

- Converted `docs/vi/README.md` to an explicit compatibility hub pointing to canonical `docs/i18n/vi/**`.
- Converted `docs/vi/reference/README.md` to canonical redirect.

### 3.4 Broken link fixes

- Fixed Greek CI workflow relative link path.
- Eliminated compatibility README broken links by redirecting to canonical paths.

## 4) Current Known Remaining Gaps

These are structural/content-depth gaps, not integrity failures:

1. Locale depth asymmetry
- `vi`/`el` have full localized trees.
- `zh-CN`/`ja`/`ru`/`fr` currently provide hub-level scaffolds rather than full runtime-contract localization.

2. Compatibility shim lifecycle
- `docs/vi/**` still exists for backward links; long-term plan should define whether to keep or fully deprecate this mirror.

3. Localized propagation of new governance docs
- New governance docs (for example this audit snapshot and i18n guide) are currently authored in English-first flow; localized summaries are not yet fully propagated.

## 5) Recommended Next Wave

1. Add locale-level mini-inventory pages under `docs/i18n/{zh-CN,ja,ru,fr}/` to make hub scaffolds more actionable.
2. Define and document a formal deprecation policy for `docs/vi/**` compatibility paths.
3. Add a lightweight automated docs index consistency check in CI (summary/inventory cross-link sanity).

## 6) Validation Status

- Relative-link existence check: passed after fixes.
- `git diff --check`: clean.

This snapshot is immutable context for the 2026-02-24 docs restructuring pass.

## Addendum (Phase-2 Deep Completion)

After the initial restructuring pass, a second completion wave was applied in the same date scope:

- Added localized bridge coverage so `docs/i18n/vi/` and `docs/i18n/el/` reach full top-level docs parity (against `docs/*.md` baseline).
- Added explicit i18n gap backlog tracker: [i18n-gap-backlog.md](i18n-gap-backlog.md).
- Added localized references for i18n governance docs (`i18n-guide`, `i18n-coverage`) and latest docs audit snapshot under `vi` and `el`.
- Updated localized hubs and summaries (`docs/i18n/vi/*`, `docs/i18n/el/*`) to expose newly added docs and governance links.

Current depth asymmetry remains for `zh-CN` / `ja` / `ru` / `fr` by design (hub-level scaffolds), now explicitly tracked with counts and wave plans in the backlog.
