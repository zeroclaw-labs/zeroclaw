import { createFileRoute } from '@tanstack/react-router'
import { onGatewayEvent, gatewayConnectCheck } from '../../server/gateway'
import type { GatewayFrame } from '../../server/gateway'
import { isAuthenticated } from '../../server/auth-middleware'

/**
 * Extract text content from a gateway message.
 */
function extractTextFromMessage(message: any): string {
  if (!message?.content) return ''
  if (Array.isArray(message.content)) {
    return message.content
      .filter((block: any) => block?.type === 'text' && block?.text)
      .map((block: any) => block.text)
      .join('')
  }
  if (typeof message.content === 'string') return message.content
  return ''
}

/**
 * SSE endpoint that streams chat events from the MAIN gateway connection.
 * Uses onGatewayEvent to listen on the shared GatewayClient —
 * no second WebSocket, no device ID conflict.
 */
export const Route = createFileRoute('/api/chat-events')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        if (!isAuthenticated(request)) {
          return new Response(
            JSON.stringify({ ok: false, error: 'Unauthorized' }),
            { status: 401, headers: { 'Content-Type': 'application/json' } },
          )
        }

        const url = new URL(request.url)
        const sessionKeyParam = url.searchParams.get('sessionKey')?.trim()

        const encoder = new TextEncoder()
        let streamClosed = false
        let cleanupListener: (() => void) | null = null
        let heartbeatTimer: ReturnType<typeof setInterval> | null = null

        const stream = new ReadableStream({
          async start(controller) {
            const sendEvent = (event: string, data: unknown) => {
              if (streamClosed) return
              try {
                const payload = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`
                controller.enqueue(encoder.encode(payload))
              } catch { /* stream closed */ }
            }

            const closeStream = () => {
              if (streamClosed) return
              streamClosed = true
              if (heartbeatTimer) { clearInterval(heartbeatTimer); heartbeatTimer = null }
              if (cleanupListener) { cleanupListener(); cleanupListener = null }
              try { controller.close() } catch { /* ignore */ }
            }

            try {
              // Ensure gateway is connected
              await gatewayConnectCheck()

              sendEvent('connected', {
                timestamp: Date.now(),
                sessionKey: sessionKeyParam || 'all',
              })

              // Listen on the MAIN gateway client's event stream
              cleanupListener = onGatewayEvent((frame: GatewayFrame) => {
                if (streamClosed) return
                if (frame.type !== 'event' && frame.type !== 'evt') return

                const eventName = (frame as any).event
                const rawPayload = (frame as any).payload ?? ((frame as any).payloadJSON ? (() => { try { return JSON.parse((frame as any).payloadJSON) } catch { return null } })() : null)
                if (!rawPayload) return

                const eventSessionKey = rawPayload?.sessionKey || rawPayload?.context?.sessionKey
                if (sessionKeyParam && eventSessionKey && eventSessionKey !== sessionKeyParam) return

                const targetSessionKey = eventSessionKey || sessionKeyParam || 'main'

                // Agent events (streaming chunks, thinking, tool calls)
                if (eventName === 'agent') {
                  const stream = rawPayload?.stream
                  const data = rawPayload?.data
                  const runId = rawPayload?.runId

                  if (stream === 'assistant' && data?.text) {
                    sendEvent('chunk', { text: data.text, runId, sessionKey: targetSessionKey })
                  } else if (stream === 'thinking' && data?.text) {
                    sendEvent('thinking', { text: data.text, runId, sessionKey: targetSessionKey })
                  } else if (stream === 'tool') {
                    sendEvent('tool', {
                      phase: data?.phase, name: data?.name, toolCallId: data?.toolCallId,
                      args: data?.args, runId, sessionKey: targetSessionKey,
                    })
                  }
                  return
                }

                // Chat events (messages, state changes)
                if (eventName === 'chat') {
                  const state = rawPayload?.state
                  const message = rawPayload?.message
                  const runId = rawPayload?.runId

                  if (state === 'delta' && message) {
                    const text = extractTextFromMessage(message)
                    if (text) sendEvent('chunk', { text, runId, sessionKey: targetSessionKey, fullReplace: true })
                    return
                  }
                  if (state === 'final') {
                    sendEvent('done', { state: 'final', runId, sessionKey: targetSessionKey, message })
                    return
                  }
                  if (state === 'error') {
                    sendEvent('done', { state: 'error', errorMessage: rawPayload?.errorMessage, runId, sessionKey: targetSessionKey })
                    return
                  }
                  if (state === 'aborted') {
                    sendEvent('done', { state: 'aborted', runId, sessionKey: targetSessionKey })
                    return
                  }
                  if (message?.role === 'user') {
                    sendEvent('user_message', { message, sessionKey: targetSessionKey, source: rawPayload?.source || rawPayload?.channel || 'external' })
                    return
                  }
                  if (message?.role === 'assistant' && !state) {
                    sendEvent('message', { message, sessionKey: targetSessionKey })
                    return
                  }
                  if (state === 'started' || state === 'thinking') {
                    sendEvent('state', { state, runId, sessionKey: targetSessionKey })
                  }
                  return
                }

                // Other message events
                if (eventName === 'message.received' || eventName === 'chat.message' || eventName === 'channel.message') {
                  const message = rawPayload?.message || rawPayload
                  if (message?.role === 'user') {
                    sendEvent('user_message', { message, sessionKey: targetSessionKey, source: rawPayload?.source || rawPayload?.channel || eventName })
                  } else if (message?.role === 'assistant') {
                    sendEvent('message', { message, sessionKey: targetSessionKey })
                  }
                }
              })

              // Heartbeat to keep SSE alive
              heartbeatTimer = setInterval(() => {
                sendEvent('heartbeat', { timestamp: Date.now() })
              }, 30000)

            } catch (err) {
              const errorMsg = err instanceof Error ? err.message : String(err)
              sendEvent('error', { message: errorMsg })
              closeStream()
            }
          },
          cancel() {
            streamClosed = true
            if (heartbeatTimer) { clearInterval(heartbeatTimer); heartbeatTimer = null }
            if (cleanupListener) { cleanupListener(); cleanupListener = null }
          },
        })

        return new Response(stream, {
          headers: {
            'Content-Type': 'text/event-stream',
            'Cache-Control': 'no-cache, no-transform',
            Connection: 'keep-alive',
            'X-Accel-Buffering': 'no',
          },
        })
      },
    },
  },
})
