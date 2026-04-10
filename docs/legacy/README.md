# docs/legacy — Documentation Archive

This directory is a staging archive created as part of the documentation
restructure described in [RFC #5576 — Intentional Documentation](../proposals/documentation-standards.md).

## What this directory is

Every document from the previous `docs/` tree has been moved here intact.
Nothing was deleted. This is a deliberate fallback: if any document is
needed before it has been promoted to the new structure or migrated to the
GitHub Wiki, it is here.

## What this directory is NOT

This is not permanent storage. Every item here has a disposition recorded
in [MANIFEST.md](./MANIFEST.md):

- **`repo:[path]`** — will be promoted to its target path in the new
  `docs/` structure once assessed and validated against the current codebase.
- **`wiki:[section]`** — will be migrated to the GitHub Wiki as operational
  content that does not version with the code.
- **`delete`** — superseded, obsolete, or replaced by a better artifact.
  Will be removed once confirmed safe to drop.

## Do not edit content here

If you find a document in `docs/legacy/` that needs updating:

1. Check the MANIFEST to see where it is headed.
2. If it is destined for the repo, make the fix in the promoted copy (or
   open an issue if it has not been promoted yet).
3. If it is destined for the Wiki, note the needed correction in a comment
   on the tracking issue for RFC #5576.

Opening a PR that edits files under `docs/legacy/` will be declined —
the content here is intentionally frozen pending assessment.

## Tracking

- RFC: [#5576](https://github.com/zeroclaw-labs/zeroclaw/issues/5576)
- Migration checklist: [MANIFEST.md](./MANIFEST.md)
- Phase 1 PR (archive move): this PR
- Phase 2–3 PR (assessment, alignment, restructure): `Depends on #5559`
