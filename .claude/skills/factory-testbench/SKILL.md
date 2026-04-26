---
name: factory-testbench
description: "Factory replay and safety testbench for ZeroClaw. Use this skill when the user wants to snapshot GitHub issues/PRs, replay factory decisions offline, test factory-clerk or factory-inspector on captured backlog data, run safety invariants, build a factory simulator, or validate factory automations before mutation. Trigger on: 'factory testbench', 'factory replay', 'snapshot issues and PRs', 'simulate factory', 'test factory automation', or 'factory safety tests'."
---

# Factory Testbench

Factory Testbench owns replay safety. It snapshots GitHub backlog state, replays factory roles offline, and checks invariants before automation is trusted with mutations.

## Authority

Read `references/policy.md` before using live GitHub data. Short version:

- Snapshot and replay are read-only.
- Invariant failures block promotion to mutation modes.
- Sandbox mutation is future work; this first version is offline replay.

## Runner

Snapshot live GitHub state:

```bash
python3 .claude/skills/factory-testbench/scripts/factory_testbench.py \
  snapshot \
  --repo zeroclaw-labs/zeroclaw
```

Replay a snapshot:

```bash
python3 .claude/skills/factory-testbench/scripts/factory_testbench.py \
  replay \
  --snapshot artifacts/factory-testbench/latest.json
```

Run built-in safety fixtures:

```bash
python3 .claude/skills/factory-testbench/scripts/factory_testbench.py fixture-test
```

Run snapshot plus replay in one pass:

```bash
python3 .claude/skills/factory-testbench/scripts/factory_testbench.py roundtrip
```

The runner writes JSON output to `artifacts/factory-testbench/` unless `--no-audit-file` is passed.

## Checks

- Clerk never auto-closes protected targets.
- Clerk never auto-closes from open PRs, similarity, or implemented-on-master evidence.
- Clerk fixed-by-merged-PR closure requires a PR merged into `master`.
- Inspector never mutates issues.
- Inspector markers remain stable per PR intake check.
- Replayed decisions are serializable for audit/baseline comparison.
