# Cron Example: Live Feed Refresh

Use this pattern to run live feed executions on schedule.

## Example jobs

- Every 5 minutes:
`*/5 * * * *` => execute active market/social/news feeds

- Hourly:
`0 * * * *` => execute media + integration feeds

- Daily cleanup:
`15 2 * * *` => prune stale feed runs and duplicate demo feeds

## Learning Notes

- Group jobs by source volatility (fast markets vs slower integrations).
- Keep execution batches small to isolate upstream failures.
- Prefer retry with backoff per feed, not global retry for entire batch.
