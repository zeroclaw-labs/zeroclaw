---
name: changelog-generation
description: "Changelog generation skill for ZeroClaw releases. Use this skill when the user wants to generate a changelog, prepare release notes, or summarize what changed between versions. Trigger on: 'generate changelog', 'changelog for v0.7.x', 'prepare release notes', 'what changed since <tag>', 'write the changelog', 'CHANGELOG-next', 'release notes for the next release'."
---

# ZeroClaw Changelog Generation

You are generating a human-friendly `CHANGELOG-next.md` for a ZeroClaw release.
The GitHub CLI (`gh`) is available and authenticated. The local repository is
checked out and up to date.

---

## Before You Start

Read the protocol reference before doing anything else:

- `docs/contributing/changelog-generation.md` — **the full procedure**; follow it
  exactly for every run. It defines the commit range logic, categorisation rules,
  GraphQL contributor resolution, filter lists, output format, and release workflow
  integration.

Do not skip it. Do not rely on memory of a prior run.

---

## Invocation

**Default — last stable tag to HEAD:**
```
generate changelog
write the changelog
CHANGELOG-next
prepare release notes
```

**Explicit range:**
```
changelog for v0.7.2
changelog v0.7.1..v0.7.2
what changed since v0.7.1
release notes v0.7.1 to v0.7.2
```

---

## Workflow

### Phase 1 — Establish the range

1. If the user provided a range, normalise it to `<from>..<to>` per the table in
   the protocol §1.
2. If no range was given, resolve the last stable tag automatically:
   ```bash
   git tag --sort=-creatordate | grep -vE '\-beta\.' | head -1
   ```
3. Verify both refs exist before proceeding. If either is missing, stop and tell
   the user what was not found.
4. Report the resolved range to the user before continuing.

### Phase 2 — Collect and categorise commits

Run all fetches in a single parallel batch — do not wait for one before starting
the next:

```bash
# Full commit list with subjects (for categorisation)
git log <from>..<to> --pretty=format:"%H %h %s" --no-merges

# Full SHAs only (for contributor resolution)
git log <from>..<to> --pretty=format:"%H" --no-merges
```

Categorise every commit per the prefix table in protocol §2. Commits without a
recognised prefix must be read and categorised by content — do not drop them.

### Phase 3 — Resolve contributors

Use the GitHub GraphQL `authors` API per protocol §3. Do not use `git log
--pretty=format:"%an"` alone — it misses `Co-Authored-By` contributors.

Paginate in batches of 100. Cross-reference each `oid` against the SHA list from
Phase 2 to restrict results to the release range. Apply the full filter list from
protocol §3 (bots, noreply addresses, AI model names).

### Phase 4 — Write the changelog

Follow the format spec in protocol §4 exactly:

1. Preamble (2–3 sentences — release theme, scale, reader context)
2. Highlights (4–6 user-visible bullet points)
3. What's New (grouped by area, human-readable sentences, PR references)
4. Bug Fixes (summary table: Area | Fix)
5. Breaking Changes (omit section entirely if none)
6. Contributors (`@login` handles, case-insensitive sort, one per line)
7. Footer (full diff reference)

Write to **two locations**:
- `tmp/CHANGELOG-next.md` — for in-session review before committing
- `CHANGELOG-next.md` in the repository root — the path the release workflows read

### Phase 5 — Review and confirm

Present a summary to the user:

- Range covered
- Commit count by category
- Contributor count
- Any commits that had no conventional prefix (so the user can sanity-check
  categorisation)

Ask the user to review `tmp/CHANGELOG-next.md` before committing. Do not
proceed to Phase 6 without explicit confirmation.

### Phase 6 — Commit and push

Only after the user confirms the content:

```bash
git add CHANGELOG-next.md
git commit -m "chore(release): add CHANGELOG-next.md for vX.Y.Z"
git push upstream <branch>
```

Replace `vX.Y.Z` with the next release version — ask the user if unsure.
Push to the open release PR branch on `zeroclaw-labs/zeroclaw`. Do **not** push
to `master` directly.

---

## Execution rules

1. **Always read `docs/contributing/changelog-generation.md` first.** The protocol
   file is authoritative. If anything in this skill conflicts with it, the protocol
   wins.
2. **Always report the resolved range before doing any work.** The user should
   confirm the range is correct before you collect commits.
3. **Never drop commits silently.** Every commit in the range must be accounted for
   in the output — either surfaced in a section or explicitly noted as omitted (e.g.
   trivial typo fix in docs).
4. **Always use the GraphQL contributor path.** `git log --format="%an"` alone is
   not acceptable — it produces an incomplete contributor list.
5. **Always apply the full filter list.** Bots, noreply addresses, and AI model
   names must be excluded from the contributor section.
6. **Always write to `tmp/CHANGELOG-next.md` first.** The user reviews before the
   file is committed to the repository root.
7. **Always confirm before committing.** Show the user the exact commit message
   and ask for an explicit yes. Do not infer consent from prior steps.
8. **Never push to `master` directly.** Always push to the open release PR branch.
9. **Never delete `CHANGELOG-next.md` manually.** The release workflow deletes it
   automatically after a successful stable release.