// Extracted from pr-auto-response.yml step: Apply contributor tier label for issue author

module.exports = async ({ github, context, core }) => {
  const owner = context.repo.owner;
  const repo = context.repo.repo;
  const issue = context.payload.issue;
  const pullRequest = context.payload.pull_request;
  const target = issue ?? pullRequest;
  async function loadContributorTierPolicy() {
    const policyPath = process.env.LABEL_POLICY_PATH || ".github/label-policy.json";
    const fallback = {
      contributorTierColor: "2ED9FF",
      contributorTierRules: [
        { label: "distinguished contributor", minMergedPRs: 50 },
        { label: "principal contributor", minMergedPRs: 20 },
        { label: "experienced contributor", minMergedPRs: 10 },
        { label: "trusted contributor", minMergedPRs: 5 },
      ],
    };
    try {
      const { data } = await github.rest.repos.getContent({
        owner,
        repo,
        path: policyPath,
        ref: context.payload.repository?.default_branch || "main",
      });
      const json = JSON.parse(Buffer.from(data.content, "base64").toString("utf8"));
      const contributorTierRules = (json.contributor_tiers || []).map((entry) => ({
        label: String(entry.label || "").trim(),
        minMergedPRs: Number(entry.min_merged_prs || 0),
      }));
      const contributorTierColor = String(json.contributor_tier_color || "").toUpperCase();
      if (!contributorTierColor || contributorTierRules.length === 0) {
        return fallback;
      }
      return { contributorTierColor, contributorTierRules };
    } catch (error) {
      core.warning(`failed to load ${policyPath}, using fallback policy: ${error.message}`);
      return fallback;
    }
  }

  const { contributorTierColor, contributorTierRules } = await loadContributorTierPolicy();
  const contributorTierLabels = contributorTierRules.map((rule) => rule.label);
  const managedContributorLabels = new Set(contributorTierLabels);
  const action = context.payload.action;
  const changedLabel = context.payload.label?.name;

  if (!target) return;
  if ((action === "labeled" || action === "unlabeled") && !managedContributorLabels.has(changedLabel)) {
    return;
  }

  const author = target.user;
  if (!author || author.type === "Bot") return;

  function contributorTierDescription(rule) {
    return `Contributor with ${rule.minMergedPRs}+ merged PRs.`;
  }

  async function ensureContributorTierLabels() {
    for (const rule of contributorTierRules) {
      const label = rule.label;
      const expectedDescription = contributorTierDescription(rule);
      try {
        const { data: existing } = await github.rest.issues.getLabel({ owner, repo, name: label });
        const currentColor = (existing.color || "").toUpperCase();
        const currentDescription = (existing.description || "").trim();
        if (currentColor !== contributorTierColor || currentDescription !== expectedDescription) {
          await github.rest.issues.updateLabel({
            owner,
            repo,
            name: label,
            new_name: label,
            color: contributorTierColor,
            description: expectedDescription,
          });
        }
      } catch (error) {
        if (error.status !== 404) throw error;
        await github.rest.issues.createLabel({
          owner,
          repo,
          name: label,
          color: contributorTierColor,
          description: expectedDescription,
        });
      }
    }
  }

  function selectContributorTier(mergedCount) {
    const matchedTier = contributorTierRules.find((rule) => mergedCount >= rule.minMergedPRs);
    return matchedTier ? matchedTier.label : null;
  }

  let contributorTierLabel = null;
  try {
    const { data: mergedSearch } = await github.rest.search.issuesAndPullRequests({
      q: `repo:${owner}/${repo} is:pr is:merged author:${author.login}`,
      per_page: 1,
    });
    const mergedCount = mergedSearch.total_count || 0;
    contributorTierLabel = selectContributorTier(mergedCount);
  } catch (error) {
    core.warning(`failed to evaluate contributor tier status: ${error.message}`);
    return;
  }

  await ensureContributorTierLabels();

  const { data: currentLabels } = await github.rest.issues.listLabelsOnIssue({
    owner,
    repo,
    issue_number: target.number,
  });
  const keepLabels = currentLabels
    .map((label) => label.name)
    .filter((label) => !contributorTierLabels.includes(label));

  if (contributorTierLabel) {
    keepLabels.push(contributorTierLabel);
  }

  await github.rest.issues.setLabels({
    owner,
    repo,
    issue_number: target.number,
    labels: [...new Set(keepLabels)],
  });

};
