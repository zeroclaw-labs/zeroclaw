import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'
import { generateSessionTitle } from '@/utils/generate-session-title'

const MAX_MESSAGES = 8
const MAX_CHAR_PER_MESSAGE = 600
const SUBSTANTIVE_FIRST_USER_CHARS = 20

const GENERIC_TITLE_PATTERNS = [
  /^a new session/i,
  /^new session/i,
  /^untitled/i,
  /^session \d/i,
  /^conversation$/i,
  /^chat$/i,
  /^[0-9a-f]{6,}/i,
  /^\w{8} \(\d{4}-\d{2}-\d{2}\)$/,
]

function isGenericTitle(title: string): boolean {
  const trimmed = title.trim()
  if (!trimmed || trimmed === 'New Session') return true
  return GENERIC_TITLE_PATTERNS.some((pattern) => pattern.test(trimmed))
}

function normalizeMessages(
  raw: unknown,
): Array<{ role: string; text: string }> {
  if (!Array.isArray(raw)) return []
  const normalized: Array<{ role: string; text: string }> = []
  for (const entry of raw) {
    if (!entry || typeof entry !== 'object') continue
    const roleRaw = (entry as { role?: unknown }).role
    const textRaw = (entry as { text?: unknown }).text
    const role = typeof roleRaw === 'string' ? roleRaw : 'user'
    if (role !== 'user' && role !== 'assistant') continue
    const text = typeof textRaw === 'string' ? textRaw.trim() : ''
    if (!text) continue
    normalized.push({
      role,
      text: text.slice(0, MAX_CHAR_PER_MESSAGE),
    })
    if (normalized.length >= MAX_MESSAGES) break
  }
  return normalized
}

function minimumMessagesForTitle(
  messages: Array<{ role: string; text: string }>,
): number {
  const firstUser = messages.find((message) => message.role === 'user')
  if ((firstUser?.text.trim().length ?? 0) > SUBSTANTIVE_FIRST_USER_CHARS) {
    return 1
  }
  return 2
}

function fallbackTitle(
  messages: Array<{ role: string; text: string }>,
  maxWords: number,
): string {
  return generateSessionTitle(messages, {
    maxLength: 48,
    maxWords,
  })
}

function resolveLocalTitle(
  messages: Array<{ role: string; text: string }>,
  maxWords: number,
): string {
  const fallback = fallbackTitle(messages, maxWords).trim()
  if (!fallback || isGenericTitle(fallback)) return ''
  return fallback
}

type GatewayTitleResponse = {
  ok?: boolean
  title?: string
  error?: string
}

export const Route = createFileRoute('/api/session-title')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >

          const sessionKey =
            typeof body.sessionKey === 'string' &&
            body.sessionKey.trim().length > 0
              ? body.sessionKey.trim()
              : ''
          const friendlyId =
            typeof body.friendlyId === 'string' &&
            body.friendlyId.trim().length > 0
              ? body.friendlyId.trim()
              : ''
          const messages = normalizeMessages(body.messages)
          const maxWords = Math.max(3, Math.min(8, Number(body.maxWords) || 6))

          if (!sessionKey && !friendlyId) {
            return json(
              { ok: false, error: 'sessionKey or friendlyId required' },
              { status: 400 },
            )
          }

          if (messages.length < minimumMessagesForTitle(messages)) {
            return json(
              { ok: false, error: 'insufficient message context' },
              { status: 400 },
            )
          }

          let title = ''
          let usedFallback = false
          let gatewayError = ''
          const snippet = [...messages]

          try {
            const payload = await gatewayRpc<GatewayTitleResponse>(
              'sessions.generateTitle',
              {
                sessionKey: sessionKey || undefined,
                friendlyId: friendlyId || undefined,
                messages: snippet,
                maxWords,
              },
            )
            // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
            if (payload && typeof payload.title === 'string') {
              title = payload.title.trim()
            }
          } catch (err) {
            gatewayError = err instanceof Error ? err.message : String(err)
          }

          if (title && isGenericTitle(title)) {
            title = ''
          }

          if (!title) {
            const fallback = resolveLocalTitle(messages, maxWords)
            if (fallback) {
              title = fallback
              usedFallback = true
            } else if (gatewayError) {
              return json(
                {
                  ok: false,
                  error: gatewayError || 'failed to generate title',
                },
                { status: 502 },
              )
            }
          }

          if (!title) {
            return json(
              { ok: false, error: 'unable to derive title' },
              { status: 422 },
            )
          }

          const trimmed = title.trim().replace(/\s+/g, ' ')

          return json({
            ok: true,
            title: trimmed,
            fallback: usedFallback,
            source: usedFallback ? 'fallback' : 'gateway',
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
    },
  },
})
