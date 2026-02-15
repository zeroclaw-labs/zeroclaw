## Summary

Describe this PR in 2-5 bullets:

- Problem:
- Why it matters:
- What changed:
- What did **not** change (scope boundary):

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

## Testing

Commands and result summary (required):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If any command is intentionally skipped, explain why.

## Security Impact

- New permissions/capabilities? (`Yes/No`)
- New external network calls? (`Yes/No`)
- Secrets/tokens handling changed? (`Yes/No`)
- File system access scope changed? (`Yes/No`)
- If any `Yes`, describe risk and mitigation:

## Agent Collaboration Notes (recommended)

- [ ] If agent/automation tools were used, I added brief workflow notes.
- [ ] I included concrete validation evidence for this change.
- [ ] I can explain design choices and rollback steps.

If agent tools were used, optional context:

- Tool(s):
- Prompt/plan summary:
- Verification focus:

## Rollback Plan

- Fast rollback command/path:
- Feature flags or config toggles (if any):
- Observable failure symptoms:
