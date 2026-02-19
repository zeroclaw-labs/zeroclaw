## Summary

Describe this PR in 2-5 bullets:

- Problem:
- Why it matters:
- What changed:
- What did **not** change (scope boundary):

## Label Snapshot (required)

- Risk label (`risk: low|medium|high`):
- Size label (`size: XS|S|M|L|XL`, auto-managed/read-only):
- Scope labels (`core|agent|channel|config|cron|daemon|doctor|gateway|health|heartbeat|integration|memory|observability|onboard|provider|runtime|security|service|skillforge|skills|tool|tunnel|docs|dependencies|ci|tests|scripts|dev`, comma-separated):
- Module labels (`<module>: <component>`, for example `channel: telegram`, `provider: kimi`, `tool: shell`):
- Contributor tier label (`trusted contributor|experienced contributor|principal contributor|distinguished contributor`, auto-managed/read-only; author merged PRs >=5/10/20/50):
- If any auto-label is incorrect, note requested correction:

## Change Metadata

- Change type (`bug|feature|refactor|docs|security|chore`):
- Primary scope (`runtime|provider|channel|memory|security|ci|docs|multi`):

## Linked Issue

- Closes #
- Related #
- Depends on # (if stacked)
- Supersedes # (if replacing older PR)

## Supersede Attribution (required when `Supersedes #` is used)

- Superseded PRs + authors (`#<pr> by @<author>`, one per line):
- Integrated scope by source PR (what was materially carried forward):
- `Co-authored-by` trailers added for materially incorporated contributors? (`Yes/No`)
- If `No`, explain why (for example: inspiration-only, no direct code/design carry-over):
- Trailer format check (separate lines, no escaped `\n`): (`Pass/Fail`)

## Validation Evidence (required)

Commands and result summary:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- Evidence provided (test/log/trace/screenshot/perf):
- If any command is intentionally skipped, explain why:

## Security Impact (required)

- New permissions/capabilities? (`Yes/No`)
- New external network calls? (`Yes/No`)
- Secrets/tokens handling changed? (`Yes/No`)
- File system access scope changed? (`Yes/No`)
- If any `Yes`, describe risk and mitigation:

## Privacy and Data Hygiene (required)

- Data-hygiene status (`pass|needs-follow-up`):
- Redaction/anonymization notes:
- Neutral wording confirmation (use ZeroClaw/project-native labels if identity-like wording is needed):

## Compatibility / Migration

- Backward compatible? (`Yes/No`)
- Config/env changes? (`Yes/No`)
- Migration needed? (`Yes/No`)
- If yes, exact upgrade steps:

## Human Verification (required)

What was personally validated beyond CI:

- Verified scenarios:
- Edge cases checked:
- What was not verified:

## Side Effects / Blast Radius (required)

- Affected subsystems/workflows:
- Potential unintended effects:
- Guardrails/monitoring for early detection:

## Agent Collaboration Notes (recommended)

- Agent tools used (if any):
- Workflow/plan summary (if any):
- Verification focus:
- Confirmation: naming + architecture boundaries followed (`AGENTS.md` + `CONTRIBUTING.md`):

## Rollback Plan (required)

- Fast rollback command/path:
- Feature flags or config toggles (if any):
- Observable failure symptoms:

## Risks and Mitigations

List real risks in this PR (or write `None`).

- Risk:
  - Mitigation:
