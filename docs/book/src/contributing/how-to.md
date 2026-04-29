# How to Contribute

We accept code, docs, bug reports, and feedback from anyone willing to file them clearly. This page covers the mechanics — how to get a change in, what we look for in review, and what to expect after you open a PR.

See [Communication](./communication.md) for non-code contributions (reporting issues, feedback, getting help).

See [RFC process](./rfcs.md) for larger changes that need design discussion before implementation.

## Before you start

For anything larger than a typo fix:

1. **Check the issue tracker.** Someone may already be working on it or have filed a related discussion.
2. **Read `AGENTS.md`.** The repo's root `AGENTS.md` is the canonical source of convention — risk tiers, PR discipline, anti-patterns, and review standards live there.
3. **Pick a branch.** PRs target `master`. Fork the repo and branch from there; there's no develop/integration branch to go through.

## The flow

```
fork → branch → commit → push → open PR → review → merge (squash)
```

The key checkpoints:

- **PR template** — `.github/pull_request_template.md`. Fill it out. The summary, validation evidence, and compatibility sections are non-negotiable.
- **CI** — runs on every PR. `ci.yml` is the composite gate; all legs must pass.
- **Labels** — scope (`scope:providers`, `scope:channels`, etc.) and risk (`risk:low` / `risk:medium` / `risk:high`) are auto-applied by path-labeler. Double-check they match your change; if not, flag in a comment.
- **Review** — maintainers review. Findings follow `[blocking]` / `[suggestion]` / `[question]` tiers. Address blockers; suggestions are optional; questions need an answer.

## Code style

- `cargo fmt` clean (checked in CI)
- `cargo clippy -D warnings` clean (checked in CI)
- No unused production code — delete it, wire it into behavior, or track a follow-up issue. Do not silence it with underscore prefixes or `#[allow(dead_code)]`; reserve underscore names for required but intentionally unused API, trait, or callback parameters.
- Error handling: `anyhow::Result` at binary boundaries, typed errors in library crates. No `unwrap()` / `expect()` in production code paths — propagate with `?` or document the invariant that makes panic impossible.
- Minimal dependencies — every dep adds to binary size; weigh the trade before adding one
- Trait-first — define the trait in `zeroclaw-api`, then implement in the right edge crate
- Security by default — allowlists, not blocklists. New external surface defaults closed
- Inline unit tests — `#[cfg(test)] mod tests {}` at the bottom of the file or a sibling `tests.rs`
- Don't commit secrets, personal data, or real-user identities — the [Privacy & PII discipline](./privacy.md) page is the merge gate

## Testing

- Unit tests co-located with the code (`mod tests`)
- Integration tests in `tests/` and crate-local unit tests — run via `cargo nextest run --locked --workspace --exclude zeroclaw-desktop`
- Feature-gated code needs feature-gated tests
- Don't mock the database for tests that exercise schema or SQL — integration tests must hit a real SQLite

For the full five-level taxonomy (unit / component / integration / system / live), shared mock infrastructure, and JSON trace fixture format, see [Testing](./testing.md).

## Docs changes

- Prose changes go in `docs/book/src/**/*.md` (this mdBook)
- Rustdoc (`///`) changes update the API reference automatically on deploy
- Reference pages (`docs/book/src/reference/cli.md`, `config.md`) are generated — don't hand-edit. Run `cargo mdbook refs` and commit the output
- Localisation — if you change user-facing strings, run `cargo mdbook sync` to refresh the `.po` files

## Commit messages

Conventional Commits:

```
feat(providers): add support for DeepSeek reasoning mode
fix(channels/matrix): prevent duplicate device sessions after verify
docs(getting-started): add YOLO-mode quick-start
refactor(runtime): split agent loop into steps
chore: bump tokio to 1.43
```

Co-authoring with AI is encouraged; add `Co-Authored-By:` trailers in commit messages where AI tools materially contributed. See FND-005 (Contribution Culture) for the full norm.

## Pull requests

Title mirrors the squash commit:

```
feat(scope): short description
```

Body uses the PR template. **The validation-evidence section is required** — paste the output of `cargo fmt --check`, `cargo clippy`, `cargo test`, plus whatever manual verification you did. "It works on my machine" is not evidence.

Risk labels:

- `risk:low` — rollback is a revert; no user action needed
- `risk:medium` — users may need to update config / env / CLI usage; rollback plan required
- `risk:high` — security-critical, schema changes, breaking behaviour. Rollback plan, feature flag, and observable failure symptoms required

## After the PR

**Merge strategy:** squash-merge with the full commit history preserved in the body. See `.claude/skills/squash-merge/SKILL.md` for the exact format — TL;DR: PR title + `(#number)` as the subject, bullet list of original commits as the body.

**Release:** changes land on `master`; `master` does not auto-release. A maintainer bumps the version and tags `vX.Y.Z` when a release ships. You'll see your PR in the CHANGELOG.

## Areas that want help

| Area | Where to start |
|---|---|
| New channel | `crates/zeroclaw-channels/` — copy an existing channel of similar shape |
| New provider | `crates/zeroclaw-providers/` — `compatible.rs` covers most OpenAI-like ones |
| Docs | `docs/book/src/` — anything marked outdated or missing |
| Translations | `cargo fluent fill --locale <code>` — see [Maintainers → Docs & Translations](../maintainers/docs-and-translations.md) |
| Hardware | `crates/zeroclaw-hardware/` — new board support, new sensor drivers |

## Code of conduct

Don't be a jerk. Disagree on ideas; not people. Accept that maintainers will close things they don't want to own — usually with an explanation, occasionally without. If a close feels unjustified, ask; if the ask goes nowhere, move on.

## See also

- [RFC process](./rfcs.md) — for anything bigger than a patch
- [Communication](./communication.md) — how to reach the team
- [Maintainers → Overview](../maintainers/index.md) — what maintainers do day-to-day
