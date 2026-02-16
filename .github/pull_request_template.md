## Summary

Describe this PR in 2-5 bullets:

- Problem:
- Why it matters:
- What changed:
- What did **not** change (scope boundary):

## Collaboration Track (required)

- [ ] Track A (low risk: docs/tests/chore)
- [ ] Track B (medium risk: behavior changes in providers/channels/memory/tools)
- [ ] Track C (high risk: security/runtime/workflows/access control)

## Change Type

- [ ] Bug fix
- [ ] Feature
- [ ] Refactor
- [ ] Docs
- [ ] Security hardening
- [ ] Chore / infra

## Scope

- [ ] Core runtime / daemon
- [ ] Provider integration
- [ ] Channel integration
- [ ] Memory / storage
- [ ] Security / sandbox
- [ ] CI / release / tooling
- [ ] Documentation

## Linked Issue

- Closes #
- Related #

## Validation Evidence (required)

Commands and result summary:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Attach at least one:

- [ ] Failing test/log before + passing after
- [ ] Runtime trace/log evidence
- [ ] Screenshot/recording (if user-facing behavior changed)
- [ ] Performance numbers (if relevant)

If any command is intentionally skipped, explain why.

## Security Impact (required)

- New permissions/capabilities? (`Yes/No`)
- New external network calls? (`Yes/No`)
- Secrets/tokens handling changed? (`Yes/No`)
- File system access scope changed? (`Yes/No`)
- If any `Yes`, describe risk and mitigation:

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

## Agent Collaboration Notes (recommended)

- [ ] If agent/automation tools were used, I added brief workflow notes.
- [ ] I included concrete validation evidence for this change.
- [ ] I can explain design choices and rollback steps.

If agent tools were used, optional context:

- Tool(s):
- Prompt/plan summary:
- Verification focus:

## Rollback Plan (required)

- Fast rollback command/path:
- Feature flags or config toggles (if any):
- Observable failure symptoms:

## Risks and Mitigations

List real risks in this PR (or write `None`).

- Risk:
  - Mitigation:
