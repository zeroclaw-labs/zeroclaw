import { randomUUID } from 'node:crypto'
import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { z } from 'zod'
import { gatewayRpc } from '../../server/gateway'
import {
  getClientIp,
  rateLimit,
  rateLimitResponse,
  safeErrorMessage,
} from '../../server/rate-limit'
import { isAuthenticated } from '../../server/auth-middleware'

const SendSchema = z.object({
  sessionKey: z.string().trim().max(200).default(''),
  friendlyId: z.string().trim().max(200).default(''),
  message: z.string().max(100_000).default(''),
  thinking: z.string().max(50).optional(),
  attachments: z.array(z.unknown()).optional(),
  clientId: z.string().trim().max(100).optional(),
  idempotencyKey: z.string().max(100).optional(),
})

type SessionsResolveResponse = {
  ok?: boolean
  key?: string
}

export const Route = createFileRoute('/api/send')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        // Auth check
        if (!isAuthenticated(request)) {
          return json({ ok: false, error: 'Unauthorized' }, { status: 401 })
        }

        // Rate limit: 30 requests per minute per IP
        const ip = getClientIp(request)
        if (!rateLimit(`send:${ip}`, 30, 60_000)) {
          return rateLimitResponse()
        }

        try {
          const raw = await request.json().catch(() => ({}))
          const parsed = SendSchema.safeParse(raw)
          if (!parsed.success) {
            return json(
              { ok: false, error: 'Invalid request body' },
              { status: 400 },
            )
          }
          const body = parsed.data

          const rawSessionKey = body.sessionKey
          const friendlyId = body.friendlyId
          const message = body.message
          const thinking = body.thinking
          const attachments = body.attachments
          const clientId = body.clientId

          if (!message.trim() && (!attachments || attachments.length === 0)) {
            return json(
              { ok: false, error: 'message required' },
              { status: 400 },
            )
          }

          // Try to resolve session key — it might be a friendlyId that needs resolution
          const keysToResolve = [rawSessionKey, friendlyId].filter(
            (k) => k.length > 0,
          )
          let sessionKey = ''

          for (const candidate of keysToResolve) {
            try {
              const resolved = await gatewayRpc<SessionsResolveResponse>(
                'sessions.resolve',
                {
                  key: candidate,
                  includeUnknown: true,
                  includeGlobal: true,
                },
              )
              const resolvedKey =
                typeof resolved.key === 'string' ? resolved.key.trim() : ''
              if (resolvedKey.length > 0) {
                sessionKey = resolvedKey
                break
              }
            } catch {
              // Resolution failed, try next candidate
            }
          }

          // If resolution failed but we have a raw key, use it directly
          // (it might be a full gateway key like agent:codex:main)
          if (!sessionKey && rawSessionKey.length > 0) {
            sessionKey = rawSessionKey
          }

          if (sessionKey.length === 0) {
            sessionKey = 'main'
          }

          const sendPayload: Record<string, unknown> = {
            sessionKey,
            message,
            thinking,
            attachments,
            deliver: false,
            timeoutMs: 120_000,
            idempotencyKey:
              typeof body.idempotencyKey === 'string'
                ? body.idempotencyKey
                : randomUUID(),
          }
          // Note: clientId is NOT sent to gateway (chat.send rejects unknown props)
          // It's only used for client-side optimistic message matching

          const res = await gatewayRpc<{ runId: string }>(
            'chat.send',
            sendPayload,
          )

          return json({
            ok: true,
            ...res,
            sessionKey,
            clientId: clientId ?? null,
          })
        } catch (err) {
          if (import.meta.env.DEV) console.error(
            '[/api/send] Error:',
            err instanceof Error ? err.message : String(err),
          )
          return json(
            { ok: false, error: safeErrorMessage(err) },
            { status: 500 },
          )
        }
      },
    },
  },
})
