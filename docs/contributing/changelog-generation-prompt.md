# Changelog Generation Prompt

Use this prompt with any AI coding assistant that has access to the GitHub CLI
and the local repository to generate a human-friendly `CHANGELOG-next.md` for
the next release.

The release workflow (`release-beta-on-push.yml` and `release-stable-manual.yml`)
will automatically use `CHANGELOG-next.md` as the GitHub release body if it exists,
replacing the auto-generated feat-only notes. After a stable release the workflow
deletes the file automatically.

---

## The Prompt

```
You are generating a human-friendly changelog for the ZeroClaw project.
The GitHub CLI (`gh`) is available and authenticated.
The local repository is at `/home/warewolf/GitHub/zeroclaw`.

## Steps

### 1. Find the last stable release tag

```bash
git tag --sort=-creatordate | grep -vE '\-beta\.' | head -1
```

### 2. Collect all commits since that tag

```bash
git log <tag>..HEAD --pretty=format:"%h %s" --no-merges
```

Save the full SHA list for use in step 3:

```bash
git log <tag>..HEAD --pretty=format:"%H" --no-merges > /tmp/commits.txt
```

### 3. Resolve the full contributor list via the GitHub GraphQL API

Do NOT use `git log --pretty=format:"%an"` alone — it misses everyone listed in
`Co-Authored-By` trailers. Use the GraphQL `authors` field, which GitHub resolves
for both direct authors and co-authors.

Paginate in batches of 100. For each commit in the range, collect every
`authors.nodes[].user.login`. Filter out:
- Bots: any login ending in `[bot]`, `web-flow`, `dependabot`, `github-actions`,
  `copilot`, `blacksmith`
- AI agents: any email containing `noreply@anthropic.com` or similar

Example query (paginate using `pageInfo.endCursor` if `hasNextPage` is true):

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

Cross-reference each `oid` against `/tmp/commits.txt` to include only commits
in the release range. Sort logins case-insensitively and prefix each with `@`.

### 4. Write CHANGELOG-next.md

Structure the file as follows:

---

#### Preamble (2–3 sentences)

Briefly describe what kind of release this is — the theme, the scale of change,
anything a reader skimming the title needs to understand before reading the rest.

---

#### ## Highlights

4–6 bullet points. Each one should be something a non-technical reader or a user
would care about. Lead with the user-visible impact, not the implementation detail.

---

#### ## What's New

Group entries by area. Use only the groups that have content. Suggested groups:

- **Architecture & Workspace**
- **Providers**
- **Channels**
- **Configuration**
- **Web Dashboard**
- **Agent & Runtime**
- **Skills**
- **Security**
- **Installation & Distribution**
- **Dependencies & Security Advisories**

Write each entry as a sentence for a human reader, not a raw commit message.
Reference PR numbers with `(#NNNN)` where available. Do not list every fix —
save those for the Bug Fixes table.

Commits without conventional prefixes (`feat:`/`fix:` etc.) should still be
read and categorised by their content. Do not silently drop them.

---

#### ## Bug Fixes

A summary table. Columns: `Area` | `Fix`. Collapse multiple fixes for the same
feature into one row where that reads more clearly than separate rows.

---

#### ## Breaking Changes

Call out every breaking change explicitly with a migration path. Look for:
- Config schema changes
- Deprecated CLI subcommands or flags
- Renamed config fields
- Crate boundary or public API changes

If there are no breaking changes, omit this section entirely.

---

#### ## Contributors

GitHub `@login` handles from step 3, sorted case-insensitively, one per line.

---

#### Footer

```
*Full diff: `git log <prev-tag>..HEAD --oneline`*
```

---

### 5. Commit and push

```bash
git add CHANGELOG-next.md
git commit -m "chore(release): add CHANGELOG-next.md for vX.Y.Z"
git push upstream <branch>
```

Push to `upstream/<branch>` — the PR branch on `zeroclaw-labs/zeroclaw`, not
your fork — so the commit appears on the open release PR.

---

## Notes

- The release workflow only surfaces `feat:` commits in its auto-generated notes.
  This changelog should also cover significant `fix:`, `refactor:`, and security
  work — that is the whole point of writing it by hand.
- AI model names in `Co-Authored-By` trailers (`Claude`, `Copilot`, etc.) are not
  contributors. Filter them.
- If the commit range spans more than 100 commits, paginate the GraphQL query
  using `pageInfo.endCursor`.
- Once the stable release workflow runs, it will automatically delete
  `CHANGELOG-next.md` and commit the removal to `master`. You do not need to
  clean it up manually.
```
