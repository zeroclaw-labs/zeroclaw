// Post actionable lint failure summary as a PR comment.
// Used by the lint-feedback CI job via actions/github-script.
//
// Required environment variables:
//   RUST_CHANGED      — "true" if Rust files changed
//   DOCS_CHANGED      — "true" if docs files changed
//   LINT_RESULT       — result of the lint job
//   LINT_DELTA_RESULT — result of the strict delta lint job
//   DOCS_RESULT       — result of the docs-quality job

module.exports = async ({ github, context, core }) => {
  const owner = context.repo.owner;
  const repo = context.repo.repo;
  const issueNumber = context.payload.pull_request?.number;
  if (!issueNumber) return;

  const marker = "<!-- ci-lint-feedback -->";
  const rustChanged = process.env.RUST_CHANGED === "true";
  const docsChanged = process.env.DOCS_CHANGED === "true";
  const lintResult = process.env.LINT_RESULT || "skipped";
  const lintDeltaResult = process.env.LINT_DELTA_RESULT || "skipped";
  const docsResult = process.env.DOCS_RESULT || "skipped";

  const failures = [];
  if (rustChanged && !["success", "skipped"].includes(lintResult)) {
    failures.push("`Lint Gate (Format + Clippy)` failed.");
  }
  if (rustChanged && !["success", "skipped"].includes(lintDeltaResult)) {
    failures.push("`Lint Gate (Strict Delta)` failed.");
  }
  if (docsChanged && !["success", "skipped"].includes(docsResult)) {
    failures.push("`Docs Quality` failed.");
  }

  const comments = await github.paginate(github.rest.issues.listComments, {
    owner,
    repo,
    issue_number: issueNumber,
    per_page: 100,
  });
  const existing = comments.find((comment) => (comment.body || "").includes(marker));

  if (failures.length === 0) {
    if (existing) {
      await github.rest.issues.deleteComment({
        owner,
        repo,
        comment_id: existing.id,
      });
    }
    core.info("No lint/docs gate failures. No feedback comment required.");
    return;
  }

  const runUrl = `${context.serverUrl}/${owner}/${repo}/actions/runs/${context.runId}`;
  const body = [
    marker,
    "### CI lint feedback",
    "",
    "This PR failed one or more fast lint/documentation gates:",
    "",
    ...failures.map((item) => `- ${item}`),
    "",
    "Open the failing logs in this run:",
    `- ${runUrl}`,
    "",
    "Local fix commands:",
    "- `./scripts/ci/rust_quality_gate.sh`",
    "- `./scripts/ci/rust_strict_delta_gate.sh`",
    "- `./scripts/ci/docs_quality_gate.sh`",
    "",
    "After fixes, push a new commit and CI will re-run automatically.",
  ].join("\n");

  if (existing) {
    await github.rest.issues.updateComment({
      owner,
      repo,
      comment_id: existing.id,
      body,
    });
  } else {
    await github.rest.issues.createComment({
      owner,
      repo,
      issue_number: issueNumber,
      body,
    });
  }
};
