## Summary

- **Base branch:** `master` (all contributions)
- **What changed and why:** (2 to 5 bullets; the diff shows *what*, you explain *why*)
- **Scope boundary:** (what this PR explicitly does NOT change)
- **Blast radius:** (what other subsystems or consumers could be affected)
- **Linked issue(s):** Use plain text outside backticks. Use `Closes #`,
  `Fixes #`, or `Resolves #` only for issues this PR fully resolves. Use
  `Related #`, `Depends on #`, or `Supersedes #` for non-closing relationships.
- **Labels:** Snapshot the current GitHub labels after labels are applied, for
  example `type:docs`, `risk:low`, `size:S`, `docs`. During label-spelling
  migration, copy the exact live label spelling from the GitHub UI.

## Testing (required)

### How you can test (when useful)

Include this subsection when reviewer-run manual verification adds useful signal, especially for user-visible behavior, a non-obvious test path, or a named CI coverage gap. For changes without useful manual verification, including docs-only, pure-refactor, or trivial changes, set the first field to `N/A` with a one-line reason and remove the remaining prompts.

When reviewer testing is requested, frame it A/B: the same steps should show the old behavior on `master` and the new behavior on this branch, so the reviewer can see the delta themselves.

- **Reviewer testing requested?** (`Yes` / `N/A`; if `N/A`, one line why)
- **Interface(s) exercised:** Name the surface(s) this touches using the same vocabulary the attribution span records: `surface` (`web` / `tui` / `cli`) and `channel` for messaging surfaces. Match the live attribution values; do not invent interface names.
- **Setup / preconditions:** (config, provider, channel, or state needed first)
- **Steps to run:** (the exact click-through or command sequence)
- **Expected on this branch (after):** (what the reviewer should observe if it works)
- **Prior behavior on `master` (before):** (run the same steps unpatched; what breaks or is missing, so the fix is visible)

### How I tested

Explain how the change was checked. Use the evidence that matches the changed surface: required CI, focused local tests, manual smoke, docs/link gates, or full workspace checks when they prove something narrower evidence would miss. Paste relevant output tails for commands you ran, not "all passed".

Fresh required CI is valid evidence when it covers the changed surface. Add extra validation only for a concrete coverage gap, such as platform-specific tests, cross-platform lint, desktop app coverage, release target builds, or stale/unavailable CI.

```sh
# Rust/code examples; choose the checks that match the changed surface:
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Docs-only changes: replace with markdown lint (`scripts/ci/docs_quality_gate.sh`) and added-link integrity (`scripts/ci/docs_links_gate.sh`). Bootstrap scripts: add `bash -n install.sh`.

- **CI checks relied on and why they cover this change:** (for example, `Docs Style` covers the changed Markdown lines)
- **Known CI coverage gap, if any:** (for example, `None after the docs and links gates`)
- **Commands run and tail output:**
- **Beyond CI, what did you manually verify?** (functional scenarios, edge cases, and any security-relevant behavior; also what you did NOT verify)
- **If any command was intentionally skipped, why:**

## Security & Privacy Impact (required)

Yes/No for each. Answer any `Yes` with a 1-2 sentence risk-and-mitigation note. Manual verification of these scenarios goes under `### How I tested`, not here.

- New permissions, capabilities, or file system access scope? (`Yes/No`)
- New external network calls? (`Yes/No`)
- Secrets / tokens / credentials handling changed? (`Yes/No`)
- PII, real identities, or personal data in diff, tests, fixtures, or docs? (`Yes/No`)
- Prompt injection or untrusted model-visible text introduced/changed? (`Yes/No`)
- If any `Yes`, describe the risk and mitigation:

## Compatibility (required)

- Backward compatible? (`Yes/No`)
- Config / env / CLI surface changed? (`Yes/No`)
- Rust/MSRV/toolchain floor changed? (`Yes/No`)
- If backward compatibility is `No` or either surface/floor question is `Yes`: exact upgrade steps for existing users:

## Rollback (required for medium/high-risk PRs)

Low-risk PRs: `git revert <sha>` is the plan unless otherwise noted.

Medium/high-risk PRs must fill:

- **Fast rollback command/path:**
- **Feature flags or config toggles:** (or `None`)
- **Observable failure symptoms:** (what to grep logs for, which metric moves, which alert fires)

## Supersede Attribution (required only when `Supersedes #` is used)

- Superseded PRs + authors (`#<pr> by @<author>`, one per line):
- Scope materially carried forward:
- `Co-authored-by` trailers added in commit messages for incorporated contributors? (`Yes/No`)
- If `No`, why (inspiration-only, no direct code/design carry-over):

---

**Labels** live in the GitHub label UI, not in the body. Maintainers and reviewers with label permissions set `risk:*`, `size:*`, and any missing manual labels via the sidebar. The PR path labeler only owns path/scope labels from `.github/labeler.yml`. Contributors without label permission can note obvious label mismatches in a comment. Canonical colon-scoped labels use no-space spelling; during migration, copy exact live label spelling from the GitHub UI.

**Do not add bot/AI attribution footers** such as `Co-authored-by: Claude ...`
or `Created with Claude Code` to the PR body or commit-message tail. Human
co-author trailers are appropriate only for incorporated contributor work under
the supersede-attribution section and privacy contract.

**Privacy contract** (`docs/book/src/contributing/privacy.md`) is a merge gate. Never commit real identities, secrets, personal emails, or PII in diff, tests, fixtures, or docs.
