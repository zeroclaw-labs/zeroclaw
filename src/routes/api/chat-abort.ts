import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'
import { isAuthenticated } from '../../server/auth-middleware'

type AbortRequestBody = {
  sessionKey?: string
}

export const Route = createFileRoute('/api/chat-abort')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        // Auth check
        if (!isAuthenticated(request)) {
          return json({ ok: false, error: 'Unauthorized' }, { status: 401 })
        }

        try {
          const body = (await request.json()) as AbortRequestBody
          const sessionKey = body.sessionKey?.trim() || undefined

          await gatewayRpc('chat.abort', { sessionKey })

          return json({ ok: true })
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err)
          return json({ ok: false, error: message }, { status: 500 })
        }
      },
    },
  },
})
