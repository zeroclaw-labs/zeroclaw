// Extracted from pr-auto-response.yml step: Handle label-driven responses

module.exports = async ({ github, context, core }) => {
  const label = context.payload.label?.name;
  if (!label) return;

  const issue = context.payload.issue;
  const pullRequest = context.payload.pull_request;
  const target = issue ?? pullRequest;
  if (!target) return;

  const isIssue = Boolean(issue);
  const issueNumber = target.number;
  const owner = context.repo.owner;
  const repo = context.repo.repo;

  const rules = [
    {
      label: "r:support",
      close: true,
      closeIssuesOnly: true,
      closeReason: "not_planned",
      message:
        "This looks like a usage/support request. Please use README + docs first, then open a focused bug with repro details if behavior is incorrect.",
    },
    {
      label: "r:needs-repro",
      close: false,
      message:
        "Thanks for the report. Please add deterministic repro steps, exact environment, and redacted logs so maintainers can triage quickly.",
    },
    {
      label: "invalid",
      close: true,
      closeIssuesOnly: true,
      closeReason: "not_planned",
      message:
        "Closing as invalid based on current information. If this is still relevant, open a new issue with updated evidence and reproducible steps.",
    },
    {
      label: "duplicate",
      close: true,
      closeIssuesOnly: true,
      closeReason: "not_planned",
      message:
        "Closing as duplicate. Please continue discussion in the canonical linked issue/PR.",
    },
  ];

  const rule = rules.find((entry) => entry.label === label);
  if (!rule) return;

  const marker = `<!-- auto-response:${rule.label} -->`;
  const comments = await github.paginate(github.rest.issues.listComments, {
    owner,
    repo,
    issue_number: issueNumber,
    per_page: 100,
  });

  const alreadyCommented = comments.some((comment) =>
    (comment.body || "").includes(marker)
  );

  if (!alreadyCommented) {
    await github.rest.issues.createComment({
      owner,
      repo,
      issue_number: issueNumber,
      body: `${rule.message}\n\n${marker}`,
    });
  }

  if (!rule.close) return;
  if (rule.closeIssuesOnly && !isIssue) return;
  if (target.state === "closed") return;

  if (isIssue) {
    await github.rest.issues.update({
      owner,
      repo,
      issue_number: issueNumber,
      state: "closed",
      state_reason: rule.closeReason || "not_planned",
    });
  } else {
    await github.rest.issues.update({
      owner,
      repo,
      issue_number: issueNumber,
      state: "closed",
    });
  }
};
