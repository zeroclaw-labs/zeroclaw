import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'

type SessionsPatchResponse = {
  ok?: boolean
  resolved?: {
    modelProvider?: string
    model?: string
  }
  [key: string]: unknown
}

export const Route = createFileRoute('/api/model-switch')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >
          const rawSessionKey =
            typeof body.sessionKey === 'string' ? body.sessionKey.trim() : ''
          const sessionKey = rawSessionKey || 'main'
          const model = typeof body.model === 'string' ? body.model.trim() : ''

          if (!model) {
            return json({ ok: false, error: 'model required' }, { status: 400 })
          }

          const payload = await gatewayRpc<SessionsPatchResponse>(
            'sessions.patch',
            {
              key: sessionKey,
              model,
            },
          )

          return json(payload)
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
