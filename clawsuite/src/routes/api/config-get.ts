import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'

export const Route = createFileRoute('/api/config-get')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const result = await gatewayRpc<{ defaultModel?: string }>(
            'config.get',
          )
          return json({ ok: true, payload: result })
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
