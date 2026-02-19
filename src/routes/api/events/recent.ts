import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { getRecentEvents } from '../../../server/activity-events'
import {
  ensureActivityStreamStarted,
  getActivityStreamStatus,
} from '../../../server/activity-stream'

const DEFAULT_RECENT_COUNT = 50
const MAX_RECENT_COUNT = 100

export const Route = createFileRoute('/api/events/recent')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        void ensureActivityStreamStarted().catch(function ignoreStartError() {
          // recent endpoint still returns buffered data while disconnected
        })

        const url = new URL(request.url)
        const countParam = Number.parseInt(
          url.searchParams.get('count') ?? '',
          10,
        )

        const count = Number.isFinite(countParam)
          ? Math.max(1, Math.min(MAX_RECENT_COUNT, countParam))
          : DEFAULT_RECENT_COUNT

        return json({
          events: getRecentEvents(count),
          connected: getActivityStreamStatus() === 'connected',
        })
      },
    },
  },
})
