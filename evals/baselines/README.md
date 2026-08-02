# Eval baselines

Git-versioned baseline files (`zeroclaw-eval/baseline/v1`) captured from prior runs. Compare a run against one with `zeroclaw eval run --baseline <file>`; refresh one with `--write-baseline <file>`. Gating is strictly per-case confirmed Pass to Fail flips against a comparable baseline (same `case_hash`, `mode`, `provider_ref`, `tool_surface`); a changed comparability key reports "changed, refresh baseline" and is never gated.
