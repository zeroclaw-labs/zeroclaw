# ZeroClaw eval suites

Suites of agent evaluation cases for `zeroclaw eval run` (crate: `crates/zeroclaw-eval`).

- `regression/` — must stay at 100% pass. Gated in CI (`crates/zeroclaw-eval/tests/regression_suite.rs`). A failure here blocks merge.
- `capability/` (planned) — hard tasks with a low pass rate; tracked over time, never gated.
- `live/` (planned) — cases executed against a real provider; cost money, never run in CI by default.

## Authoring rules

- Source cases from real failures (bug tracker, support reports). Start small; 20–50 good cases beat 500 vague ones.
- Every case states its class: a **positive** case (behavior must happen) or a **negative** case (behavior must NOT happen — e.g. `tools_not_used`, `response_not_contains`, `max_tool_calls: 0`). Keep the suite balanced; one-sided evals create one-sided optimization.
- The two-experts test: two people reading the case must independently reach the same pass/fail verdict from the case text alone. If they wouldn't, the case is ambiguous — tighten it.
- A replay case's scripted steps double as its reference solution: they prove the task is solvable.
- `expects` supports response checks (`response_contains`/`response_not_contains`/`response_matches`/`response_json`), tool checks (`tools_used`/`tools_not_used`/`max_tool_calls`/`all_tools_succeeded`), workspace end-state checks (`workspace.file_exists`/`file_absent`/`file_contains`), and resource budgets (`budget.max_*`). See the eval-harness book page for the full field reference. Do not assert tool-call order or tool arguments; agents find valid alternate paths, so those checks are deliberately excluded.
- Privacy: fixtures ship forever. Placeholder identities only (`zeroclaw_user`, `example.com`) per `docs/book/src/contributing/privacy.md`. Never paste real transcripts, names, keys, or hostnames.

Suite owner: the maintainer group for `crates/zeroclaw-eval` (update when a named owner volunteers).
