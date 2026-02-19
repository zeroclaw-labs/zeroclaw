// Extracted from ci-run.yml step: Require owner approval for workflow file changes

module.exports = async ({ github, context, core }) => {
    const owner = context.repo.owner;
    const repo = context.repo.repo;
    const prNumber = context.payload.pull_request?.number;
    const prAuthor = context.payload.pull_request?.user?.login?.toLowerCase() || "";
    if (!prNumber) {
      core.setFailed("Missing pull_request context.");
      return;
    }

    const baseOwners = ["theonlyhennygod", "willsarg"];
    const configuredOwners = (process.env.WORKFLOW_OWNER_LOGINS || "")
      .split(",")
      .map((login) => login.trim().toLowerCase())
      .filter(Boolean);
    const ownerAllowlist = [...new Set([...baseOwners, ...configuredOwners])];

    if (ownerAllowlist.length === 0) {
      core.setFailed("Workflow owner allowlist is empty.");
      return;
    }

    core.info(`Workflow owner allowlist: ${ownerAllowlist.join(", ")}`);

    const files = await github.paginate(github.rest.pulls.listFiles, {
      owner,
      repo,
      pull_number: prNumber,
      per_page: 100,
    });

    const workflowFiles = files
      .map((file) => file.filename)
      .filter((name) => name.startsWith(".github/workflows/"));

    if (workflowFiles.length === 0) {
      core.info("No workflow files changed in this PR.");
      return;
    }

    core.info(`Workflow files changed:\n- ${workflowFiles.join("\n- ")}`);

    if (prAuthor && ownerAllowlist.includes(prAuthor)) {
      core.info(`Workflow PR authored by allowlisted owner: @${prAuthor}`);
      return;
    }

    const reviews = await github.paginate(github.rest.pulls.listReviews, {
      owner,
      repo,
      pull_number: prNumber,
      per_page: 100,
    });

    const latestReviewByUser = new Map();
    for (const review of reviews) {
      const login = review.user?.login;
      if (!login) continue;
      latestReviewByUser.set(login.toLowerCase(), review.state);
    }

    const approvedUsers = [...latestReviewByUser.entries()]
      .filter(([, state]) => state === "APPROVED")
      .map(([login]) => login);

    if (approvedUsers.length === 0) {
      core.setFailed("Workflow files changed but no approving review is present.");
      return;
    }

    const ownerApprover = approvedUsers.find((login) => ownerAllowlist.includes(login));
    if (!ownerApprover) {
      core.setFailed(
        `Workflow files changed. Approvals found (${approvedUsers.join(", ")}), but none match workflow owner allowlist.`,
      );
      return;
    }

    core.info(`Workflow owner approval present: @${ownerApprover}`);

};
