import { randomUUID } from 'node:crypto'
import { createFileRoute } from '@tanstack/react-router'
import { gatewayRpc, onGatewayEvent, gatewayConnectCheck } from '../../server/gateway'
import type { GatewayFrame } from '../../server/gateway'
import { resolveSessionKey } from '../../server/session-utils'
import { isAuthenticated } from '../../server/auth-middleware'

export const Route = createFileRoute('/api/send-stream')({
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

        const body = (await request.json().catch(() => ({}))) as Record<
          string,
          unknown
        >

        const rawSessionKey =
          typeof body.sessionKey === 'string' ? body.sessionKey.trim() : ''
        const friendlyId =
          typeof body.friendlyId === 'string' ? body.friendlyId.trim() : ''
        const message = String(body.message ?? '')
        const thinking =
          typeof body.thinking === 'string' ? body.thinking : undefined
        const attachments = Array.isArray(body.attachments)
          ? body.attachments
          : undefined
        const idempotencyKey =
          typeof body.idempotencyKey === 'string'
            ? body.idempotencyKey
            : randomUUID()

        if (!message.trim() && (!attachments || attachments.length === 0)) {
          return new Response(
            JSON.stringify({ ok: false, error: 'message required' }),
            {
              status: 400,
              headers: { 'Content-Type': 'application/json' },
            },
          )
        }

        // Resolve session key
        let sessionKey: string
        try {
          const resolved = await resolveSessionKey({
            rawSessionKey,
            friendlyId,
            defaultKey: 'main',
          })
          sessionKey = resolved.sessionKey
        } catch (err) {
          const errorMsg = err instanceof Error ? err.message : String(err)
          if (errorMsg === 'session not found') {
            return new Response(
              JSON.stringify({ ok: false, error: 'session not found' }),
              {
                status: 404,
                headers: { 'Content-Type': 'application/json' },
              },
            )
          }
          return new Response(JSON.stringify({ ok: false, error: errorMsg }), {
            status: 500,
            headers: { 'Content-Type': 'application/json' },
          })
        }

        // Create streaming response using the SHARED gateway connection
        const encoder = new TextEncoder()
        let streamClosed = false
        let cleanupListener: (() => void) | null = null

        const stream = new ReadableStream({
          async start(controller) {
            const sendEvent = (event: string, data: unknown) => {
              if (streamClosed) return
              const payload = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`
              controller.enqueue(encoder.encode(payload))
            }

            const closeStream = () => {
              if (streamClosed) return
              streamClosed = true
              if (cleanupListener) {
                cleanupListener()
                cleanupListener = null
              }
              try {
                controller.close()
              } catch {
                // ignore
              }
            }

            try {
              // Ensure shared gateway connection is active
              await gatewayConnectCheck()

              // Listen for events on the shared connection
              cleanupListener = onGatewayEvent((frame: GatewayFrame) => {
                if (frame.type !== 'evt' && frame.type !== 'event') return
                const eventName = (frame as any).event as string
                const payload = parsePayload(frame)

                if (eventName === 'agent') {
                  const agentPayload = payload as any
                  const stream = agentPayload?.stream
                  const data = agentPayload?.data

                  if (stream === 'assistant' && data?.text) {
                    sendEvent('assistant', {
                      text: data.text,
                      runId: agentPayload?.runId,
                    })
                  } else if (stream === 'tool') {
                    sendEvent('tool', {
                      phase: data?.phase,
                      name: data?.name,
                      toolCallId: data?.toolCallId,
                      args: data?.args,
                      runId: agentPayload?.runId,
                    })
                  } else if (stream === 'thinking' && data?.text) {
                    sendEvent('thinking', {
                      text: data.text,
                      runId: agentPayload?.runId,
                    })
                  }
                } else if (eventName === 'chat') {
                  const chatPayload = payload as any
                  const state = chatPayload?.state
                  if (
                    state === 'final' ||
                    state === 'aborted' ||
                    state === 'error'
                  ) {
                    sendEvent('done', {
                      state,
                      errorMessage: chatPayload?.errorMessage,
                      runId: chatPayload?.runId,
                    })
                    closeStream()
                  }
                }
              })

              // Send the chat message via shared RPC
              const sendResult = await gatewayRpc<{ runId?: string }>(
                'chat.send',
                {
                  sessionKey,
                  message,
                  thinking,
                  attachments,
                  deliver: false,
                  timeoutMs: 120_000,
                  idempotencyKey,
                },
              )

              // Send initial event with runId
              sendEvent('started', {
                runId: sendResult.runId,
                sessionKey,
              })

              // Set a timeout to close the stream if no completion event
              setTimeout(() => {
                if (!streamClosed) {
                  sendEvent('error', { message: 'Stream timeout' })
                  closeStream()
                }
              }, 180_000) // 3 minute timeout
            } catch (err) {
              const errorMsg = err instanceof Error ? err.message : String(err)
              sendEvent('error', { message: errorMsg })
              closeStream()
            }
          },
          cancel() {
            streamClosed = true
            if (cleanupListener) {
              cleanupListener()
              cleanupListener = null
            }
          },
        })

        return new Response(stream, {
          headers: {
            'Content-Type': 'text/event-stream',
            'Cache-Control': 'no-cache',
            Connection: 'keep-alive',
          },
        })
      },
    },
  },
})

function parsePayload(frame: any): unknown {
  if (frame.payload !== undefined) return frame.payload
  if (typeof frame.payloadJSON === 'string') {
    try { return JSON.parse(frame.payloadJSON) } catch { return null }
  }
  return null
}
