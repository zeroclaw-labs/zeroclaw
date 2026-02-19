import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '@/server/gateway'

export const Route = createFileRoute('/api/gateway/nodes')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const result = await gatewayRpc<Record<string, unknown>>(
            'node.list',
            {},
          )
          return json({ ok: true, data: result })
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
