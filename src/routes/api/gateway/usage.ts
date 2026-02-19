import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '@/server/gateway'

export const Route = createFileRoute('/api/gateway/usage')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const [usage, cost] = await Promise.allSettled([
            gatewayRpc<Record<string, unknown>>('sessions.usage', {
              limit: 1000,
              includeContextWeight: true,
            }),
            gatewayRpc<Record<string, unknown>>('usage.cost', {}),
          ])
          return json({
            ok: true,
            data: {
              usage: usage.status === 'fulfilled' ? usage.value : null,
              cost: cost.status === 'fulfilled' ? cost.value : null,
            },
          })
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
