// Extracted from pr-check-status.yml step: Nudge PRs that need rebase or CI refresh

module.exports = async ({ github, context, core }) => {
  const staleHours = Number(process.env.STALE_HOURS || "48");
  const ignoreLabels = new Set(["no-stale", "stale", "maintainer", "no-pr-hygiene"]);
  const marker = "<!-- pr-hygiene-nudge -->";
  const owner = context.repo.owner;
  const repo = context.repo.repo;

  const openPrs = await github.paginate(github.rest.pulls.list, {
    owner,
    repo,
    state: "open",
    per_page: 100,
  });

  const activePrs = openPrs.filter((pr) => {
    if (pr.draft) {
      return false;
    }

    const labels = new Set((pr.labels || []).map((label) => label.name));
    return ![...ignoreLabels].some((label) => labels.has(label));
  });

  core.info(`Scanning ${activePrs.length} open PR(s) for hygiene nudges.`);

  let nudged = 0;
  let skipped = 0;

  for (const pr of activePrs) {
    const { data: headCommit } = await github.rest.repos.getCommit({
      owner,
      repo,
      ref: pr.head.sha,
    });

    const headCommitAt =
      headCommit.commit?.committer?.date || headCommit.commit?.author?.date;
    if (!headCommitAt) {
      skipped += 1;
      core.info(`#${pr.number}: missing head commit timestamp, skipping.`);
      continue;
    }

    const ageHours = (Date.now() - new Date(headCommitAt).getTime()) / 3600000;
    if (ageHours < staleHours) {
      skipped += 1;
      continue;
    }

    const { data: prDetail } = await github.rest.pulls.get({
      owner,
      repo,
      pull_number: pr.number,
    });

    const isBehindBase = prDetail.mergeable_state === "behind";

    const { data: checkRunsData } = await github.rest.checks.listForRef({
      owner,
      repo,
      ref: pr.head.sha,
      per_page: 100,
    });

    const ciGateRuns = (checkRunsData.check_runs || [])
      .filter((run) => run.name === "CI Required Gate")
      .sort((a, b) => {
        const aTime = new Date(a.started_at || a.completed_at || a.created_at).getTime();
        const bTime = new Date(b.started_at || b.completed_at || b.created_at).getTime();
        return bTime - aTime;
      });

    let ciState = "missing";
    if (ciGateRuns.length > 0) {
      const latest = ciGateRuns[0];
      if (latest.status !== "completed") {
        ciState = "in_progress";
      } else if (["success", "neutral", "skipped"].includes(latest.conclusion || "")) {
        ciState = "success";
      } else {
        ciState = String(latest.conclusion || "failure");
      }
    }

    const ciMissing = ciState === "missing";
    const ciFailing = !["success", "in_progress", "missing"].includes(ciState);

    if (!isBehindBase && !ciMissing && !ciFailing) {
      skipped += 1;
      continue;
    }

    const reasons = [];
    if (isBehindBase) {
      reasons.push("- Branch is behind `main` (please rebase or merge the latest base branch).");
    }
    if (ciMissing) {
      reasons.push("- No `CI Required Gate` run was found for the current head commit.");
    }
    if (ciFailing) {
      reasons.push(`- Latest \`CI Required Gate\` result is \`${ciState}\`.`);
    }

    const shortSha = pr.head.sha.slice(0, 12);
    const body = [
      marker,
      `Hi @${pr.user.login}, friendly automation nudge from PR hygiene.`,
      "",
      `This PR has had no new commits for **${Math.floor(ageHours)}h** and still needs an update before merge:`,
      "",
      ...reasons,
      "",
      "### Recommended next steps",
      "1. Rebase your branch on `main`.",
      "2. Push the updated branch and re-run checks (or use **Re-run failed jobs**).",
      "3. Post fresh validation output in this PR thread.",
      "",
      "Maintainers: apply `no-stale` to opt out for accepted-but-blocked work.",
      `Head SHA: \`${shortSha}\``,
    ].join("\n");

    const { data: comments } = await github.rest.issues.listComments({
      owner,
      repo,
      issue_number: pr.number,
      per_page: 100,
    });

    const existing = comments.find(
      (comment) => comment.user?.type === "Bot" && comment.body?.includes(marker),
    );

    if (existing) {
      if (existing.body === body) {
        skipped += 1;
        continue;
      }

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
        issue_number: pr.number,
        body,
      });
    }

    nudged += 1;
    core.info(`#${pr.number}: hygiene nudge posted/updated.`);
  }

  core.info(`Done. Nudged=${nudged}, skipped=${skipped}`);
};
