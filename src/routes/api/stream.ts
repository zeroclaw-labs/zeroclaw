import { randomUUID } from 'node:crypto'
import { createFileRoute } from '@tanstack/react-router'
import WebSocket from 'ws'
import { isAuthenticated } from '../../server/auth-middleware'

type GatewayFrame =
  | { type: 'req'; id: string; method: string; params?: unknown }
  | {
      type: 'res'
      id: string
      ok: boolean
      payload?: unknown
      error?: { code: string; message: string; details?: unknown }
    }
  | { type: 'event'; event: string; payload?: unknown; seq?: number }

type ConnectParams = {
  minProtocol: number
  maxProtocol: number
  client: {
    id: string
    displayName?: string
    version: string
    platform: string
    mode: string
    instanceId?: string
  }
  auth?: { token?: string; password?: string }
  role?: 'operator' | 'node'
  scopes?: Array<string>
}

function getGatewayConfig() {
  const url = process.env.CLAWDBOT_GATEWAY_URL?.trim() || 'ws://127.0.0.1:18789'
  const token = process.env.CLAWDBOT_GATEWAY_TOKEN?.trim() || ''
  const password = process.env.CLAWDBOT_GATEWAY_PASSWORD?.trim() || ''

  if (!token && !password) {
    throw new Error(
      'Missing gateway auth. Set CLAWDBOT_GATEWAY_TOKEN (recommended) or CLAWDBOT_GATEWAY_PASSWORD in the server environment.',
    )
  }

  return { url, token, password }
}

function buildConnectParams(token: string, password: string): ConnectParams {
  return {
    minProtocol: 3,
    maxProtocol: 3,
    client: {
      id: 'gateway-client-stream',
      displayName: 'clawsuite-stream',
      version: 'dev',
      platform: process.platform,
      mode: 'ui',
      instanceId: randomUUID(),
    },
    auth: {
      token: token || undefined,
      password: password || undefined,
    },
    role: 'operator',
    scopes: ['operator.admin'],
  }
}

export const Route = createFileRoute('/api/stream')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        // Auth check
        if (!isAuthenticated(request)) {
          return new Response(
            JSON.stringify({ ok: false, error: 'Unauthorized' }),
            { status: 401, headers: { 'Content-Type': 'application/json' } },
          )
        }

        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >

          const sessionKey =
            typeof body.sessionKey === 'string' ? body.sessionKey.trim() : ''
          const friendlyId =
            typeof body.friendlyId === 'string' ? body.friendlyId.trim() : ''
          const message = String(body.message ?? '')
          const thinking =
            typeof body.thinking === 'string' ? body.thinking : undefined
          const attachments = Array.isArray(body.attachments)
            ? body.attachments
            : undefined

          if (!message.trim() && (!attachments || attachments.length === 0)) {
            return new Response(
              JSON.stringify({ ok: false, error: 'message required' }),
              {
                status: 400,
                headers: { 'Content-Type': 'application/json' },
              },
            )
          }

          const { url, token, password } = getGatewayConfig()

          // Create SSE response stream
          const encoder = new TextEncoder()
          const stream = new ReadableStream({
            async start(controller) {
              const ws = new WebSocket(url)
              let connected = false
              let runId: string | null = null

              const sendSSE = (event: string, data: unknown) => {
                const chunk = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`
                controller.enqueue(encoder.encode(chunk))
              }

              const cleanup = () => {
                try {
                  if (
                    ws.readyState === ws.OPEN ||
                    ws.readyState === ws.CONNECTING
                  ) {
                    ws.close()
                  }
                } catch {
                  // ignore
                }
              }

              ws.on('open', async () => {
                try {
                  // Send connect handshake
                  const connectId = randomUUID()
                  const connectParams = buildConnectParams(token, password)
                  ws.send(
                    JSON.stringify({
                      type: 'req',
                      id: connectId,
                      method: 'connect',
                      params: connectParams,
                    }),
                  )
                } catch (err) {
                  sendSSE('error', { message: 'Failed to connect to gateway' })
                  cleanup()
                  controller.close()
                }
              })

              ws.on('message', (data) => {
                try {
                  const parsed = JSON.parse(data.toString()) as GatewayFrame

                  if (parsed.type === 'res') {
                    if (!connected) {
                      // Connect response
                      if (parsed.ok) {
                        connected = true
                        // Now send the chat message
                        const chatId = randomUUID()
                        ws.send(
                          JSON.stringify({
                            type: 'req',
                            id: chatId,
                            method: 'chat.send',
                            params: {
                              sessionKey: sessionKey || 'main',
                              friendlyId: friendlyId || undefined,
                              message,
                              thinking,
                              attachments,
                              deliver: false,
                              stream: true, // Request streaming if supported
                              timeoutMs: 120_000,
                              idempotencyKey: randomUUID(),
                            },
                          }),
                        )
                        sendSSE('connected', { sessionKey, friendlyId })
                      } else {
                        sendSSE('error', {
                          message: parsed.error?.message ?? 'Connection failed',
                        })
                        cleanup()
                        controller.close()
                      }
                    } else {
                      // Chat response
                      if (parsed.ok) {
                        const payload = parsed.payload as {
                          runId?: string
                          text?: string
                          content?: Array<{ type: string; text?: string }>
                          message?: {
                            content?: Array<{ type: string; text?: string }>
                          }
                        }
                        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
                        runId = payload?.runId ?? null
                        sendSSE('started', { runId })

                        // If the response includes the full message text, emit it as a complete event
                        // This handles gateways that don't support streaming
                        let responseText = ''
                        if (typeof payload.text === 'string') {
                          responseText = payload.text
                        } else if (Array.isArray(payload.content)) {
                          responseText = payload.content
                            .filter((c) => c.type === 'text' && c.text)
                            .map((c) => c.text)
                            .join('')
                        } else if (payload.message?.content) {
                          responseText = payload.message.content
                            .filter((c) => c.type === 'text' && c.text)
                            .map((c) => c.text)
                            .join('')
                        }

                        if (responseText) {
                          // Emit the full response as chunks for a typing effect
                          // Split by words for word-by-word streaming simulation
                          const words = responseText.split(/(\s+)/)
                          let accumulated = ''
                          for (const word of words) {
                            accumulated += word
                            sendSSE('chunk', { delta: word, text: accumulated })
                          }
                          sendSSE('complete', { text: responseText })
                          cleanup()
                          controller.close()
                        }
                      } else {
                        sendSSE('error', {
                          message: parsed.error?.message ?? 'Chat send failed',
                        })
                        cleanup()
                        controller.close()
                      }
                    }
                  } else if (parsed.type === 'event') {
                    // Forward streaming events
                    const eventName = parsed.event
                    const payload = parsed.payload as Record<string, unknown>

                    if (
                      eventName === 'chat.chunk' ||
                      eventName === 'chat.delta' ||
                      eventName === 'message.delta'
                    ) {
                      // Streaming text chunk
                      sendSSE('chunk', payload)
                    } else if (
                      eventName === 'chat.complete' ||
                      eventName === 'chat.done' ||
                      eventName === 'message.complete'
                    ) {
                      // Stream complete
                      sendSSE('complete', payload)
                      cleanup()
                      controller.close()
                    } else if (
                      eventName === 'chat.error' ||
                      eventName === 'message.error'
                    ) {
                      sendSSE('error', payload)
                      cleanup()
                      controller.close()
                    } else if (eventName === 'chat.thinking') {
                      // Thinking/reasoning updates
                      sendSSE('thinking', payload)
                    } else if (eventName === 'chat.tool') {
                      // Tool call updates
                      sendSSE('tool', payload)
                    } else {
                      // Forward other events
                      sendSSE('event', { event: eventName, ...payload })
                    }
                  }
                } catch {
                  // Ignore parse errors
                }
              })

              ws.on('error', (err) => {
                sendSSE('error', { message: String(err.message || err) })
                cleanup()
                controller.close()
              })

              ws.on('close', () => {
                sendSSE('close', {})
                try {
                  controller.close()
                } catch {
                  // Already closed
                }
              })

              // Set a timeout to close the connection if no response
              setTimeout(() => {
                if (ws.readyState === ws.OPEN) {
                  sendSSE('timeout', { message: 'Request timed out' })
                  cleanup()
                  try {
                    controller.close()
                  } catch {
                    // Already closed
                  }
                }
              }, 125_000)
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
        } catch (err) {
          return new Response(
            JSON.stringify({
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            }),
            {
              status: 500,
              headers: { 'Content-Type': 'application/json' },
            },
          )
        }
      },
    },
  },
})
