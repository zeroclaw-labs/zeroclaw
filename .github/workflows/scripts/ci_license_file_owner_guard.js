// Enforce ownership rules for root license files in PRs.

module.exports = async ({ github, context, core }) => {
  const owner = context.repo.owner;
  const repo = context.repo.repo;
  const prNumber = context.payload.pull_request?.number;
  const prAuthor = context.payload.pull_request?.user?.login?.toLowerCase() || "";

  if (!prNumber) {
    core.setFailed("Missing pull_request context.");
    return;
  }

  const ownerAllowlist = ["willsarg"];

  if (ownerAllowlist.length === 0) {
    core.setFailed("License owner allowlist is empty.");
    return;
  }

  const protectedFiles = new Set(["LICENSE-APACHE", "LICENSE-MIT"]);
  const files = await github.paginate(github.rest.pulls.listFiles, {
    owner,
    repo,
    pull_number: prNumber,
    per_page: 100,
  });

  const changedProtectedFiles = files
    .map((file) => file.filename)
    .filter((name) => protectedFiles.has(name));

  if (changedProtectedFiles.length === 0) {
    core.info("No protected root license files changed in this PR.");
    return;
  }

  core.info(`Protected license files changed:\n- ${changedProtectedFiles.join("\n- ")}`);
  core.info(`Allowed license file editors: ${ownerAllowlist.join(", ")}`);

  if (!prAuthor) {
    core.setFailed("Unable to resolve PR author login.");
    return;
  }

  if (!ownerAllowlist.includes(prAuthor)) {
    core.setFailed(
      `Root license files (${changedProtectedFiles.join(", ")}) can only be changed by ${ownerAllowlist.join(", ")}. PR author is @${prAuthor}.`,
    );
    return;
  }

  core.info(`License file edit authorized for PR author: @${prAuthor}`);
};
