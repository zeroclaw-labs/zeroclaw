# Maintainer dashboard

This report-only dashboard collects public ZeroClaw work-selection signals in one place. GitHub remains authoritative for issue, pull request, review, check, and merge state.

Use the optional maintainer field to add a personal **My work** view. The value stays in the page URL so the view can be bookmarked or shared; the dashboard does not store it or authenticate as that user.

<div id="maintainer-dashboard-app" class="maintainer-dashboard" data-repository="zeroclaw-labs/zeroclaw" data-zt5-url="zt5-public-status.json">
  <form class="maintainer-dashboard-controls" data-dashboard-controls>
    <label for="maintainer-dashboard-actor">Maintainer</label>
    <input id="maintainer-dashboard-actor" name="actor" type="text" inputmode="text" autocomplete="off" placeholder="GitHub login">
    <button type="submit">Apply</button>
    <button type="button" data-dashboard-refresh>Refresh</button>
  </form>
  <p class="maintainer-dashboard-note" data-dashboard-status>Loading public GitHub data…</p>
  <section aria-labelledby="dashboard-review-heading">
    <h2 id="dashboard-review-heading">Pull request review</h2>
    <p>These are routing candidates, not merge-readiness decisions. Use the <a href="./reviewer-playbook.html#pr-backlog-pruning">review queue CLI</a> for unanswered-request age and exact-head Core approval detail.</p>
    <div class="maintainer-dashboard-grid" data-dashboard-review></div>
  </section>
  <section aria-labelledby="dashboard-project-heading">
    <h2 id="dashboard-project-heading">Project planning</h2>
    <p>These views derive from public issue metadata. They do not read or write GitHub ProjectV2 fields.</p>
    <div class="maintainer-dashboard-grid" data-dashboard-project></div>
  </section>
  <section aria-labelledby="dashboard-zt5-heading">
    <h2 id="dashboard-zt5-heading">Zero-to-5 capabilities</h2>
    <p>This section contains only deliberately public facts. Detailed evidence, unpublished gaps, and security-sensitive analysis remain outside the public site.</p>
    <div class="maintainer-dashboard-grid" data-dashboard-zt5></div>
  </section>
  <noscript>This dashboard needs JavaScript to read public GitHub state. The linked GitHub searches remain available in the maintainer workflow documentation.</noscript>
</div>

## Interpretation limits

- A successful-check search narrows the review queue but does not establish mergeability, approval sufficiency, or current-head review validity.
- The author-action card shows the current label-backed backlog. The CLI inspects timelines before estimating whether a request has actually gone unanswered for the selected number of days.
- The second-Core card identifies possible high-risk or security candidates with an approval. The CLI checks the published Core roster and the approval's commit before calling out one-current-head-approval candidates.
- Public Zero-to-5 entries are curated snapshots, not a mirror of private dossiers. A missing score means the score has not been approved for public release.
