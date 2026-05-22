# PR Workflow

The maintainer-side governance contract for PRs targeting `master`. Branch-protection settings, the DoR/DoD readiness contracts, and the failure-recovery protocol live here. Day-to-day reviewing lives in the [Reviewer Playbook](./reviewer-playbook.md). The contributor-facing flow lives in [How to contribute](../contributing/how-to.md).

## Governance goals

The workflow exists to keep five things true under high PR volume:

1. Merge throughput is predictable.
2. CI signal quality stays high — fast feedback, low false positives.
3. Security review is explicit on risky surfaces.
4. Changes are easy to reason about and easy to revert.
5. Repository artifacts stay free of personal or sensitive data.

The control loop that delivers this is layered on purpose:

- **Intake classification** — path/size/risk labels route the PR to the right depth.
- **Deterministic validation** — the merge gate depends on reproducible checks, not subjective comments.
- **Risk-based review depth** — high-risk paths get deep review, low-risk paths stay fast.
- **Rollback-first merge contract** — every merge path includes a concrete recovery story.

Automation handles intake labels and CI gating. Final merge accountability stays with human maintainers and PR authors.

## PR lanes

PR lanes are routing expectations, not another required label family. Use them to decide how much review depth, sequencing, and maintainer attention a PR needs. CODEOWNERS, native GitHub review state, CI, labels, linked issues, and explicit relationship keywords still carry the actual routing data.

| Lane | Common examples | Expected movement |
|---|---|---|
| A: maintenance fast lane | Docs-only corrections, small tests that leave behavior unchanged, metadata/template fixes, narrow examples, CI/tooling fixes that preserve permissions and release behavior | Lightest review; fast merge once CI, template, labels, and privacy checks are clean. Usually `risk: low` and `size: XS` or `size: S`. |
| B: narrow bug/fix lane | Small bug fixes with clear failing behavior, targeted provider/channel/tool fixes with focused validation, compatibility fixes that preserve behavior outside the reported path | Normal review by one subsystem-aware reviewer unless risk or ownership says otherwise. Merge when the linked issue is actually satisfied, validation is credible, and CI is green. |
| C: feature slice lane | Additive feature work, new provider/channel/tool support, new config surface, scoped user-visible behavior changes | Normal review plus boundary-specific validation. Milestone fit matters, and the PR should say whether it implements, depends on, or is related to a tracker. |
| D: architecture, migration, and high-risk lane | Runtime, gateway, security, tool-execution, workflow, broad crate migration, lifecycle, persistence, provider payload, channel behavior, permission, or release-infrastructure changes | Deep review, stronger local and CI evidence, rollback and compatibility analysis, and possible milestone sequencing or second-maintainer review. |
| E: supersede, replacement, and overlap lane | Multiple PRs solving the same issue, newer PRs replacing older ones, contributor work carried forward from another PR, old PR made obsolete by current `master` | Coordinate before deep review. Choose one canonical path when possible, use `Supersedes #N` only when accurate, and preserve attribution when work is materially carried forward. |

Do not build a separate manual PR board for these lanes unless native GitHub state and CODEOWNERS stop answering the routing question. Check native GitHub merge state before normal lane review: `DIRTY` means resolve conflicts first; `BEHIND` alone is mergeability housekeeping, not an author-facing blocker.

## Required repository settings

Branch protection on `master`:

- Require status checks before merge.
- Require check `CI Required Gate`.
- Require pull request reviews before merge.
- Require CODEOWNERS review for protected paths.
- For `.github/workflows/**`, require owner approval via `CI Required Gate` (`WORKFLOW_OWNER_LOGINS`); keep branch / ruleset bypass limited to org owners.
- Default workflow-owner allowlist is configured via the `WORKFLOW_OWNER_LOGINS` repository variable (see CODEOWNERS for the current list).
- Dismiss stale approvals when new commits are pushed.
- Restrict force-push.
- All contributor PRs target `master` directly.

## Definition of Ready (DoR)

Before requesting review, the PR has all of these:

- PR template fully completed.
- Scope boundary explicit (what changed / what did not).
- Validation evidence attached — actual command output, not "CI will check."
- Security & privacy and rollback fields completed for risky paths.
- Privacy and data-hygiene rules satisfied — neutral, project-scoped test wording. See [Privacy](../contributing/privacy.md).
- Identity-like wording, where unavoidable, uses ZeroClaw / project-native labels.

## Definition of Done (DoD)

Before merge:

- `CI Required Gate` is green.
- Required reviewers approved (including any CODEOWNERS paths).
- Risk labels match touched paths. See [Labels](./labels.md).
- Migration / compatibility impact is documented.
- Rollback path is concrete and fast.

## Maintainer merge checklist

Every merge:

- Scope is focused and understandable.
- CI gate is green.
- Docs-quality checks are green when docs changed.
- Security and privacy fields are complete; evidence is redacted / anonymized.
- Agent-workflow notes are sufficient for reproducibility (if AI-assisted).
- Rollback plan is explicit.
- Commit title follows Conventional Commits.

Squash-merge with full commit history preserved in the body. The `squash-merge` skill produces both the purple **Merged** badge and the conventional-commits formatted body — see [Skills](./skills.md) for invocation.

## AI / Agent contribution policy

AI-assisted PRs are welcome. Review can also be agent-assisted.

**Required:**

1. Clear PR summary with scope boundary.
2. Explicit test / validation evidence.
3. Security impact and rollback notes for risky changes.

**Recommended:**

1. Brief tool / workflow notes when automation materially influenced the change.
2. Optional prompt / plan snippets for reproducibility.

We do **not** require contributors to quantify AI-vs-human line ownership. The diff and the validation evidence carry the load.

For AI-heavy PRs, reviewers focus on:

- Contract compatibility.
- Security boundaries.
- Error handling.
- Performance and memory regressions.
- Whether the author can answer questions about behavior and blast radius (intent comprehension).

## Review SLA and queue discipline

- First maintainer triage target: **within 48 hours**.
- Blocked PRs get one actionable checklist comment, not a series of partial reviews.
- `status:no-stale` reserved for accepted-but-blocked work with a visible reason.

For stacked work, require explicit `Depends on #...` so review order is deterministic.

For replacements, require explicit `Supersedes #...`. See [Superseding PRs](./superseding.md) for attribution and template rules.

The reviewer-side queue management — backlog pruning order, stale handling, label hygiene — is in [Reviewer Playbook](./reviewer-playbook.md).

## Security and stability rules

These paths require stricter review and stronger test evidence:

- `crates/zeroclaw-runtime/src/security/`
- The rest of `crates/zeroclaw-runtime/`
- `crates/zeroclaw-gateway/` (ingress, authentication, pairing)
- `crates/zeroclaw-tools/` (anything with execution capability)
- Filesystem access boundaries.
- Network and authentication behavior.
- `.github/workflows/` and the release pipeline.

**Minimum for risky PRs:** threat / risk statement, mitigation notes, rollback steps.

**Recommended for high-risk PRs:** a focused test proving boundary behavior, plus one explicit failure-mode scenario with expected degradation.

For agent-assisted contributions on these paths, reviewers also verify the author can talk through runtime behavior and blast radius — not just paste validation output.

## Failure recovery

If a merged PR causes regressions:

1. Revert on `master` immediately.
2. Open a follow-up issue with root-cause analysis.
3. Re-introduce the fix only with regression tests covering the failure mode.

Prefer fast restoration of service quality over a delayed perfect fix.

## What this page does NOT cover

- **Day-to-day review mechanics** — see [Reviewer Playbook](./reviewer-playbook.md) and [PR Review Protocol](../contributing/pr-review-protocol.md).
- **Label thresholds and definitions** — see [Labels](./labels.md).
- **Privacy and PII rules** — see [Privacy](../contributing/privacy.md).
- **Supersede attribution and templates** — see [Superseding PRs](./superseding.md).
- **CI workflow inventory and triage** — see [CI & Actions](./ci-and-actions.md).
- **Release procedure** — see [Release Runbook](./release-runbook.md).
