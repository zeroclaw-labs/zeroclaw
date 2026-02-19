import { randomUUID } from 'node:crypto'
import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../../server/gateway'

type SessionsResolveResponse = {
  ok?: boolean
  key?: string
}

type SendGatewayResponse = {
  runId?: string
}

function looksLikeMethodMissingError(error: unknown): boolean {
  if (!(error instanceof Error)) return false
  const message = error.message.toLowerCase()
  return (
    message.includes('method') &&
    (message.includes('not found') || message.includes('unknown'))
  )
}

async function sendMessageViaGateway(payload: {
  sessionKey: string
  message: string
  idempotencyKey: string
}) {
  try {
    return await gatewayRpc<SendGatewayResponse>('sessions.send', {
      sessionKey: payload.sessionKey,
      message: payload.message,
      timeoutMs: 120_000,
      idempotencyKey: payload.idempotencyKey,
    })
  } catch (error) {
    if (!looksLikeMethodMissingError(error)) {
      throw error
    }

    return gatewayRpc<SendGatewayResponse>('chat.send', {
      sessionKey: payload.sessionKey,
      message: payload.message,
      deliver: false,
      timeoutMs: 120_000,
      idempotencyKey: payload.idempotencyKey,
    })
  }
}

export const Route = createFileRoute('/api/sessions/send')({
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
          const friendlyId =
            typeof body.friendlyId === 'string' ? body.friendlyId.trim() : ''
          const message = String(body.message ?? '').trim()

          if (!message) {
            return json(
              { ok: false, error: 'message required' },
              { status: 400 },
            )
          }

          let sessionKey = rawSessionKey.length > 0 ? rawSessionKey : ''

          if (!sessionKey && friendlyId) {
            const resolved = await gatewayRpc<SessionsResolveResponse>(
              'sessions.resolve',
              {
                key: friendlyId,
                includeUnknown: true,
                includeGlobal: true,
              },
            )
            const resolvedKey =
              typeof resolved.key === 'string' ? resolved.key.trim() : ''
            if (!resolvedKey) {
              return json(
                { ok: false, error: 'session not found' },
                { status: 404 },
              )
            }
            sessionKey = resolvedKey
          }

          if (!sessionKey) {
            sessionKey = 'main'
          }

          const idempotencyKey =
            typeof body.idempotencyKey === 'string' &&
            body.idempotencyKey.trim().length > 0
              ? body.idempotencyKey.trim()
              : randomUUID()

          const result = await sendMessageViaGateway({
            sessionKey,
            message,
            idempotencyKey,
          })

          return json({ ok: true, sessionKey, runId: result.runId ?? null })
        } catch (error) {
          return json(
            {
              ok: false,
              error: error instanceof Error ? error.message : String(error),
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
