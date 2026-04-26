---
name: factory-foreman
description: "Factory orchestration foreman for ZeroClaw. Use this skill when the user wants to run the software factory end-to-end, coordinate factory-clerk, factory-inspector, and factory-testbench, run a safe scheduled factory pass, produce one combined factory summary, or decide which factory roles run in preview/comment/apply modes. Trigger on: 'factory foreman', 'run the factory', 'full factory run', 'orchestrate factory', 'factory cron', or 'factory full run'."
---

# Factory Foreman

Factory Foreman owns orchestration. It runs factory roles in a fixed order with preview-first defaults and one combined summary.

## Authority

Read `references/policy.md` before running non-preview modes. Short version:

- `preview`: always safe; runs Testbench, Clerk, and Inspector without GitHub mutations.
- `comment-only`: may run Clerk/Inspector comment-only after Testbench passes.
- `apply-safe`: blocked unless `--allow-apply-safe` is passed, and still runs Testbench first.

## Runner

Preview all roles:

```bash
python3 .claude/skills/factory-foreman/scripts/factory_foreman.py \
  --repo zeroclaw-labs/zeroclaw \
  --mode preview
```

Comment-only:

```bash
python3 .claude/skills/factory-foreman/scripts/factory_foreman.py \
  --mode comment-only \
  --max-mutations 10
```

Apply safe Clerk closures only after explicit approval:

```bash
python3 .claude/skills/factory-foreman/scripts/factory_foreman.py \
  --mode apply-safe \
  --allow-apply-safe \
  --max-mutations 10
```

The runner writes role summaries under `artifacts/factory-foreman/`.

## Order

1. `factory-testbench fixture-test`
2. `factory-clerk`
3. `factory-inspector`

If Testbench fails, Foreman stops before any non-preview Clerk/Inspector run.
