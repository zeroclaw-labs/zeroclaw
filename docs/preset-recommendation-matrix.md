# Preset Recommendation Matrix

Official recommendation matrix for mapping user intent to built-in presets and optional packs.

Last updated: **February 21, 2026**.

## Quick Matrix

| User goal | Recommended base preset | Optional add packs | Optional remove packs | Risk notes |
|---|---|---|---|---|
| Smallest install, local core workflows | `minimal` | none | `browser-native`, `probe-rs`, `peripheral-rpi`, `rag-pdf`, `sandbox-landlock` | Usually no risk-gated packs unless manually added |
| General day-to-day use | `default` | `browser-native`, `rag-pdf` | none | `tools-update` is included and risk-gated |
| Browser automation and web workflow | `automation` | `rag-pdf` | `tools-update` (if update must be disabled) | `tools-update` requires explicit confirmation |
| Embedded debugging / hardware lab | `hardware-lab` | `peripheral-rpi` | none | `tools-update` requires explicit confirmation |
| Linux sandbox hardening | `hardened-linux` | `rag-pdf` | none | `sandbox-landlock` and `tools-update` are risk-gated |
| Raspberry Pi GPIO/peripheral control | `hardware-lab` | `peripheral-rpi` | none | `tools-update` requires explicit confirmation |
| Automation but no update | `automation` | none | `tools-update` | Removes risk-gated update path |
| Security-first with no browser | `hardened-linux` | none | `browser-native` | Keep explicit consent for any remaining risk-gated packs |

## Command Recipes

```bash
# Inspect official presets and packs
zeroclaw preset list

# Compose from official base + explicit pack edits
zeroclaw preset apply --preset automation --remove-pack tools-update --dry-run

# Apply and rebuild (both confirmations are explicit)
zeroclaw preset apply --preset hardware-lab --pack rag-pdf --yes-risky --rebuild --yes-rebuild

# Post-onboard natural-language orchestration
zeroclaw preset intent "need browser automation but no update" --dry-run
zeroclaw preset intent "need embedded debug with datasheet support" --apply --yes-risky
```

## Agent Orchestration Guardrails

- Agent may propose composition automatically after onboarding.
- Risk-gated packs require explicit user approval (`--yes-risky`) before persisting.
- Rebuild execution requires explicit user approval (`--yes-rebuild` or `preset rebuild --yes`).
- Large or security-sensitive actions default to dry-run/plan mode first.

## Import/Share Pairing Advice

- Team baseline rollout: use `import --mode overwrite`.
- Team baseline + user local additions: use `import --mode merge`.
- Conservative enrichment of local config: use `import --mode fill`.

See:

- [presets-guide.md](presets-guide.md)
- [commands-reference.md](commands-reference.md)
