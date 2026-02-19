import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { getProviderUsage } from '../../server/provider-usage'

export type {
  ProviderUsageResult,
  ProviderUsageResponse,
  UsageLine,
} from '../../server/provider-usage'

export const Route = createFileRoute('/api/provider-usage')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        try {
          const url = new URL(request.url)
          const force = url.searchParams.get('force') === '1'
          const payload = await getProviderUsage(force)
          return json(payload)
        } catch (err) {
          return json(
            {
              ok: false,
              updatedAt: Date.now(),
              providers: [],
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 503 },
          )
        }
      },
    },
  },
})
