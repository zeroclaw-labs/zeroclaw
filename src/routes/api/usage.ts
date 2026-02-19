import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'
import { getConfiguredProviderNames } from '../../server/providers'
import {
  buildUsageSummary,
  isGatewayMethodUnavailable,
} from '../../server/usage-cost'

const UNAVAILABLE_MESSAGE = 'Unavailable on this Gateway version'

function readErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim().length > 0) {
    return error.message
  }
  return String(error)
}

export const Route = createFileRoute('/api/usage')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const configuredProviders = getConfiguredProviderNames()

          const sessionsUsagePayload = await gatewayRpc('sessions.usage', {
            limit: 1000,
            includeContextWeight: true,
          })

          let usageStatusPayload: unknown
          try {
            usageStatusPayload = await gatewayRpc('usage.status', {})
          } catch (error) {
            if (!isGatewayMethodUnavailable(error)) {
              // Keep usage totals available even when provider quota snapshots fail.
              usageStatusPayload = undefined
            }
          }

          const usage = buildUsageSummary({
            configuredProviders,
            sessionsUsagePayload,
            usageStatusPayload,
          })

          return json({ ok: true, usage })
        } catch (error) {
          if (isGatewayMethodUnavailable(error)) {
            return json(
              {
                ok: false,
                unavailable: true,
                error: UNAVAILABLE_MESSAGE,
              },
              { status: 501 },
            )
          }

          return json(
            {
              ok: false,
              error: readErrorMessage(error),
            },
            { status: 503 },
          )
        }
      },
    },
  },
})
