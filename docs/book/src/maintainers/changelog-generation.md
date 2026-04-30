# Changelog Generation

The authoritative procedure for assembling `CHANGELOG-next.md` between stable releases. This page is loaded by the `changelog-generation` skill and read by maintainers running a release manually — both consume the same protocol.

The release workflows (`release-stable-manual.yml`) automatically use `CHANGELOG-next.md` as the GitHub Release body if it's at the repo root when a release fires. After the stable release ships, the workflow deletes the file. No manual cleanup needed.

## 1. Establish the commit range

### Default: last stable tag → HEAD

```bash
PREV_TAG=$(git tag --sort=-creatordate | grep -vE '\-beta\.' | head -1)
echo "Range: ${PREV_TAG}..HEAD"
```

### User-specified range

Accept any of the following and normalize to `<from>..<to>`:

| Input | Interpretation |
|---|---|
| `v0.7.2` | `v0.7.2..HEAD` |
| `v0.7.1..v0.7.2` | Exactly as given |
| `v0.7.1 v0.7.2` | `v0.7.1..v0.7.2` |

Verify both refs exist before proceeding:

```bash
git rev-parse --verify <ref> 2>/dev/null || echo "ERROR: ref not found"
```

## 2. Collect and categorise commits

### Collect

```bash
git log <from>..<to> --pretty=format:"%H %h %s" --no-merges
```

Save full SHAs for the contributor resolution step:

```bash
git log <from>..<to> --pretty=format:"%H" --no-merges > /tmp/zc-commits.txt
```

### Categorise

Map each commit to a section by its conventional commit prefix. Commits without a recognized prefix must still be read and categorized by content — never silently drop them.

| Prefix | Section |
|---|---|
| `feat:`, `feat(*)` | What's New |
| `fix:`, `fix(*)` | Bug Fixes |
| `refactor:`, `perf:` | What's New (group as "Improvements") |
| `security:`, `fix(*security*)` | What's New → Security |
| `docs:`, `docs(*)` | What's New → Documentation (omit trivial typo fixes) |
| `chore:`, `ci:`, `build:` | Omit unless user-visible (new install path, dropped platform, etc.) |
| `breaking:` or `!` suffix | Breaking Changes — always surface |
| No prefix | Read body; categorize by content; note in review |

### Section ordering in the output file

1. Preamble
2. Highlights
3. What's New
4. Bug Fixes
5. Breaking Changes (omit if empty)
6. Contributors

## 3. Contributor resolution

Do **not** use `git log --pretty=format:"%an"` alone — it misses everyone listed in `Co-Authored-By` trailers. Use the GitHub GraphQL `authors` field, which resolves direct authors and co-authors.

### Query

Paginate in batches of 100 commits. Use `pageInfo.endCursor` while `hasNextPage` is `true`.

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

Cross-reference each `oid` from the GraphQL response against `/tmp/zc-commits.txt` to include only commits within the release range. Collect unique logins, sort case-insensitively, prefix each with `@`.

## 4. CHANGELOG-next.md format

### Preamble

Two or three sentences. Describe the release theme, scale, and anything a reader skimming the title needs before reading on. Write for a non-technical reader.

### Highlights

Four to six bullets. Lead with user-visible impact, not implementation detail. Each bullet should answer: *"What can I do now that I couldn't before?"* or *"What just got better?"*

### What's New

Group entries by area. Use only groups that have content.

Suggested groups (add or omit freely):

- Architecture & Workspace
- Agent & Runtime
- Providers
- Channels
- Tools
- Configuration
- Web Dashboard
- Skills
- Security
- Hardware
- Installation & Distribution
- Dependencies & Security Advisories

Write each entry as a sentence for a human reader — not a raw commit message. Reference PR numbers with `(#NNNN)` where available.

### Bug Fixes

A summary table. Columns: `Area` | `Fix`. Collapse multiple fixes for the same feature into one row when that reads more clearly than separate rows.

### Breaking Changes

Call out every breaking change with a migration path. Look for:

- Config schema changes (renamed or removed fields)
- Deprecated or renamed CLI subcommands or flags
- Crate boundary or public API surface changes
- Behavior changes behind existing config keys

If there are no breaking changes, omit this section entirely.

### Contributors

`@login` handles from step 3, sorted case-insensitively, one per line.

### Footer

```
*Full diff: `git log <prev-tag>..<next-tag> --oneline`*
```

## 5. Output and release workflow integration

### Write location

Write to `CHANGELOG-next.md` at the repository root — that's the path the release workflows look for. A copy also lands at `tmp/CHANGELOG-next.md` for in-session review before committing.

### Commit

```bash
git add CHANGELOG-next.md
git commit -m "chore(release): add CHANGELOG-next.md for vX.Y.Z"
```

Replace `vX.Y.Z` with the next release version. Ask the user for confirmation before committing.

### Push

Push to the open release PR branch on `zeroclaw-labs/zeroclaw`:

```bash
git push upstream <branch>
```

Don't push directly to `master`.

### Workflow consumption

`release-stable-manual.yml` checks for `CHANGELOG-next.md` at the start of the release job. If found, its content becomes the GitHub Release body. If not found, the workflow falls back to auto-generated `feat:`-only notes.

After a successful stable release, the workflow automatically deletes `CHANGELOG-next.md` and commits the removal. No manual cleanup is required.
