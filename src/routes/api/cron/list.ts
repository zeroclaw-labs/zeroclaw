import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayCronRpc, normalizeCronJobs } from '@/server/cron'

export const Route = createFileRoute('/api/cron/list')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const payload = await gatewayCronRpc(
            ['cron.list', 'cron.jobs.list', 'scheduler.jobs.list'],
            { includeDisabled: true },
          )

          return json({
            jobs: normalizeCronJobs(payload),
          })
        } catch (err) {
          return json(
            {
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
