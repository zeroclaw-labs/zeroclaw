import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'

export const Route = createFileRoute('/api/agent-activity')({
  server: {
    handlers: {
      GET: async () => {
        return json({ events: [], ok: true })
      },
    },
  },
})
