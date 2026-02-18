// Extracted from test-benchmarks.yml step: Post benchmark summary on PR

module.exports = async ({ github, context, core }) => {
    const fs = require('fs');
    const output = fs.readFileSync('benchmark_output.txt', 'utf8');

    // Extract Criterion result lines
    const lines = output.split('\n').filter(l =>
      l.includes('time:') || l.includes('change:') || l.includes('Performance')
    );

    if (lines.length === 0) {
      core.info('No benchmark results to post.');
      return;
    }

    const body = [
      '## 📊 Benchmark Results',
      '',
      '```',
      lines.join('\n'),
      '```',
      '',
      '<details><summary>Full output</summary>',
      '',
      '```',
      output.substring(0, 60000),
      '```',
      '</details>',
    ].join('\n');

    // Find and update or create comment
    const { data: comments } = await github.rest.issues.listComments({
      owner: context.repo.owner,
      repo: context.repo.repo,
      issue_number: context.payload.pull_request.number,
    });

    const marker = '## 📊 Benchmark Results';
    const existing = comments.find(c => c.body && c.body.startsWith(marker));

    if (existing) {
      await github.rest.issues.updateComment({
        owner: context.repo.owner,
        repo: context.repo.repo,
        comment_id: existing.id,
        body,
      });
    } else {
      await github.rest.issues.createComment({
        owner: context.repo.owner,
        repo: context.repo.repo,
        issue_number: context.payload.pull_request.number,
        body,
      });
    }
};
