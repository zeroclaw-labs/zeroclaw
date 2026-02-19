import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'

export const Route = createFileRoute('/api/agent-steer')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >
          const sessionKey =
            typeof body.sessionKey === 'string' ? body.sessionKey.trim() : ''
          const message =
            typeof body.message === 'string' ? body.message.trim() : ''

          if (!sessionKey) {
            return json(
              { ok: false, error: 'sessionKey required' },
              { status: 400 },
            )
          }

          if (!message) {
            return json(
              { ok: false, error: 'message required' },
              { status: 400 },
            )
          }

          const { randomUUID } = await import('node:crypto')
          await gatewayRpc('chat.send', {
            sessionKey,
            message: `[System Directive] ${message}`,
            deliver: false,
            timeoutMs: 30_000,
            idempotencyKey: randomUUID(),
          })

          return json({ ok: true })
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
