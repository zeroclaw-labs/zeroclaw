// Run safe intake checks for PR events and maintain a single sticky comment.
// Used by .github/workflows/pr-intake-checks.yml via actions/github-script.

module.exports = async ({ github, context, core }) => {
  const owner = context.repo.owner;
  const repo = context.repo.repo;
  const pr = context.payload.pull_request;
  if (!pr) return;
  const prAuthor = (pr.user?.login || "").toLowerCase();
  const prBaseRef = pr.base?.ref || "";

  const marker = "<!-- pr-intake-checks -->";
  const legacyMarker = "<!-- pr-intake-sanity -->";
  const requiredSections = [
    "## Summary",
    "## Validation Evidence (required)",
    "## Security Impact (required)",
    "## Privacy and Data Hygiene (required)",
    "## Rollback Plan (required)",
  ];
  const body = pr.body || "";

  const missingSections = requiredSections.filter((section) => !body.includes(section));
  const missingFields = [];
  const requiredFieldChecks = [
    ["summary problem", /- Problem:\s*\S+/m],
    ["summary why it matters", /- Why it matters:\s*\S+/m],
    ["summary what changed", /- What changed:\s*\S+/m],
    ["validation commands", /Commands and result summary:\s*[\s\S]*```/m],
    ["security risk/mitigation", /- New permissions\/capabilities\?\s*\(`Yes\/No`\):\s*\S+/m],
    ["privacy status", /- Data-hygiene status\s*\(`pass\|needs-follow-up`\):\s*\S+/m],
    ["rollback plan", /- Fast rollback command\/path:\s*\S+/m],
  ];
  for (const [name, pattern] of requiredFieldChecks) {
    if (!pattern.test(body)) {
      missingFields.push(name);
    }
  }

  const files = await github.paginate(github.rest.pulls.listFiles, {
    owner,
    repo,
    pull_number: pr.number,
    per_page: 100,
  });

  const formatWarnings = [];
  const dangerousProblems = [];
  for (const file of files) {
    const patch = file.patch || "";
    if (!patch) continue;
    const lines = patch.split("\n");
    for (let idx = 0; idx < lines.length; idx += 1) {
      const line = lines[idx];
      if (!line.startsWith("+") || line.startsWith("+++")) continue;
      const added = line.slice(1);
      const lineNo = idx + 1;
      if (/\t/.test(added)) {
        formatWarnings.push(`${file.filename}:patch#${lineNo} contains tab characters`);
      }
      if (/[ \t]+$/.test(added)) {
        formatWarnings.push(`${file.filename}:patch#${lineNo} contains trailing whitespace`);
      }
      if (/^(<<<<<<<|=======|>>>>>>>)/.test(added)) {
        dangerousProblems.push(`${file.filename}:patch#${lineNo} contains merge conflict markers`);
      }
    }
  }

  const workflowFilesChanged = files
    .map((file) => file.filename)
    .filter((name) => name.startsWith(".github/workflows/"));

  const advisoryFindings = [];
  const blockingFindings = [];
  if (missingSections.length > 0) {
    advisoryFindings.push(`Missing required PR template sections: ${missingSections.join(", ")}`);
  }
  if (missingFields.length > 0) {
    advisoryFindings.push(`Incomplete required PR template fields: ${missingFields.join(", ")}`);
  }
  if (formatWarnings.length > 0) {
    advisoryFindings.push(`Formatting issues in added lines (${formatWarnings.length})`);
  }
  if (dangerousProblems.length > 0) {
    blockingFindings.push(`Dangerous patch markers found (${dangerousProblems.length})`);
  }
  const promotionAuthorAllowlist = new Set(["willsarg", "theonlyhennygod"]);
  const shouldRetargetToDev =
    prBaseRef === "main" && !promotionAuthorAllowlist.has(prAuthor);

  if (shouldRetargetToDev) {
    advisoryFindings.push(
      "This PR targets `main`, but normal contributions must target `dev`. Retarget this PR to `dev` unless this is an authorized promotion PR.",
    );
  }

  const comments = await github.paginate(github.rest.issues.listComments, {
    owner,
    repo,
    issue_number: pr.number,
    per_page: 100,
  });
  const existing = comments.find((comment) => {
    const body = comment.body || "";
    return body.includes(marker) || body.includes(legacyMarker);
  });

  if (advisoryFindings.length === 0 && blockingFindings.length === 0) {
    if (existing) {
      await github.rest.issues.deleteComment({
        owner,
        repo,
        comment_id: existing.id,
      });
    }
    core.info("PR intake sanity checks passed.");
    return;
  }

  const runUrl = `${context.serverUrl}/${owner}/${repo}/actions/runs/${context.runId}`;
  const advisoryDetails = [];
  if (formatWarnings.length > 0) {
    advisoryDetails.push(...formatWarnings.slice(0, 20).map((entry) => `- ${entry}`));
    if (formatWarnings.length > 20) {
      advisoryDetails.push(`- ...and ${formatWarnings.length - 20} more issue(s)`);
    }
  }
  const blockingDetails = [];
  if (dangerousProblems.length > 0) {
    blockingDetails.push(...dangerousProblems.slice(0, 20).map((entry) => `- ${entry}`));
    if (dangerousProblems.length > 20) {
      blockingDetails.push(`- ...and ${dangerousProblems.length - 20} more issue(s)`);
    }
  }

  const isBlocking = blockingFindings.length > 0;

  const ownerApprovalNote = workflowFilesChanged.length > 0
    ? [
        "",
        "Workflow files changed in this PR:",
        ...workflowFilesChanged.map((name) => `- \`${name}\``),
        "",
        "Reminder: workflow changes require owner approval via `CI Required Gate`.",
      ].join("\n")
    : "";

  const commentBody = [
    marker,
    isBlocking
      ? "### PR intake checks failed (blocking)"
      : "### PR intake checks found warnings (non-blocking)",
    "",
    isBlocking
      ? "Fast safe checks found blocking safety issues:"
      : "Fast safe checks found advisory issues. CI lint/test/build gates still enforce merge quality.",
    ...(blockingFindings.length > 0 ? blockingFindings.map((entry) => `- ${entry}`) : []),
    ...(advisoryFindings.length > 0 ? advisoryFindings.map((entry) => `- ${entry}`) : []),
    "",
    "Action items:",
    "1. Complete required PR template sections/fields.",
    "2. Remove tabs, trailing whitespace, and merge conflict markers from added lines.",
    "3. Re-run local checks before pushing:",
    "   - `./scripts/ci/rust_quality_gate.sh`",
    "   - `./scripts/ci/rust_strict_delta_gate.sh`",
    "   - `./scripts/ci/docs_quality_gate.sh`",
    ...(shouldRetargetToDev
      ? ["4. Retarget this PR base branch from `main` to `dev`."]
      : []),
    "",
    `Run logs: ${runUrl}`,
    "",
    "Detected blocking line issues (sample):",
    ...(blockingDetails.length > 0 ? blockingDetails : ["- none"]),
    "",
    "Detected advisory line issues (sample):",
    ...(advisoryDetails.length > 0 ? advisoryDetails : ["- none"]),
    ownerApprovalNote,
  ].join("\n");

  if (existing) {
    await github.rest.issues.updateComment({
      owner,
      repo,
      comment_id: existing.id,
      body: commentBody,
    });
  } else {
    await github.rest.issues.createComment({
      owner,
      repo,
      issue_number: pr.number,
      body: commentBody,
    });
  }

  if (isBlocking) {
    core.setFailed("PR intake sanity checks found blocking issues. See sticky comment for details.");
    return;
  }

  core.info("PR intake sanity checks found advisory issues only.");
};
