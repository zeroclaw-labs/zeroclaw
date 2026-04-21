# Changelog Generation — Protocol Reference

This document is the authoritative procedure for generating a human-friendly
`CHANGELOG-next.md` between stable releases. It is loaded and executed by the
`.claude/skills/changelog-generation/SKILL.md` skill.

The release workflows (`release-beta-on-push.yml` and `release-stable-manual.yml`)
automatically use `CHANGELOG-next.md` as the GitHub release body if it exists at
release time, replacing the auto-generated `feat:`-only notes. After a stable release
ships the workflow deletes the file automatically — no manual cleanup needed.

---

## Section 1 — Establish the commit range

### Default: last stable tag → HEAD

```bash
PREV_TAG=$(git tag --sort=-creatordate | grep -vE '\-beta\.' | head -1)
echo "Range: ${PREV_TAG}..HEAD"
```

### User-specified range

Accept any of the following forms and normalise to `<from>..<to>`:

| Input form | Interpretation |
|---|---|
| `v0.7.2` | `v0.7.2..HEAD` |
| `v0.7.1..v0.7.2` | Exactly as given |
| `v0.7.1 v0.7.2` | `v0.7.1..v0.7.2` |

Verify both refs exist before proceeding:

```bash
git rev-parse --verify <ref> 2>/dev/null || echo "ERROR: ref not found"
```

---

## Section 2 — Collect and categorise commits

### Collect

```bash
git log <from>..<to> --pretty=format:"%H %h %s" --no-merges
```

Save full SHAs for the contributor resolution step:

```bash
git log <from>..<to> --pretty=format:"%H" --no-merges > /tmp/zc-commits.txt
```

### Categorise

Map each commit to a section by its conventional commit prefix. Commits without
a recognised prefix must still be read and categorised by content — do not
silently drop them.

| Prefix(es) | Section |
|---|---|
| `feat:`, `feat(*)` | What's New |
| `fix:`, `fix(*)` | Bug Fixes |
| `refactor:`, `perf:` | What's New (group as "Improvements") |
| `security:`, `fix(*security*)` | What's New → Security |
| `docs:`, `docs(*)` | What's New → Documentation (omit trivial typo fixes) |
| `chore:`, `ci:`, `build:` | Omit unless user-visible (e.g. new install path, dropped platform) |
| `breaking:` or `!` suffix | Breaking Changes — always surface these |
| No prefix | Read body; categorise by content; note in review |

### Section ordering in the output file

1. Preamble
2. Highlights
3. What's New
4. Bug Fixes
5. Breaking Changes (omit section entirely if empty)
6. Contributors

---

## Section 3 — Contributor resolution

Do **not** use `git log --pretty=format:"%an"` alone — it misses everyone
listed in `Co-Authored-By` trailers. Use the GitHub GraphQL `authors` field,
which resolves both direct authors and co-authors.

### Query

Paginate in batches of 100 commits. Use `pageInfo.endCursor` when
`hasNextPage` is true.

```graphql
{
  repository(owner: "zeroclaw-labs", name: "zeroclaw") {
    ref(qualifiedName: "refs/heads/master") {
      target {
        ... on Commit {
          history(first: 100) {
            pageInfo { hasNextPage endCursor }
            nodes {
              oid
              authors(first: 10) {
                nodes {
                  user { login }
                  email
                }
              }
            }
          }
        }
      }
    }
  }
}
```

Run via `gh`:

```bash
gh api graphql -f query='<query>'
```

### Filter list — exclude all of the following

**By login pattern:**
- Any login ending in `[bot]`
- `web-flow`
- `dependabot`
- `github-actions`
- `blacksmith`

**By email pattern:**
- `*@noreply.github.com`
- `*@noreply.anthropic.com`
- `*noreply*`

**AI model names appearing as author names (not logins):**
- `Claude`, `Copilot`, `ChatGPT`, `Codex`, `Gemini`, `GitHub Copilot`
- Any name matching `^(gpt|claude|gemini|copilot)-`

### Output

Cross-reference each `oid` from the GraphQL response against `/tmp/zc-commits.txt`
to include only commits within the release range. Collect unique logins, sort
case-insensitively, prefix each with `@`.

---

## Section 4 — CHANGELOG-next.md format

### Preamble

2–3 sentences. Describe the release theme, scale, and anything a reader skimming
the title needs before reading on. Write for a non-technical reader.

### Highlights

4–6 bullet points. Lead with user-visible impact, not implementation detail.
Each bullet should answer: *"What can I do now that I couldn't before?"* or
*"What just got better?"*

### What's New

Group entries by area. Use only groups that have content.

Suggested groups (add or omit freely):

- **Architecture & Workspace**
- **Agent & Runtime**
- **Providers**
- **Channels**
- **Tools**
- **Configuration**
- **Web Dashboard**
- **Skills**
- **Security**
- **Hardware**
- **Installation & Distribution**
- **Dependencies & Security Advisories**

Write each entry as a sentence for a human reader — not a raw commit message.
Reference PR numbers with `(#NNNN)` where available.

### Bug Fixes

A summary table. Columns: `Area` | `Fix`.
Collapse multiple fixes for the same feature into one row where that reads
more clearly than separate rows.

### Breaking Changes

Call out every breaking change with a migration path. Look for:

- Config schema changes (renamed or removed fields)
- Deprecated or renamed CLI subcommands/flags
- Crate boundary or public API surface changes
- Behaviour changes behind existing config keys

If there are no breaking changes, omit this section entirely.

### Contributors

`@login` handles from Section 3, sorted case-insensitively, one per line.

### Footer

```
*Full diff: `git log <prev-tag>..<next-tag> --oneline`*
```

---

## Section 5 — Output and release workflow integration

### Write location

Write to `CHANGELOG-next.md` in the repository root (not `tmp/`) — this is
the path the release workflows look for.

A copy is also written to `tmp/CHANGELOG-next.md` for in-session review before
committing.

### Commit

```bash
git add CHANGELOG-next.md
git commit -m "chore(release): add CHANGELOG-next.md for vX.Y.Z"
```

Replace `vX.Y.Z` with the next release version. Ask the user for confirmation
before committing.

### Push

Push to the open release PR branch on `zeroclaw-labs/zeroclaw`:

```bash
git push upstream <branch>
```

Do **not** push directly to `master`.

### Workflow consumption

`release-beta-on-push.yml` and `release-stable-manual.yml` both check for
`CHANGELOG-next.md` at the start of the release job. If found, its content
becomes the GitHub Release body. If not found, the workflow falls back to
auto-generated `feat:`-only notes.

After a successful stable release the workflow automatically deletes
`CHANGELOG-next.md` and commits the removal. No manual cleanup is required.