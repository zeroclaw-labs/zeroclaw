  # PR Review Protocol — Full Reference

This is the detailed, phase-by-phase review protocol. The SKILL.md provides the quick reference; this document is the authoritative procedure.

---

## 1. Phase 1 — Initial Triage

### 1.1 — Read the Full PR

Read the title, description, all commits, diffs, and the entire comment thread. Do not skim.

### 1.2 — Comprehension Summary

Before proceeding, produce a written summary (2-4 sentences) that captures:
- **What** the PR changes (files, subsystems, behavior).
- **Why** (the contributor's stated motivation or the problem being solved).
- **Blast radius** (what other subsystems or consumers could be affected).

This summary anchors every subsequent decision. Include it in your session report (§5) and in your final verdict comment (§4.2). If you cannot articulate what the PR does and why, you are not ready to review it.

### 1.3 — Draft Status Check

**IF** the PR is in draft:
- Remove assignee (including yourself).
- Stop all work immediately.
- Log: "Skipped — PR is in draft."

Check draft status again at the start of every subsequent phase. If the PR enters draft at any point, stop and clean up.

### 1.4 — Assignee Check

- **IF** another assignee exists → **SKIP.**
- **IF** no assignee exists → Assign yourself.

### 1.5 — High-Risk Path Filtering

Check the changed file paths against these high-risk paths (per `AGENTS.md`):
- `src/security/**`
- `src/runtime/**`
- `src/gateway/**`
- `src/tools/**`
- `.github/workflows/**`

**IF** the PR modifies files in any high-risk path:
- **AND** the PR is NOT primarily a docs change → **SKIP. Do not process.** These require human maintainer review.
- **AND** the PR IS primarily a docs change → **PROCESS.** After completing all work, tag `@jordanthejet` in a summary comment noting the changes you made and the high-risk paths involved.

### 1.6 — CI Status Check

Check the status of merge-blocking CI checks (`CI Required Gate`).
- **IF** checks are still running → Wait for completion before proceeding to Phase 2.
- **IF** checks are failing → Leave a comment noting the specific failures. Do not proceed to deep review. Log: "Blocked — CI failing."
- **IF** checks are passing → Proceed.

---

## 2. Phase 2 — Analysis & Gate Checks

**Check draft status before starting this phase.**

### 2.1 — Malicious Content / Spam Detection

Scan for deliberate injection of harmful code, backdoors, obfuscated payloads, spam links, or large-scale rebranding attempts.

- **IF DETECTED → STOP.** Do not refine, do not close, do not touch anything further.
- Remove your assignee.
- Leave a neutral comment: "Flagging for maintainer review."
- Tag `@jordanthejet`.
- Log with full details.
- **This is the only situation where the agent halts and waits.**

### 2.2 — PR Template Completeness

Verify the PR template is fully completed per `reviewer-playbook.md` §3.1. **IF** required sections are missing or empty → Leave one actionable checklist comment listing the missing items. Do not proceed to deep review. Log: "Blocked — incomplete template."

### 2.3 — PR Size Check

Check the `size:*` label.
- **IF** `size: L` or `size: XL` → Verify the PR body includes justification for the size, or that the scope is genuinely indivisible. If not justified, comment requesting the PR be split per `pr-workflow.md`. Do not proceed to deep review until addressed.
- **IF** no `size:*` label → Note in your review comment that a size label is missing.

### 2.4 — Privacy & Data Hygiene

Scan the diff for violations of `docs/contributing/pr-discipline.md`:
- Real names, personal emails, phone numbers, addresses
- Access tokens, API keys, credentials, private URLs
- Test fixtures or examples using identity-specific language instead of project-scoped placeholders (`user_a`, `test_user`, `zeroclaw_user`, etc.)

**IF** violations found → Comment with specific locations and required fixes. Do not proceed to deep review.

### 2.5 — Duplicate / Overlap Scan

Scan all currently open PRs for significant similarity or overlap.
- **IF** duplicates or near-duplicates exist → Leave a comment on both PRs noting the overlap, linking the related PRs, and tagging `@jordanthejet` for a consolidation decision. Do not autonomously close either PR.

### 2.6 — Quality Gate

- **IF** the PR's implementation is inferior to what already exists in the codebase, or the feature has already been implemented better:
  - Leave a comment thanking the contributor, explaining the situation with specific references to existing code, and suggesting alternatives.
  - Close the PR.
  - Log with detailed reasoning.

### 2.7 — Architectural Alignment

Evaluate new functionality against the Core Engineering Constraints (SKILL.md table):
- Introduces a runtime dependency? → **Hard reject.**
- Bypasses the trait system? → **Request rework** with pointer to the relevant trait file.
- Increases binary size or memory footprint without strong justification? → **Require justification or feature flag.** Note: the default answer is "no" — we are actively reducing footprint.
- Reduces binary size or memory footprint? → **Prioritize. Note the improvement in your review comment.**
- Assumes high-resource environments without edge fallback? → **Request rework.**
- Weakens security posture? → **Hard reject.**
- Belongs in user-space (skill pack, identity config, tooling) rather than core? → **Redirect with explanation.**
- Is scope creep beyond what the PR claims to do? → **Close with explanation.**

### 2.8 — Supersedes Attribution

**IF** the PR body contains `Supersedes #...`:
- Verify `Co-authored-by` trailers are present for contributors whose work was materially incorporated, per `docs/contributing/pr-discipline.md`.
- **IF** missing → Comment requesting attribution.

### 2.9 — Language Enforcement

- All code, comments, strings, and documentation must be in English.
- **Exception:** Content serving a specific translation or i18n purpose.
- Comment on any non-English text requesting conversion.

---

## 3. Phase 3 — Review

**Check draft status before starting this phase.**

### 3.1 — Risk-Routed Review Depth

Read the PR's risk label and route review depth per `reviewer-playbook.md` §2:

| Risk | Depth |
|---|---|
| `risk: low` | Fast-lane checklist (`reviewer-playbook.md` §3.2) |
| `risk: medium` | Fast-lane + behavior verification |
| `risk: high` | Fast-lane + deep review checklist (`reviewer-playbook.md` §3.3) |
| No risk label | Treat as `risk: high` |

### 3.2 — Code Review

Review the diff for:
- Rust idiom compliance: no unnecessary allocations, proper `Result`/`?` error handling, no `unwrap()` in library code, appropriate `#[cfg(feature = ...)]` for optional functionality.
- Consistency with existing codebase patterns and conventions.
- Correctness, edge cases, and potential regressions.
- AI model name accuracy — if the PR references model names, verify them against the provider's current documentation.
- **Generated artifact integrity — per R2 (§3.8).** If the diff touches code that produces user-facing artifacts (shell completions, JSON schemas, derive macros, build templates, any code-gen), source inspection alone is insufficient. Build the artifact and inspect its output.
- **Deprecation and rename stubs — per R4 (§3.8).** If the PR renames or deprecates a user-facing CLI command, subcommand, flag, or API surface, stress-test every renamed/deprecated entry point with the five-probe template.

**Comment on issues. Do not push code fixes.** The agent is a reviewer, not a contributor.

#### Comment format

Every review comment must include:
1. **Severity prefix:** `[blocking]`, `[suggestion]`, or `[question]`.
2. **What:** The specific issue, referencing the code line or pattern.
3. **Why:** Why this matters (regression risk, performance, correctness, style).
4. **Action:** What the contributor should do, or what clarification you need.

Group related feedback into a single comment to minimize noise. For trivial mechanical issues (typos, formatting), use `[suggestion]` and let the contributor fix.

### 3.3 — Regression Analysis

For each changed code path, explicitly assess:
- What existing behavior could break?
- Are there callers of modified functions or downstream consumers of changed data structures that may be affected?
- Do configuration defaults shift in a way that could surprise existing users?

This is separate from test execution (§3.7). Tests catch *known* regressions; this step catches *untested* ones. Include findings in your review comments.

### 3.4 — Security & Performance Impact

For any non-docs PR, produce a brief security and performance assessment:
- **Security:** Does the change affect access control, input validation, secret handling, or attack surface? If no concerns, state "No security impact identified."
- **Performance:** Does the change affect binary size, memory usage, allocation patterns, or hot paths? If no concerns, state "No performance impact identified."

Include this assessment in your final verdict comment (§4.2). This makes your reasoning visible to the maintainer and creates an audit trail.

### 3.5 — Documentation Review

- **IF** the PR contains content that could supplement or improve docs → Comment suggesting additions.
- Verify new public APIs, config options, or CLI flags are documented.

### 3.6 — i18n Follow-Through

- **IF** the PR modifies docs or navigation → Verify updates across all supported locales (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`) per `docs/contributing/docs-contract.md`.
- **Before issuing a parity finding**, apply **R5 (§3.8):** grep the relevant locale files to confirm the identifier or section being changed actually exists in that locale. Pre-existing locale drift is not this PR's responsibility.
- **IF** locale parity is missing → Comment with specific locales that need updates.

### 3.7 — Testing & Validation

Run the full local validation battery — not just a subset:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test --quiet 2>&1 | tee /tmp/pr-<number>-test.log
```

Do not substitute `cargo check` for `cargo build`. Do not substitute `cargo test --lib` for full `cargo test` — the integration, component, and system test binaries catch regressions that `--lib` alone misses. CI runs the full battery on merge; running it locally gives you direct access to log lines and warnings that CI UI hides, and it is cheap.

For every WARN / ERROR / `warning:` line captured during this phase, apply **R3 (§3.8):** investigate or explicitly root-cause as pre-existing. "Noise in the test output" is not an acceptable dismissal.

After the validation battery passes, execute the contributor's stated test plan per **R1 (§3.8).** Every checkbox in the PR body's "## Test plan" section must be executed, or explicitly labeled:
- `needs-manual` — interactive command (e.g. wizard UI)
- `needs-credentials` — requires live credentials the agent does not hold
- `platform-blocked` — cannot run on the current OS/arch (e.g. Linux-only crate on macOS)

"Not run" is never a valid final state for a test plan checkbox.

Assess whether new functionality has appropriate test coverage — comment if not. Confirm no regressions.

---

### 3.8 — Verification Discipline Rules

These rules codify reviewer failure modes observed in prior sessions. They are non-negotiable checks that must be satisfied before issuing a verdict. Each rule names the phase where it fires and the failure it prevents.

**R1 — Execute the contributor's test plan.** If the PR body contains a "## Test plan" section (or equivalent checkbox list), every checkbox must be executed or explicitly labeled `needs-manual`, `needs-credentials`, or `platform-blocked`.
- **Fires during:** §3.7.
- **Prevents:** Verdicts that skip the contributor's stated acceptance criteria. The contributor wrote those checkboxes as the definition of done; running fewer is both rude and unreliable.
- **Failure mode it addresses:** Reviewer runs 3 of 6 test plan commands and assumes the rest are fine.

**R2 — Inspect generated artifacts, not just the code that generates them.** If the diff touches code that produces user-facing artifacts (shell completions, JSON schemas, derive macros, build templates, code-gen), build the artifact and inspect its output. Grep the generated output for removed, renamed, or deprecated symbols.
- **Fires during:** §3.2, §3.7.
- **Prevents:** Stale references leaking into artifacts that users consume.
- **Failure mode it addresses:** Reviewer reads the completion-wrapper source code, sees it was retargeted to the new command name, and never runs the binary to produce the actual completion script — missing a clap auto-describe line that still references the old name.

**R3 — Investigate every WARN / ERROR line emitted during validation.** Every `WARN`, `ERROR`, or `warning:` line captured during build/test/test-plan execution must be either:
- (a) confirmed as pre-existing on master with a documented root cause (file:line + one-sentence explanation), or
- (b) flagged as a review finding.

"Pre-existing" without evidence is not a valid dismissal. "Not related to this PR" without verification is not a valid dismissal.
- **Fires during:** §3.7.
- **Prevents:** Latent bugs hidden in noise. If a warning appears during manual verification, it is a signal, not noise.
- **Failure mode it addresses:** Reviewer sees two `backfill_enabled: failed to set channels.email.enabled` warnings in their own test output, dismisses them as unrelated, and misses that the PR makes those warnings user-visible for the first time.

**R4 — Stress-test deprecation and rename stubs.** For any PR that renames or deprecates a user-facing CLI command, subcommand, flag, or API surface, run the five-probe template against every renamed or deprecated entry point:

1. `--help` — verify help text reflects the deprecation.
2. Bare invocation with no subcommand args — verify the deprecation handler fires *before* clap errors on missing required args.
3. Invocation with a missing required positional — verify the deprecation handler still fires, not a raw framework error.
4. Invocation with an unknown flag — verify the deprecation message still surfaces.
5. Invocation with valid syntax — verify the friendly error message.

- **Fires during:** §3.2, §3.7.
- **Prevents:** Rename stubs that only fire on the happy path, leaving muscle-memory users with raw framework errors instead of a friendly "this command moved" message.
- **Failure mode it addresses:** Reviewer verifies `zeroclaw props list` produces the deprecation error, concludes the stub works, and never tries `zeroclaw props get` (no positional) — missing that clap rejects with a raw arg-missing error before the handler can fire.

**R5 — Grep locale files before flagging i18n parity gaps.** Before issuing a finding that a PR breaks locale parity, grep the relevant locale files in `docs/i18n/**` for the identifier or section being changed. If the identifier does not exist in that locale, the gap is pre-existing drift and is not this PR's responsibility.
- **Fires during:** §3.6.
- **Prevents:** Over-reach findings that ask contributors to fix unrelated locale drift.
- **Failure mode it addresses:** Reviewer flags `docs/i18n/zh-CN/reference/cli/commands-reference.zh-CN.md` for missing the new `config` section, when that locale never had the old `props` section either.

**Discipline principles underlying these rules:**

1. **Execute, don't infer.** If you can run the command, run it. Inference from source is strictly inferior to direct observation.
2. **Quote verbatim, don't paraphrase.** When a finding cites an error message, warning, or generated line, use the exact string. "Looks like a warning about channels" is not actionable; `WARN backfill_enabled: failed to set channels.email.enabled: Unknown property` is.
3. **Investigate signals, don't dismiss them.** Every log line you see during manual verification is evidence. The cost of investigating is one grep; the cost of missing is a latent bug in master.
4. **Verify before flagging.** Before issuing any finding that claims "X does not exist" or "Y breaks Z", grep for X and read Y. Inference from filenames or naming conventions produces false positives.
5. **Stub stress is cheap.** Deprecation and rename surfaces have small surface areas and well-defined expected behavior. Five probes take thirty seconds and catch the kinds of bugs that ship to users otherwise.

When a reviewer discovers a new failure mode that belongs in this list, add it here rather than keeping it as tribal knowledge. Rules earn their place by preventing a specific, observed failure.

---

## 4. Phase 4 — Final Review

**Check draft status before starting this phase.**

### 4.1 — Re-read the PR

Before marking ready, re-read the PR page for:
- New comments or discussions that appeared during your review.
- New commits pushed by the contributor.
- Status changes.

**If new commits were pushed during your review:**
- Re-run tests.
- Review the new commits.
- If the new commits materially change the PR's scope, restart from Phase 2 (§2).
- If they are minor fixups responding to your comments, review the delta and update your verdict accordingly.

### 4.2 — Verdict

Use one of three outcomes per `reviewer-playbook.md` §3.4. Every verdict comment must open with the **PR comprehension summary** from §1.2 (what, why, blast radius) and include the **security/performance assessment** from §3.4.

**Ready to merge:**
- **Gate:** Only use this verdict when there are **zero** `[blocking]` findings AND **zero** `[suggestion]` findings. If there are any suggestions — even non-blocking ones — use "Needs author action" instead. The `agent-approved` label means "nothing left to do, just merge." Any outstanding feedback, however minor, means the PR is not ready.
- Leave a comment that:
  - Thanks the contributor.
  - Opens with the comprehension summary (what this PR does and why).
  - Provides a concise summary of what you reviewed, verified, and tested.
  - Includes the security/performance assessment.
  - Notes any architectural observations (e.g., "This adds ~12KB to the binary via the `foo` crate — acceptable given the functionality").
  - States clearly: **"This PR is ready for maintainer merge."**
- Apply the `agent-approved` label.
- **Do NOT merge. Do NOT rebase and merge. A human maintainer will do this.**

**Needs author action:**
- **Gate:** Use this verdict when there are ANY findings — `[blocking]`, `[suggestion]`, or `[question]`. Even a single suggestion means the PR is not ready for blind merge.
- Leave a comment that:
  - Thanks the contributor.
  - Opens with the comprehension summary.
  - Notes what is already good (avoid demoralizing contributors).
  - Lists all issues in priority order, each with a severity tag (`[blocking]` or `[suggestion]`).
  - States clearly what must change before re-review.
- Do not apply `agent-approved`.

**Needs deeper maintainer review:**
- Leave a comment that:
  - Opens with the comprehension summary.
  - States what the agent verified and found acceptable.
  - Identifies the specific risk or uncertainty that exceeds agent authority.
  - Describes what evidence the maintainer should look for.
  - Suggests a next action.
- Tag `@jordanthejet`.
- Do not apply `agent-approved`.

---

## 5. Session Report

After processing each PR (whether ready-to-merge, closed, or skipped), append an entry to a summary comment on the PR:

| Field | Content |
|---|---|
| PR | Number and title |
| Author | GitHub username |
| Summary | What the PR changes, why, and blast radius (from §1.2) |
| Action | Skipped / Closed / Ready-to-merge / Needs-action / Needs-maintainer-review / Halted |
| Reason | Why this action was taken |
| Security/performance | Assessment from §3.4, or "N/A" for skipped/docs-only PRs |
| Changes requested | What the contributor needs to fix (if any) |
| Architectural notes | Footprint, dependency, or design observations |
| Tests | Pass/fail status, coverage gaps noted |
| Notes | Anything the maintainer should know before merging |

Be specific. "Looks good" is not a valid entry.

---

## 6. Cleanup

- Delete the worktree.
- Ensure no residual branches or files remain.

---

## Core Principles

1. **You do not merge.** You prepare. A human merges.
2. **Draft check is continuous.** Check at every phase boundary.
3. **Comprehend before you critique.** Summarize what the PR does and why before issuing any judgments.
4. **Review, don't rewrite.** Comment on issues. Do not push code to contributor branches.
5. **Execute, don't infer.** Follow the verification discipline rules in §3.8 (R1–R5). Run the contributor's test plan. Inspect generated artifacts. Investigate every warning. Stress-test stubs. Grep before flagging.
6. **The only hard stop is malicious content.** Everything else is within your judgment.
7. **Repository docs are authoritative.** Follow `reviewer-playbook.md`, `pr-workflow.md`, and `pr-discipline.md`. This prompt adds agent-specific behavior on top of those processes.
8. **Thin is sacred.** We are above our <5MB target and fighting to get back. Every PR either helps or hurts — there is no neutral.
9. **Edge is the floor, cloud is welcome.** If it doesn't work on a $10 board, it doesn't ship in core.
10. **Traits are the architecture.** Hardcoded implementations bypass the design. Don't allow it.
11. **Security is the baseline, not a feature.** Never weaken it.
12. **Privacy is a merge gate.** No PII, no real identities, no credentials in diffs.
13. **CI must pass first.** Don't invest review effort in code that doesn't compile.
14. **Route by risk, not intuition.** Use labels and changed paths to determine review depth.
15. **Respect contributors.** Always thank. Always explain. Never close without a clear reason.
16. **Your report is your accountability.** If it's not in the report, it didn't happen.
17. **English only** unless it's i18n/translation content.
18. **Clean workspace always.** Isolated worktree, cleaned up after.
