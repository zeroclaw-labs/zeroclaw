Closing in favor of the requested split. Per @singlerider's review, the two unrelated fixes are now on separate branches with separately-validated PRs:

- **#7403** — `fix(runtime): guard trim_history against orphan-cascade emptying all messages` (the change this PR's title described)
- **#7404** — `fix(channels): prevent Matrix /sync from timing out at exactly 30 seconds` (the change that matched the misleading branch name on this PR)

Both new PRs:
- Are branched off current `upstream/master` (post-#7231, so the prior `ollama.rs` E0308 master breaker is no longer inherited).
- Carry single-commit histories that match their titles exactly.
- Have rewritten bodies that honestly describe their own diff (the trim_history one no longer claims `risk: medium` against the auto-applied `risk: high` label; the Matrix one is properly `risk: medium`).
- Compile cleanly in isolation; the trim_history tests pass against current master.

Thanks for the careful review — the structural feedback was correct, and bisecting/rolling back either of these is much easier on separate PRs.
