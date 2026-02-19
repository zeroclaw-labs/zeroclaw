import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayCronRpc } from '@/server/cron'

export const Route = createFileRoute('/api/cron/run')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >

          const jobId = typeof body.jobId === 'string' ? body.jobId.trim() : ''
          if (!jobId) {
            return json({ error: 'jobId is required' }, { status: 400 })
          }

          const payload = await gatewayCronRpc(
            ['cron.run'],
            {
              jobId,
            },
          )

          return json({ ok: true, payload })
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
