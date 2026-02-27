// Enforce at least one human approval on pull requests.
// Used by .github/workflows/ci-run.yml via actions/github-script.

module.exports = async ({ github, context, core }) => {
  const owner = context.repo.owner;
  const repo = context.repo.repo;
  const prNumber = context.payload.pull_request?.number;
  if (!prNumber) {
    core.setFailed("Missing pull_request context.");
    return;
  }

  const botAllowlist = new Set(
    (process.env.HUMAN_REVIEW_BOT_LOGINS || "github-actions[bot],dependabot[bot],coderabbitai[bot]")
      .split(",")
      .map((value) => value.trim().toLowerCase())
      .filter(Boolean),
  );

  const isBotAccount = (login, accountType) => {
    if (!login) return false;
    if ((accountType || "").toLowerCase() === "bot") return true;
    if (login.endsWith("[bot]")) return true;
    return botAllowlist.has(login);
  };

  const reviews = await github.paginate(github.rest.pulls.listReviews, {
    owner,
    repo,
    pull_number: prNumber,
    per_page: 100,
  });

  const latestReviewByUser = new Map();
  const decisiveStates = new Set(["APPROVED", "CHANGES_REQUESTED", "DISMISSED"]);
  for (const review of reviews) {
    const login = review.user?.login?.toLowerCase();
    if (!login) continue;
    if (!decisiveStates.has(review.state)) continue;
    latestReviewByUser.set(login, {
      state: review.state,
      type: review.user?.type || "",
    });
  }

  const humanApprovers = [];
  for (const [login, review] of latestReviewByUser.entries()) {
    if (review.state !== "APPROVED") continue;
    if (isBotAccount(login, review.type)) continue;
    humanApprovers.push(login);
  }

  if (humanApprovers.length === 0) {
    core.setFailed(
      "No human approving review found. At least one non-bot approval is required before merge.",
    );
    return;
  }

  core.info(`Human approval check passed. Approver(s): ${humanApprovers.join(", ")}`);
};
