// First-time contributor greeting (replaces deprecated actions/first-interaction)

module.exports = async ({ github, context, core }) => {
  const owner = context.repo.owner;
  const repo = context.repo.repo;
  const issue = context.payload.issue;
  const pullRequest = context.payload.pull_request;
  const target = issue ?? pullRequest;

  if (!target) return;

  const author = target.user;
  if (!author || author.type === "Bot") return;

  const authorLogin = author.login;
  const issueNumber = target.number;
  const isIssue = Boolean(issue);

  // HTML marker to prevent duplicate greetings
  const marker = "<!-- auto-response:first-interaction -->";

  // Check if we already commented
  const comments = await github.paginate(github.rest.issues.listComments, {
    owner,
    repo,
    issue_number: issueNumber,
    per_page: 100,
  });

  const alreadyCommented = comments.some((comment) =>
    (comment.body || "").includes(marker)
  );

  if (alreadyCommented) {
    core.debug("First interaction greeting already posted");
    return;
  }

  // Check if this is genuinely the author's first interaction
  // Search for ANY issues OR PRs by this author in the repo
  const { data: userActivity } = await github.rest.search.issuesAndPullRequests({
    q: `repo:${owner}/${repo} author:${authorLogin}`,
    per_page: 1,
  });

  // If total_count is 1, it means only the current issue/PR exists
  // (the search includes the current issue/PR that triggered this workflow)
  const isFirstTime = userActivity.total_count === 1;

  if (!isFirstTime) {
    core.debug(`Author ${authorLogin} has previous activity (total: ${userActivity.total_count})`);
    return;
  }

  // Message templates (preserving original content from deprecated action)
  const messages = {
    issue: `Thanks for opening this issue.

Before maintainers triage it, please confirm:
- Repro steps are complete and run on latest \`main\`
- Environment details are included (OS, Rust version, ZeroClaw version)
- Sensitive values are redacted

This helps us keep issue throughput high and response latency low.

${marker}`,

    pullRequest: `Thanks for contributing to ZeroClaw.

For faster review, please ensure:
- PR template sections are fully completed
- \`cargo fmt --all -- --check\`, \`cargo clippy --all-targets -- -D warnings\`, and \`cargo test\` are included
- If automation/agents were used heavily, add brief workflow notes
- Scope is focused (prefer one concern per PR)

See \`CONTRIBUTING.md\` and \`docs/pr-workflow.md\` for full collaboration rules.

${marker}`,
  };

  const message = isIssue ? messages.issue : messages.pullRequest;

  await github.rest.issues.createComment({
    owner,
    repo,
    issue_number: issueNumber,
    body: message,
  });

  core.info(`Posted first interaction greeting for @${authorLogin}`);
};
