# Contributing to ZeroClaw

Thanks for your interest. Every kind of contribution helps — code, docs, bug reports, design feedback. This file is the first stop; the full contributor guide lives in the [docs book](docs/book/src/contributing/how-to.md).

---

## ⚠️ Branch Migration Notice (March 2026)

**`master` is the ONLY default branch. The `main` branch no longer exists.**

If you have an existing fork or local clone that tracks `main`, update it:

```bash
git checkout master
git branch -D main 2>/dev/null          # delete local main if it exists
git remote set-head origin master
git fetch origin --prune                 # remove stale remote refs

# If your fork still has a main branch, delete it
git push origin --delete main 2>/dev/null
```

All PRs target **`master`**. PRs targeting `main` will be rejected.

---

## First-time contributors

1. **Find an issue.** Look for [`good first issue`](https://github.com/zeroclaw-labs/zeroclaw/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) labels — these are scoped for newcomers and include enough context to get moving.
2. **Pick a small scope.** Typo fixes, doc improvements, test additions, and small bug fixes are the fastest path to a merged PR.
3. **Fork → branch → change → test → PR.** PRs target `master`. Use `feat/*` or `fix/*` branch names.
4. **Open a draft PR early** if you get stuck and ask questions in the description.

For the full mechanics — code style, testing levels, PR template requirements, review process — see **[How to contribute](docs/book/src/contributing/how-to.md)**.

## Development setup

```bash
# Clone
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# Enable the pre-push hook (runs fmt, clippy, tests before every push)
git config core.hooksPath .githooks

# Build and test
cargo build
cargo test --locked

# Format and lint (required before PR)
./scripts/ci/rust_quality_gate.sh

# Full CI parity in Docker
./dev/ci.sh all
```

Pre-push hook opt-ins (set the env var to enable for one push):

| Variable | Effect |
|---|---|
| `ZEROCLAW_STRICT_LINT=1` | Strict lint pass on the full repo |
| `ZEROCLAW_STRICT_DELTA_LINT=1` | Strict lint on changed Rust lines only |
| `ZEROCLAW_DOCS_LINT=1` | Markdown gate on changed lines |
| `ZEROCLAW_DOCS_LINKS=1` | Link check on added links only |

Skip the hook for rapid iteration with `git push --no-verify`. CI runs the same checks regardless.

## Local secret management

ZeroClaw supports layered secret management for local development.

**Storage options:**

1. **Environment variables** (recommended for development) — copy `.env.example` to `.env` and fill in values. `.env` is git-ignored.
2. **Config file** (`~/.zeroclaw/config.toml`) — when `secrets.encrypt = true` (default), values are encrypted with the key at `~/.zeroclaw/.secret_key`. Use `zeroclaw onboard` for guided setup.

**API key resolution order:**

1. Explicit key passed from config or CLI.
2. Provider-specific env vars (`OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, …).

Set credentials in your config file (`~/.zeroclaw/config.toml` by default; custom workspaces override the path) under `[providers.models.<type>.<alias>]`. Generic env-var fallbacks (`ZEROCLAW_API_KEY`, `API_KEY`) and the V1/V2 `ZEROCLAW_PROVIDER` / `PROVIDER` / `ZEROCLAW_MODEL` overrides were removed in V3.

**Never commit:** `.env`, API keys / tokens / passwords / OAuth tokens / webhook signing secrets, `~/.zeroclaw/.secret_key`, or any personal identifier in tests or fixtures. The full content discipline is in **[Privacy & PII](docs/book/src/contributing/privacy.md)**.

**Pre-commit secret scan.** `.githooks/pre-commit` runs `gitleaks protect --staged --redact` when `gitleaks` is installed; if it's not installed, the hook prints a warning and continues. Install one of:

- [gitleaks](https://github.com/gitleaks/gitleaks) (what the hook uses)
- [trufflehog](https://github.com/trufflesecurity/trufflehog)
- [git-secrets](https://github.com/awslabs/git-secrets)

Quick manual audit of the staged diff:

```bash
git diff --cached | grep -iE '(api[_-]?key|secret|token|password|bearer|sk-)'
git status --short | grep -E '\.env$'
```

**If a secret lands in a commit by accident:** rotate the credential immediately, then purge history with `git filter-repo` or BFG and force-push (coordinate with maintainers). `git revert` alone is not enough — history still contains the secret. Also remove the leaked value from any PR, issue, discussion, or comment that quoted it.

## Where to find everything else

The book is the source of truth for everything contributor-facing. Quick links:

| Topic | Page |
|---|---|
| The full contribution flow | [How to contribute](docs/book/src/contributing/how-to.md) |
| Communication channels | [Communication](docs/book/src/contributing/communication.md) |
| Filing an RFC | [RFC process](docs/book/src/contributing/rfcs.md) |
| Privacy & PII rules | [Privacy](docs/book/src/contributing/privacy.md) |
| Testing taxonomy | [Testing](docs/book/src/contributing/testing.md) |
| PR review protocol | [PR review protocol](docs/book/src/contributing/pr-review-protocol.md) |
| Trait extension examples | [Extension examples](docs/book/src/developing/extension-examples.md) |
| Contributor License Agreement | [CLA](docs/book/src/contributing/cla.md) |
| Project foundations (governance, culture, infrastructure) | [Foundations](docs/book/src/foundations/README.md) |

For maintainer-facing content (PR workflow, reviewer playbook, release runbook, labels), see the [Maintainers section](docs/book/src/maintainers/index.md).

## Quick rules

- **One concern per PR.** Avoid mixing refactor + feature + infra.
- **Small PRs first.** Prefer `XS/S/M`. Split large work into stacked PRs.
- **Template is mandatory.** Complete every section in `.github/pull_request_template.md`.
- **Validation evidence is required** — actual command output, not "CI will check."
- **Privacy and identity discipline is a merge gate.** Never commit real names, emails, tokens, or PII.
- **Co-authoring with AI is welcome.** Use the `Co-Authored-By` trailer where AI tools materially contributed.
- **Squash-merge with conventional commits** is the merge style.

## Reporting

- **Bugs** — use the bug template; include OS, `zeroclaw --version`, and `zeroclaw doctor` output.
- **Features** — use the feature template; focus on use case and constraints.
- **Security** — see `SECURITY.md` for responsible disclosure. Do not file public issues for vulnerabilities.

## License

Dual-licensed: [MIT](LICENSE-MIT) OR [Apache 2.0](LICENSE-APACHE). Contributors automatically grant rights under both — see the [CLA](docs/book/src/contributing/cla.md).

By submitting a contribution you agree to the CLA. No separate signature required.
