import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayCronRpc, normalizeCronRuns } from '@/server/cron'

export const Route = createFileRoute('/api/cron/runs/$jobId')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        try {
          const url = new URL(request.url)
          const limitRaw = Number(url.searchParams.get('limit') ?? '10')
          const limit = Number.isFinite(limitRaw)
            ? Math.max(1, Math.min(100, Math.round(limitRaw)))
            : 10

          const segments = url.pathname.split('/')
          const maybeJobId = decodeURIComponent(
            segments[segments.length - 1] ?? '',
          )
          const jobId = maybeJobId.trim()
          if (!jobId) {
            return json({ error: 'jobId is required' }, { status: 400 })
          }

          const payload = await gatewayCronRpc(
            ['cron.runs', 'cron.jobs.runs', 'scheduler.runs'],
            {
              jobId,
              limit,
            },
          )

          return json({
            runs: normalizeCronRuns(payload).slice(0, limit),
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
