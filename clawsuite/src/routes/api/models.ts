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

          // If we have models from gateway or config, return them
          if (filteredModels.length > 0) {
            return json({
              ok: true,
              models: filteredModels,
              configuredProviders,
            })
          }

          // Fall through to env fallback if no models found
        } catch {
          // Gateway unavailable — fall through to env fallback
        }

        // Fallback: derive model from env
        console.warn('[models] falling back to env model list')
        const modelId = process.env.ZEROCLAW_MODEL?.trim() || 'kimi-k2.5'
        const provider = process.env.PROVIDER?.trim() || 'moonshot-intl'

        const fallbackModels: ModelEntry[] = [
          {
            id: modelId,
            provider,
            name: modelId,
          },
        ]

        // Use configured providers if available, otherwise fall back to env provider
        let configuredProviders: string[]
        try {
          configuredProviders = getConfiguredProviderNames()
          if (configuredProviders.length === 0) {
            configuredProviders = [provider]
          }
        } catch {
          configuredProviders = [provider]
        }

        return json({
          ok: true,
          models: fallbackModels,
          configuredProviders,
        })
      },
    },
  },
})