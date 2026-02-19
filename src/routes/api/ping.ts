import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayConnectCheck } from '../../server/gateway'

export const Route = createFileRoute('/api/ping')({
  server: {
    handlers: {
      GET: async () => {
        try {
          await gatewayConnectCheck()
          return json({ ok: true })
        } catch (err) {
          // Don't call gatewayReconnect() here — it destroys the existing
          // connection and creates a new one, which evicts the current
          // connection from the gateway (same device ID).
          // Just report the failure and let the client's own reconnect logic handle it.
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 503 },
          )
        }
      },
    },
  },
})
