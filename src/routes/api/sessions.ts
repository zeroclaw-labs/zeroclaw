import { randomUUID } from 'node:crypto'
import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'
import { isAuthenticated } from '../../server/auth-middleware'

type SessionsListGatewayResponse = {
  sessions?: Array<Record<string, unknown>>
}

type SessionsListResponse = {
  sessions: Array<Record<string, unknown>>
}

type SessionsPatchResponse = {
  ok?: boolean
  key?: string
  path?: string
  entry?: Record<string, unknown>
}

type SessionsResolveResponse = {
  ok?: boolean
  key?: string
}

function deriveFriendlyIdFromKey(key: unknown): string {
  if (typeof key !== 'string' || key.trim().length === 0) return 'main'
  const parts = key.split(':')
  const tail = parts[parts.length - 1]
  return tail && tail.trim().length > 0 ? tail.trim() : key
}

function normalizeSessions(
  payload: SessionsListGatewayResponse,
): SessionsListResponse {
  const sessions: Array<Record<string, unknown>> = Array.isArray(
    payload.sessions,
  )
    ? payload.sessions
    : []
  const normalized = sessions.map((session) => {
    const rawKey = session.key
    const key = typeof rawKey === 'string' ? rawKey : ''
    const rawFriendly = session.friendlyId
    const friendlyIdFromPayload =
      typeof rawFriendly === 'string' ? rawFriendly.trim() : ''
    const friendlyId =
      friendlyIdFromPayload.length > 0
        ? friendlyIdFromPayload
        : deriveFriendlyIdFromKey(key)
    return {
      ...session,
      key,
      friendlyId,
    }
  })

  return { sessions: normalized }
}

export const Route = createFileRoute('/api/sessions')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        // Auth check
        if (!isAuthenticated(request)) {
          return json({ ok: false, error: 'Unauthorized' }, { status: 401 })
        }

        try {
          const payload = await gatewayRpc<SessionsListGatewayResponse>(
            'sessions.list',
            {
              limit: 50,
              includeLastMessage: true,
              includeDerivedTitles: true,
            },
          )

          return json(normalizeSessions(payload))
        } catch (err) {
          return json(
            {
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >

          const requestedLabel =
            typeof body.label === 'string' ? body.label.trim() : ''
          const label = requestedLabel || undefined

          const requestedFriendlyId =
            typeof body.friendlyId === 'string' ? body.friendlyId.trim() : ''
          const friendlyId = requestedFriendlyId || randomUUID()

          const params: Record<string, unknown> = { key: friendlyId }
          if (label) params.label = label

          const payload = await gatewayRpc<SessionsPatchResponse>(
            'sessions.patch',
            params,
          )

          const sessionKeyRaw = payload.key
          const sessionKey =
            typeof sessionKeyRaw === 'string' && sessionKeyRaw.trim().length > 0
              ? sessionKeyRaw.trim()
              : ''
          if (sessionKey.length === 0) {
            throw new Error('gateway returned an invalid response')
          }

          // Register the friendly id so subsequent lookups resolve quickly.
          await gatewayRpc<SessionsResolveResponse>('sessions.resolve', {
            key: friendlyId,
            includeUnknown: true,
            includeGlobal: true,
          }).catch(() => ({ ok: false }))

          return json({
            ok: true,
            sessionKey,
            friendlyId,
            entry: payload.entry,
          })
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
      PATCH: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >

          const rawSessionKey =
            typeof body.sessionKey === 'string' ? body.sessionKey.trim() : ''
          const rawFriendlyId =
            typeof body.friendlyId === 'string' ? body.friendlyId.trim() : ''
          const label =
            typeof body.label === 'string' ? body.label.trim() : undefined

          let sessionKey = rawSessionKey
          const friendlyId = rawFriendlyId

          if (friendlyId) {
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
            if (resolvedKey.length > 0) sessionKey = resolvedKey
          }

          if (!sessionKey) {
            return json(
              { ok: false, error: 'sessionKey required' },
              { status: 400 },
            )
          }

          const params: Record<string, unknown> = { key: sessionKey }
          if (label) params.label = label

          const payload = await gatewayRpc<SessionsPatchResponse>(
            'sessions.patch',
            params,
          )

          return json({
            ok: true,
            sessionKey,
            entry: payload.entry,
          })
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
      DELETE: async ({ request }) => {
        try {
          const url = new URL(request.url)
          const rawSessionKey = url.searchParams.get('sessionKey') ?? ''
          const rawFriendlyId = url.searchParams.get('friendlyId') ?? ''
          let sessionKey = rawSessionKey.trim()
          const friendlyId = rawFriendlyId.trim()

          if (friendlyId) {
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
            if (resolvedKey.length > 0) sessionKey = resolvedKey
          }

          if (!sessionKey) {
            return json(
              { ok: false, error: 'sessionKey required' },
              { status: 400 },
            )
          }

          await gatewayRpc('sessions.delete', { key: sessionKey })
          if (friendlyId && friendlyId !== sessionKey) {
            await gatewayRpc('sessions.delete', { key: friendlyId }).catch(
              () => ({}),
            )
          }

          return json({ ok: true, sessionKey })
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
