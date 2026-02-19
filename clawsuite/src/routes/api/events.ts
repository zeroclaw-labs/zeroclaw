import { createFileRoute } from '@tanstack/react-router'
import {
  ensureActivityStreamStarted,
  getActivityStreamStatus,
} from '../../server/activity-stream'
import { offEvent, onEvent } from '../../server/activity-events'
import type { ActivityEvent } from '../../types/activity-event'

const HEARTBEAT_INTERVAL_MS = 20_000

export const Route = createFileRoute('/api/events')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        void ensureActivityStreamStarted().catch(function ignoreStartError() {
          // stream stays available even when gateway is offline
        })

        const encoder = new TextEncoder()
        let cleanupStream = function noCleanupYet() {}

        const stream = new ReadableStream({
          start(controller) {
            let closed = false
            let heartbeatTimer: ReturnType<typeof setInterval> | null = null

            function sendEvent(eventName: string, payload: unknown) {
              if (closed) return
              const chunk = `event: ${eventName}\ndata: ${JSON.stringify(payload)}\n\n`
              controller.enqueue(encoder.encode(chunk))
            }

            const listener = function onActivityEvent(event: ActivityEvent) {
              sendEvent('activity', event)
            }

            function cleanup() {
              if (closed) return
              closed = true
              if (heartbeatTimer) {
                clearInterval(heartbeatTimer)
                heartbeatTimer = null
              }
              offEvent(listener)
              request.signal.removeEventListener('abort', onAbort)

              try {
                controller.close()
              } catch {
                // stream already closed
              }
            }

            function onAbort() {
              cleanup()
            }

            onEvent(listener)
            sendEvent('ready', {
              connected: getActivityStreamStatus() === 'connected',
            })

            heartbeatTimer = setInterval(function sendHeartbeat() {
              if (closed) return
              controller.enqueue(encoder.encode(': keep-alive\n\n'))
            }, HEARTBEAT_INTERVAL_MS)

            request.signal.addEventListener('abort', onAbort, { once: true })
            cleanupStream = cleanup
          },
          cancel() {
            cleanupStream()
          },
        })

        return new Response(stream, {
          headers: {
            'Content-Type': 'text/event-stream',
            'Cache-Control': 'no-cache',
            Connection: 'keep-alive',
            'X-Accel-Buffering': 'no',
          },
        })
      },
    },
  },
})
