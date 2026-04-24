## Summary

- **Base branch:** `master` (all contributions)
- **What changed and why:** (2–5 bullets — the diff shows *what*, you explain *why*)
- **Scope boundary:** (what this PR explicitly does NOT change)
- **Blast radius:** (what other subsystems or consumers could be affected)
- **Linked issue(s):** `Closes #`, `Related #`, `Depends on #` (stacked), `Supersedes #` (replacing older PR)

## Validation Evidence (required)

Local validation is the signal CI cannot replace. Run the full battery and paste literal output (tails, failures, warnings — not "all passed").

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Docs-only changes: replace with markdown lint + link-integrity (`scripts/ci/docs_quality_gate.sh`). Bootstrap scripts: add `bash -n install.sh`.

- **Commands run and tail output:**
- **Beyond CI — what did you manually verify?** (scenarios, edge cases, what you did NOT verify)
- **If any command was intentionally skipped, why:**

## Security & Privacy Impact (required)

Yes/No for each. Answer any `Yes` with a 1–2 sentence explanation.

- New permissions, capabilities, or file system access scope? (`Yes/No`)
- New external network calls? (`Yes/No`)
- Secrets / tokens / credentials handling changed? (`Yes/No`)
- PII, real identities, or personal data in diff, tests, fixtures, or docs? (`Yes/No`)
- If any `Yes`, describe the risk and mitigation:

## Compatibility (required)

- Backward compatible? (`Yes/No`)
- Config / env / CLI surface changed? (`Yes/No`)
- If `No` or `Yes` to either: exact upgrade steps for existing users:

## Rollback (required for `risk: medium` and `risk: high`)

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

**Labels** live in the GitHub label UI, not in the body. Set `risk:*`, `size:*`, and scope labels via the sidebar. Auto-label corrections: add `risk: manual` and the intended label.

**Commit trailers** capture AI-assisted collaboration (`Co-Authored-By: Claude ...`) — no separate section needed.

**Privacy contract** (`docs/contributing/pr-discipline.md`) is a merge gate. Never commit real identities, secrets, personal emails, or PII in diff, tests, fixtures, or docs.
