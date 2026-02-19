import { createFileRoute } from '@tanstack/react-router'
import { getTerminalSession } from '../../server/terminal-sessions'

export const Route = createFileRoute('/api/terminal-resize')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const body = (await request.json().catch(() => ({}))) as Record<
          string,
          unknown
        >
        const sessionId =
          typeof body.sessionId === 'string' ? body.sessionId : ''
        const cols = typeof body.cols === 'number' ? body.cols : 80
        const rows = typeof body.rows === 'number' ? body.rows : 24
        const session = getTerminalSession(sessionId)
        if (!session) {
          return new Response(JSON.stringify({ ok: false }), {
            status: 404,
            headers: { 'Content-Type': 'application/json' },
          })
        }
        session.resize(cols, rows)
        return new Response(JSON.stringify({ ok: true }), {
          headers: { 'Content-Type': 'application/json' },
        })
      },
    },
  },
})
