import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import {
  ensureActivityStreamStarted,
  getActivityStreamDiagnostics,
  sanitizeText,
} from '../../../server/activity-stream'
import { isAuthenticated } from '../../../server/auth-middleware'

const DEFAULT_GATEWAY_URL = 'ws://127.0.0.1:18789'

function readGatewayUrl(): string {
  return process.env.CLAWDBOT_GATEWAY_URL?.trim() || DEFAULT_GATEWAY_URL
}

function stripAuthorityAuth(value: string): string {
  if (!value.includes('@')) return value
  const parts = value.split('@')
  const lastPart = parts[parts.length - 1]
  return lastPart || value
}

function maskGatewayUrl(rawUrl: string): string {
  const sanitizedUrl = sanitizeText(rawUrl)

  try {
    const parsed = new URL(sanitizedUrl)
    const safeHost = stripAuthorityAuth(parsed.host)
    return `${parsed.protocol}//${safeHost}`
  } catch {
    const hostMatch = sanitizedUrl.match(/^([a-z]+:\/\/)?([^/]+)/i)
    if (!hostMatch) return 'Unavailable'
    const protocol = hostMatch[1] || ''
    const authority = hostMatch[2] || ''
    const safeAuthority = stripAuthorityAuth(authority)
    return `${protocol}${safeAuthority}` || 'Unavailable'
  }
}

export const Route = createFileRoute('/api/debug/status')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        if (!isAuthenticated(request)) {
          return json({ error: 'Unauthorized' }, { status: 401 })
        }

        void ensureActivityStreamStarted().catch(function ignoreStartError() {
          // endpoint still returns diagnostics while disconnected
        })

        const diagnostics = getActivityStreamDiagnostics()
        return json({
          state: diagnostics.status,
          gatewayUrl: maskGatewayUrl(readGatewayUrl()),
          connectedSinceMs: diagnostics.connectedSinceMs,
          lastDisconnectedAtMs: diagnostics.lastDisconnectedAtMs,
          nowMs: Date.now(),
        })
      },
    },
  },
})
