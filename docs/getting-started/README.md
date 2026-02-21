# Getting Started Docs

For first-time setup and quick orientation.

## Start Path

1. Main overview and quick start: [../../README.md](../../README.md)
2. One-click setup and dual bootstrap mode: [../one-click-bootstrap.md](../one-click-bootstrap.md)
3. Find commands by tasks: [../commands-reference.md](../commands-reference.md)

## Onboarding and Validation

- Quick onboarding: `zeroclaw onboard --api-key "sk-..." --provider openrouter`
- Quick onboarding with explicit non-strict policy: `zeroclaw onboard --security-profile flexible --yes-security-risk`
- Interactive onboarding: `zeroclaw onboard --interactive`
- Interactive security profiles: default is `Strict supervised`; lower-guardrail profiles require explicit risk acknowledgment before continuing.
- Onboarding now shows a concise preset-aware security suggestion and lets users expand advanced rationale on demand.
- Interactive onboarding now adds a preset-aware security recommendation after pack selection, so users can tighten or relax guardrails with context.
- Inspect current policy anytime: `zeroclaw security show`
- Keep non-CLI channels on manual approval (default): `zeroclaw security profile set strict --non-cli-approval manual`
- Allow non-CLI auto-approval only when you explicitly accept risk: `zeroclaw security profile set strict --non-cli-approval auto --yes-risk`
- Ask for intent-based recommendation (advisory): `zeroclaw security profile recommend "need unattended browser automation"`
- Preflight recommendation against a candidate composition: `zeroclaw security profile recommend "hardened deployment" --from-preset hardened-linux --remove-pack tools-update`
- Export orchestration-ready preset plan + security follow-up commands: `zeroclaw preset intent "need unattended browser automation" --json`
- Generate a reusable orchestration script template: `zeroclaw preset intent "need unattended browser automation" --emit-shell ./scripts/preset-plan.sh`
- If an agent action is blocked by policy, ZeroClaw now returns a graded remediation path (L0-L4) with explicit risk warnings before any permission relaxation guidance.
- Roll back to strict defaults quickly: `zeroclaw security profile set strict`
- Validate environment: `zeroclaw status` + `zeroclaw doctor`

## Next

- Runtime operations: [../operations/README.md](../operations/README.md)
- Reference catalogs: [../reference/README.md](../reference/README.md)
