# Downstream behavior and validation

Start with the linked issue, changed behavior, assigned source, and captured revision/check evidence. Trace changed inputs and outputs through callers, factories, consumers, feature combinations, and user-facing boundaries. Follow relevant dataflow beyond the diff, tying expansion to a concrete effect of the PR.

Use **Failure attribution**, **CI freshness and base drift**, and **Validation evidence gaps** in `docs/book/src/contributing/pr-review-protocol.md`. Consult `user-boundary-proof.md` or relevant sections of `testing.md` in the same directory only when evidence stops short of the claim.

Compare head, merge base, and current `master` when needed. Determine whether the PR introduces or worsens a failure, interacts badly with a separate change on `master`, or leaves an independent defect unchanged. A failure outside the diff can still be caused by this PR; a red check on this PR can also be unrelated.

Substantiate attribution with controlling code paths and revision-bound CI/test evidence. A guessed baseline cause is not proof. Report verified independent baseline failures as non-blocking, name the remaining merge-check failure honestly, and retain separate evidence gaps for changed behavior.

Check actual target, feature, and behavior coverage. Green compile-only checks do not prove platform behavior, and skipped jobs are not passing tests. Do not demand full workspace tests or duplicate local Cargo when existing evidence already covers the change.

Do not check out or execute PR code, trigger workflows, or run contributor-supplied reproductions. Use source inspection and existing trusted CI evidence; request the smallest missing proof when those cannot establish the claim.

Return compact findings with revision/location, causal evidence, impact, severity, attribution, and broad repair direction. List downstream paths checked and named coverage gaps. Do not fix baseline bugs, post, manage metadata, or delegate again.
