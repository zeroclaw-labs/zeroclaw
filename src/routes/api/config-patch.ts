import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'
import { isAuthenticated } from '../../server/auth-middleware'

export const Route = createFileRoute('/api/config-patch')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        // Auth check
        if (!isAuthenticated(request)) {
          return json({ ok: false, error: 'Unauthorized' }, { status: 401 })
        }

        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >
          const raw = typeof body.raw === 'string' ? body.raw : ''

          if (!raw.trim()) {
            return json(
              { ok: false, error: 'raw config patch required' },
              { status: 400 },
            )
          }

          // Get current config hash for optimistic concurrency
          const configResult = await gatewayRpc<{ hash?: string }>('config.get')
          const baseHash = (configResult as any)?.hash

          const params: Record<string, unknown> = { raw }
          if (baseHash) {
            params.baseHash = baseHash
          }

          const result = await gatewayRpc<{ ok: boolean; error?: string }>(
            'config.patch',
            params,
          )

          return json({ ...result, ok: true })
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
