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
2. For each `wit/vN/` directory in the diff, check whether `wit/vN/.frozen` exists. If absent, report the version as experimental and state that components must be rebuilt against the WIT shipped by the target host; do not claim the frozen-version compatibility window applies.
3. For each frozen version with changes, classify every modification against the breaking-change taxonomy in `wit/VERSIONING.md`:
   - **Breaking**: removing/renaming any type, function, record field, enum case, or variant case; adding a case to an existing enum or variant; changing a function signature; changing a field type; reordering record fields; adding a required (non-optional) field to an existing record; adding a non-capability-gated required function to an existing interface.
   - **Non-breaking**: new `flags` bits, new capability-gated functions, new record/variant/enum types (but not cases added to an existing enum or variant), new interfaces, new worlds, `@since`/`@unstable` annotation additions.
4. Report each finding with a verdict:
   - ✅ Non-breaking — with a brief reason citing the taxonomy
   - ❌ Breaking — with a brief reason citing the taxonomy
   - ⚠️ Uncertain — with the ambiguity explained
5. If any breaking change is found, summarize the required migration path for plugin authors. For an unfrozen experimental version, state that components must be rebuilt against the target host's shipped WIT; the current-and-previous-major host compatibility window begins only after a version directory is frozen.

## Notes

The `.frozen` marker is a human-readable convention: its presence signals to reviewers and this skill that the version is stable and requires the breaking-change check before merge. For experimental (unfrozen) versions, report the target-host rebuild requirement without assigning a frozen-version compatibility verdict.
