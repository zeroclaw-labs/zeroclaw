---
name: wit-breaking-change-check
description: "Classify WIT interface changes as breaking or non-breaking against frozen version markers. Use this skill when the user wants to check WIT breaking changes, review a WIT diff, verify whether a WIT change is breaking, or run a WIT compat check. Trigger on: 'check WIT breaking changes', 'review WIT diff', 'is this WIT change breaking', 'WIT compat check'."
---

# WIT Breaking Change Check

Classifies every modification in the current WIT diff against the breaking-change taxonomy and reports a verdict for each finding.

## When to Use

- Before merging any branch that touches `wit/`
- When reviewing a PR that modifies WIT interface definitions
- To verify a WIT change is safe before publishing a plugin-compatible release

## Procedure

1. Run `git diff origin/master -- wit/` to obtain the current diff.
2. For each `wit/vN/` directory in the diff, check whether `wit/vN/.frozen` exists. If absent, report the version as experimental and skip it.
3. For each frozen version with changes, classify every modification against the breaking-change taxonomy in `wit/VERSIONING.md`:
   - **Breaking**: removing/renaming any type, function, record field, or variant case; changing a function signature; changing a field type; reordering record fields; adding a required (non-optional) field to an existing record; adding a non-capability-gated required function to an existing interface.
   - **Non-breaking**: new `flags` bits, new capability-gated functions, new record/variant/enum types, new interfaces, new worlds, `@since`/`@unstable` annotation additions.
4. Report each finding with a verdict:
   - ✅ Non-breaking — with a brief reason citing the taxonomy
   - ❌ Breaking — with a brief reason citing the taxonomy
   - ⚠️ Uncertain — with the ambiguity explained
5. If any breaking change is found, summarize the required migration path for plugin authors.

## Notes

The `.frozen` marker is a human-readable convention: its presence signals to reviewers and this skill that the version is stable and requires the breaking-change check before merge. Experimental (unfrozen) versions are skipped without a verdict.
