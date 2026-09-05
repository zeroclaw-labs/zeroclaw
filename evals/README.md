# ZeroClaw eval suites

Suites of agent evaluation cases for `zeroclaw eval run` (crate: `crates/zeroclaw-eval`).

- `regression/` — must stay at 100% pass. Gated in CI (`crates/zeroclaw-eval/tests/regression_suite.rs`). A failure here blocks merge.
- `capability/` (planned) — hard tasks with a low pass rate; tracked over time, never gated.
- `live/` (planned) — cases executed against a real provider; cost money, never run in CI by default.

## Authoring rules

- Source cases from real failures (bug tracker, support reports). Start small; 20–50 good cases beat 500 vague ones.
- A tracker-attributed regression case must reach the boundary where that bug occurred. A replay fixture that only asserts text supplied by its own scripted provider is not evidence for provider serialization, streaming, policy, history, or UI behavior; keep the case generic or test the real boundary elsewhere.
- A case that names a value round trip (encoding, argument fidelity) must grade the value with `tool_arguments_contain` / `tool_results_contain`, not `response_contains` — the final response is scripted by the replay provider and stays green when the round trip breaks. A case that names N dispatches must use `exact_tool_calls: N`: `tools_used` is existential and `max_tool_calls` is an upper bound, so neither fails when a dispatch goes missing. Use `call_index` when the case claims an order.
- Every fixture's name and expectations must agree. If a case cannot assert the behavior its filename advertises, either grade the boundary or rename the case to what it actually proves.
- Every case states its class: a **positive** case (behavior must happen) or a **negative** case (behavior must NOT happen — e.g. `tools_not_used`, `response_not_contains`, `max_tool_calls: 0`). Keep the suite balanced; one-sided evals create one-sided optimization.
- The two-experts test: two people reading the case must independently reach the same pass/fail verdict from the case text alone. If they wouldn't, the case is ambiguous — tighten it.
- A replay case's scripted steps double as its reference solution: they prove the task is solvable.
- Privacy: fixtures ship forever. Placeholder identities only (`zeroclaw_user`, `example.com`) per `docs/book/src/contributing/privacy.md`. Never paste real transcripts, names, keys, or hostnames.

Suite owner: the maintainer group for `crates/zeroclaw-eval` (update when a named owner volunteers).
