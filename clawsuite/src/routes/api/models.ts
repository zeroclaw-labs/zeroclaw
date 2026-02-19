import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'
import {
  getConfiguredModelIds,
  getConfiguredProviderNames,
  getConfiguredModelsFromConfig,
} from '../../server/providers'

type ModelsListGatewayResponse = {
  models?: Array<unknown>
}

type ModelEntry = {
  provider?: string
  id?: string
  name?: string
  [key: string]: unknown
}

export const Route = createFileRoute('/api/models')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const payload = await gatewayRpc<ModelsListGatewayResponse>(
            'models.list',
            {},
          )
          const allModels = Array.isArray(payload.models) ? payload.models : []

          // Filter to only configured providers AND configured model IDs
          const configuredProviders = getConfiguredProviderNames()
          const configuredModelIds = getConfiguredModelIds()
          const providerSet = new Set(configuredProviders)

          const filteredModels = allModels.filter((model) => {
            if (typeof model === 'string') return false
            const entry = model as ModelEntry

            // Must be from a configured provider
            if (!entry.provider || !providerSet.has(entry.provider)) {
              return false
            }

            // Must be a configured model ID
            if (!entry.id || !configuredModelIds.has(entry.id)) {
              return false
            }

            return true
          })

          // Merge in any models from config that the gateway didn't auto-discover
          const discoveredIds = new Set(filteredModels.map((m) => (m as ModelEntry).id))
          const configModels = getConfiguredModelsFromConfig()
          for (const cm of configModels) {
            if (!discoveredIds.has(cm.id)) {
              filteredModels.push(cm)
            }
          }

          return json({
            ok: true,
            models: filteredModels,
            configuredProviders,
          })
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 503 },
          )
        }
      },
    },
  },
})
