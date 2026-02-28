// Extracted from ci-run.yml step: Require @chumyin approval for CI/CD related changes

module.exports = async ({ github, context, core }) => {
    const owner = context.repo.owner;
    const repo = context.repo.repo;
    const prNumber = context.payload.pull_request?.number;
    if (!prNumber) {
      core.setFailed("Missing pull_request context.");
      return;
    }

    const requiredApprover = "chumyin";

    const files = await github.paginate(github.rest.pulls.listFiles, {
      owner,
      repo,
      pull_number: prNumber,
      per_page: 100,
    });

    const ciCdFiles = files
      .map((file) => file.filename)
      .filter((name) =>
        name.startsWith(".github/workflows/") ||
        name.startsWith(".github/codeql/") ||
        name.startsWith(".github/connectivity/") ||
        name.startsWith(".github/release/") ||
        name.startsWith(".github/security/") ||
        name.startsWith("scripts/ci/") ||
        name === ".github/actionlint.yaml" ||
        name === ".github/dependabot.yml" ||
        name === "docs/ci-map.md" ||
        name === "docs/actions-source-policy.md" ||
        name === "docs/operations/self-hosted-runner-remediation.md",
      );

    if (ciCdFiles.length === 0) {
      core.info("No CI/CD related files changed in this PR.");
      return;
    }

    core.info(`CI/CD related files changed:\n- ${ciCdFiles.join("\n- ")}`);
    core.info(`Required approver: @${requiredApprover}`);

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
      core.setFailed("CI/CD related files changed but no approving review is present.");
      return;
    }

    if (!approvedUsers.includes(requiredApprover)) {
      core.setFailed(
        `CI/CD related files changed. Approvals found (${approvedUsers.join(", ")}), but @${requiredApprover} approval is required.`,
      );
      return;
    }

    core.info(`Required CI/CD approval present: @${requiredApprover}`);

};
