//! `cargo generate review-docs` materializes the repeated PR-review policy
//! summaries from one typed contract. The full review protocol remains the
//! reviewer-facing authority; generated zones keep its consumers aligned.

use std::path::PathBuf;

const COMMAND: &str = "cargo generate review-docs";

#[derive(Clone, Copy)]
struct ReviewCiPolicy {
    base_branch: &'static str,
    required_gate: &'static str,
    minimum_gh_major: u16,
    minimum_gh_minor: u16,
    pending_exit_code: u8,
    current_failure_buckets: &'static [&'static str],
    evidence_gap_buckets: &'static [&'static str],
}

const POLICY: ReviewCiPolicy = ReviewCiPolicy {
    base_branch: "master",
    required_gate: "CI Required Gate",
    minimum_gh_major: 2,
    minimum_gh_minor: 50,
    pending_exit_code: 8,
    current_failure_buckets: &["fail", "cancel"],
    evidence_gap_buckets: &["skipping"],
};

impl ReviewCiPolicy {
    fn minimum_gh_version(self) -> String {
        format!("{}.{}.0", self.minimum_gh_major, self.minimum_gh_minor)
    }
}

type Render = fn(&ReviewCiPolicy, &str) -> anyhow::Result<String>;

struct Surface {
    name: &'static str,
    file: &'static str,
    render: Render,
}

fn registry() -> Vec<Surface> {
    vec![
        Surface {
            name: "review-skill",
            file: ".claude/skills/github-pr-review-session/SKILL.md",
            render: render_skill,
        },
        Surface {
            name: "review-protocol",
            file: "docs/book/src/contributing/pr-review-protocol.md",
            render: render_protocol,
        },
        Surface {
            name: "contributor-review-guidance",
            file: "docs/book/src/contributing/how-to.md",
            render: render_how_to,
        },
        Surface {
            name: "reviewer-playbook",
            file: "docs/book/src/maintainers/reviewer-playbook.md",
            render: render_reviewer_playbook,
        },
    ]
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn begin(zone: &str) -> String {
    format!(
        "{}<!-- >>> generated:{zone} by `{COMMAND}` - do not edit <<< -->",
        marker_indent(zone)
    )
}

fn end(zone: &str) -> String {
    format!(
        "{}<!-- >>> end generated:{zone} <<< -->",
        marker_indent(zone)
    )
}

fn marker_indent(zone: &str) -> &'static str {
    if zone == "review-ci-state-fetch" {
        "   "
    } else {
        ""
    }
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let begin = begin(zone);
    let end = end(zone);
    anyhow::ensure!(
        current.matches(&begin).count() == 1 && current.matches(&end).count() == 1,
        "{zone} must contain exactly one generated sentinel pair"
    );

    let begin_at = current
        .find(&begin)
        .ok_or_else(|| anyhow::Error::msg(format!("missing {zone} begin sentinel")))?;
    let after_begin = begin_at + begin.len();
    let end_at = current
        .find(&end)
        .ok_or_else(|| anyhow::Error::msg(format!("missing {zone} end sentinel")))?;
    anyhow::ensure!(after_begin < end_at, "{zone} sentinels are out of order");

    Ok(format!(
        "{}\n{}\n{}",
        &current[..after_begin],
        body.trim_end(),
        &current[end_at..]
    ))
}

fn buckets(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn render_skill(policy: &ReviewCiPolicy, current: &str) -> anyhow::Result<String> {
    let fetch = format!(
        "- **What to fetch** (PR metadata, comments, inline threads, formal reviews,\n  diff, RFCs, current merge state, base drift against `{}`, and required\n  checks) — run all fetches in a single parallel batch",
        policy.base_branch
    );
    let freshness = format!(
        "- **CI freshness and base-drift classification** — current GitHub\n  state and the protocol's precedence rules classify base drift and a\n  post-refresh pending `{}` rerun as warnings without\n  hiding current failures, evidence gaps, or conflicts",
        policy.required_gate
    );

    let rendered = splice(current, "review-fetch-state", &fetch)?;
    splice(&rendered, "review-ci-freshness-summary", &freshness)
}

fn render_protocol_fetch(policy: &ReviewCiPolicy) -> String {
    let minimum_gh_version = policy.minimum_gh_version();
    format!(
        r#"   <div class="os-tabs-src">

   #### sh

   ```sh
   GH_MIN_VERSION={minimum_gh_version}
   GH_VERSION=$(gh --version | awk 'NR == 1 {{ print $3 }}')
   GH_MAJOR=${{GH_VERSION%%.*}}
   GH_REST=${{GH_VERSION#*.}}
   GH_MINOR=${{GH_REST%%.*}}
   if ! printf '%s\n' "$GH_MAJOR" "$GH_MINOR" | awk 'NF != 1 || $0 !~ /^[0-9]+$/ {{ exit 1 }}'; then
     echo "could not parse gh version: $GH_VERSION" >&2
     exit 1
   fi
   if [ "$GH_MAJOR" -lt {minimum_gh_major} ] || {{ [ "$GH_MAJOR" -eq {minimum_gh_major} ] && [ "$GH_MINOR" -lt {minimum_gh_minor} ]; }}; then
     echo "gh $GH_MIN_VERSION or newer is required for machine-readable required checks" >&2
     exit 1
   fi

   PR_STATE=$(gh pr view <number> --repo zeroclaw-labs/zeroclaw \
     --json headRefOid,mergeable,mergeStateStatus)
   printf '%s\n' "$PR_STATE"
   HEAD_SHA=$(printf '%s' "$PR_STATE" | jq -r .headRefOid)
   gh api "repos/zeroclaw-labs/zeroclaw/compare/{base_branch}...${{HEAD_SHA}}" \
     --jq '{{status,behind_by,ahead_by}}'
   gh pr checks <number> --repo zeroclaw-labs/zeroclaw \
     --required --json name,state,bucket
   ```

   </div>

   This classification requires `gh >= {minimum_gh_version}`. Stop and upgrade
   an older client rather than silently dropping required-check data. Record
   `headRefOid` as the revision being reviewed. Treat the check output and
   `behind_by` comparison as current only for that head. `gh pr checks` exits
   non-zero by design when required checks are pending (exit {pending_exit_code}), failing, or
   absent. Treat that exit code as state to classify, not as a failed fetch,
   and inspect any JSON output it returned. Use this state for the CI freshness
   and base drift rules below, never an author's description of the state.

   On a re-review, a verified refreshed head means this `headRefOid` differs
   from the previously reviewed head recorded in `tmp/handoff.md` or the
   `commit_id` of the reviewer's own prior review from step 4, and the reviewer
   has confirmed that the new revision contains the requested refresh. On a
   first review or without a prior reviewed head, do not infer a rerun from
   author prose; use the normal pending-CI rules."#,
        base_branch = policy.base_branch,
        minimum_gh_version = minimum_gh_version,
        minimum_gh_major = policy.minimum_gh_major,
        minimum_gh_minor = policy.minimum_gh_minor,
        pending_exit_code = policy.pending_exit_code,
    )
}

fn render_protocol_policy(policy: &ReviewCiPolicy) -> String {
    format!(
        r#"## CI freshness and base drift

Classify CI freshness from the current GitHub state fetched above, not from an
author's prose or a stale review artifact. Base drift alone is mergeability
housekeeping, consistent with the [PR lanes](../maintainers/pr-workflow.md#pr-lanes),
but the full state determines the review classification.

Apply these rules in order:

1. If `mergeable` or `mergeStateStatus` is `UNKNOWN`, refetch this state once.
   If it remains unknown, stop this classification and do not approve on the
   freshness-warning path; GitHub has not established whether the PR conflicts.
2. `mergeable == "CONFLICTING"` or `mergeStateStatus == "DIRTY"` is a merge
   conflict, not a freshness warning. A request to refresh onto `{base_branch}` does
   not downgrade the conflict.
3. A required check whose `bucket` is {current_failure_buckets} on the current
   `headRefOid` is a current failure first. Investigate its cause and classify
   the concrete failure on its merits; it may be blocking. Do not treat a
   failed result from an older head as current.
4. A required check whose `bucket` is {evidence_gap_buckets}, or a required gate that is
   absent from the output or otherwise unavailable, is an evidence gap, not the
   pending-rerun carve-out. Classify the exact missing evidence on its merits
   and withhold approval when the affected behavior is not substantiated by
   other credible evidence.
5. After excluding unknown state, conflicts, current failures, and evidence
   gaps, classify a request to refresh a branch that is behind current `{base_branch}`
   (`mergeStateStatus == "BEHIND"` or the comparison reports `behind_by > 0`),
   or to wait for the repo's required aggregate gate (currently
   `{required_gate}`) when its `bucket` is `pending` on the verified refreshed
   `headRefOid`, as `### 🟡 Warning — ...`. Do not use `--request-changes` or
   withhold approval solely for either freshness state when the implementation
   review and other evidence are otherwise sufficient.
6. A pending gate that is not a rerun on a verified refreshed head does not use
   the freshness carve-out. Apply the normal validation-evidence and verdict
   rules to that state.

Pending CI is not evidence and must not be described as proof. This rule only
says that a verified refresh-and-rerun state is not itself a code-review
blocker. It does not make the PR merge-ready. The `squash-merge` skill's
required-check and freshness-basis steps still apply before merge. Because
`{base_branch}` dismisses stale approvals when new commits are pushed, an approval on
this path is dismissed when the author performs the requested refresh;
re-approve the refreshed head after reviewing it and once the required gate
reports."#,
        base_branch = policy.base_branch,
        required_gate = policy.required_gate,
        current_failure_buckets = buckets(policy.current_failure_buckets),
        evidence_gap_buckets = buckets(policy.evidence_gap_buckets),
    )
}

fn render_validation_gap(_policy: &ReviewCiPolicy) -> String {
    "When validation is the concern, identify the exact evidence gap instead of asking for \"full Cargo\" by reflex. Check the current required CI jobs and the changed surface, then ask for extra validation only where required CI does not prove the thing under review: tests for a platform that only received compile checks, Clippy for a platform or path outside the required lint job, desktop coverage when the desktop workflow did not trigger, release targets outside the PR matrix, stale CI beyond the base-drift-only case classified above, or unavailable CI.".to_owned()
}

fn render_protocol(policy: &ReviewCiPolicy, current: &str) -> anyhow::Result<String> {
    let rendered = splice(
        current,
        "review-ci-state-fetch",
        &render_protocol_fetch(policy),
    )?;
    let rendered = splice(
        &rendered,
        "review-ci-freshness-policy",
        &render_protocol_policy(policy),
    )?;
    splice(
        &rendered,
        "review-validation-evidence-gaps",
        &render_validation_gap(policy),
    )
}

fn render_how_to(_policy: &ReviewCiPolicy, current: &str) -> anyhow::Result<String> {
    let body = "Add more evidence when the PR depends on a known CI coverage gap: platform-specific tests, cross-platform lint, desktop app coverage, release target builds, stale CI beyond the [base-drift-only review case](./pr-review-protocol.md#ci-freshness-and-base-drift), or unavailable CI. \"It works on my machine\" is not evidence.";
    splice(current, "review-ci-evidence-how-to", body)
}

fn render_reviewer_playbook(_policy: &ReviewCiPolicy, current: &str) -> anyhow::Result<String> {
    let body = "- Duplicate local Cargo is not required when fresh required CI covers the same head, target, and feature set. Ask for extra validation only when it maps to a named gap in the required gate, such as macOS/Windows tests, cross-platform Clippy, desktop coverage, release target builds, stale CI beyond the [base-drift-only review case](../contributing/pr-review-protocol.md#ci-freshness-and-base-drift), or unavailable CI.";
    splice(current, "review-ci-evidence-playbook", body)
}

pub fn run(check: bool) -> anyhow::Result<()> {
    let root = workspace_root();
    let mut drift = false;

    for surface in registry() {
        let path = root.join(surface.file);
        let current = std::fs::read_to_string(&path)
            .map_err(|error| anyhow::Error::msg(format!("{}: {error}", path.display())))?;
        let rendered = (surface.render)(&POLICY, &current)?;
        if check {
            if current == rendered {
                println!("ok: {} in sync", surface.name);
            } else {
                eprintln!(
                    "DRIFT: {} is out of sync with the review policy",
                    surface.name
                );
                drift = true;
            }
        } else if current == rendered {
            println!("unchanged {}", path.display());
        } else {
            std::fs::write(&path, rendered)?;
            println!("wrote {}", path.display());
        }
    }

    if check && drift {
        anyhow::bail!("one or more review docs drifted; run `{COMMAND}`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_surfaces_are_fresh_and_idempotent() {
        let root = workspace_root();
        for surface in registry() {
            let current = std::fs::read_to_string(root.join(surface.file)).unwrap();
            let once = (surface.render)(&POLICY, &current).unwrap();
            let twice = (surface.render)(&POLICY, &once).unwrap();
            assert_eq!(current, once, "{} must be freshly generated", surface.name);
            assert_eq!(
                once, twice,
                "{} generation must be idempotent",
                surface.name
            );
        }
    }

    #[test]
    fn canonical_facts_reach_the_protocol() {
        let rendered = render_protocol_policy(&POLICY);
        assert!(rendered.contains(POLICY.base_branch));
        assert!(rendered.contains(POLICY.required_gate));
        assert!(rendered.contains(&buckets(POLICY.current_failure_buckets)));
        assert!(rendered.contains(&buckets(POLICY.evidence_gap_buckets)));

        let fetch = render_protocol_fetch(&POLICY);
        assert!(fetch.contains(&POLICY.minimum_gh_version()));
        assert!(fetch.contains(&POLICY.pending_exit_code.to_string()));
        assert!(fetch.contains(&format!(
            "[ \"$GH_MAJOR\" -lt {} ]",
            POLICY.minimum_gh_major
        )));
        assert!(fetch.contains(&format!(
            "[ \"$GH_MINOR\" -lt {} ]",
            POLICY.minimum_gh_minor
        )));
    }

    #[test]
    fn duplicate_or_missing_sentinels_fail_closed() {
        let zone = "sample";
        let duplicate = format!(
            "{}\nold\n{}\n{}\nsecond\n{}",
            begin(zone),
            end(zone),
            begin(zone),
            end(zone)
        );
        assert!(splice(&duplicate, zone, "new").is_err());
        assert!(splice("no markers", zone, "new").is_err());
    }
}
