---
name: factory-inspector
description: "Factory intake inspector for ZeroClaw. Use this skill when the user wants to inspect PR or issue intake quality, check PR templates, verify validation evidence, check risk labels against touched paths, find missing linked issues, or run intake QA. Trigger on: 'factory inspector', 'inspect intake', 'PR intake', 'issue intake', 'check PR template', 'validation evidence audit', or 'risk label audit'."
---

# Factory Inspector

Factory Inspector owns intake quality. It checks whether new work is reviewable before maintainers spend deeper review time.

## Authority

Read `references/policy.md` before posting comments. Short version:

- `preview`: always safe; produces intake findings only.
- `comment-only`: may post one concise checklist comment per PR when deterministic intake checks fail.
- No close/merge/write-to-branch authority.
- Issue findings are preview-only for now.

## Runner

```bash
python3 .claude/skills/factory-inspector/scripts/factory_inspector.py \
  --repo zeroclaw-labs/zeroclaw \
  --mode preview
```

Modes:

```bash
python3 .claude/skills/factory-inspector/scripts/factory_inspector.py --mode preview
python3 .claude/skills/factory-inspector/scripts/factory_inspector.py --mode comment-only
```

Useful scoped runs:

```bash
python3 .claude/skills/factory-inspector/scripts/factory_inspector.py --checks pr-intake
python3 .claude/skills/factory-inspector/scripts/factory_inspector.py --checks issue-intake
```

The runner writes JSON audit output to `artifacts/factory-inspector/` unless `--no-audit-file` is passed.

## Checks

- PR template section presence.
- Linked issue section completeness.
- Validation evidence is not obviously placeholder-only.
- Risk label exists and high-risk touched paths are marked `risk: high` or `risk: manual`.
- Rollback section is filled for `risk: medium` / `risk: high`.
- Security/privacy section is not obviously placeholder-only.
- Issue intake preview for unlabeled reports and thin bug reports.

Use `factory-clerk` for lifecycle cleanup; use `factory-inspector` for intake quality.
