import { Add01Icon, CheckmarkCircle02Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { Button } from '@/components/ui/button'
import { fetchModels } from '@/lib/gateway-api'
import type { GatewayModelCatalogEntry } from '@/lib/gateway-api'
import {
  getProviderDisplayName,
  getProviderInfo,
  normalizeProviderId,
} from '@/lib/provider-catalog'
import { cn } from '@/lib/utils'
import { ProviderIcon } from './components/provider-icon'
import { ProviderWizard } from './components/provider-wizard'

type ProviderStatus = 'active' | 'configured'

type ProviderSummary = {
  id: string
  name: string
  description: string
  modelCount: number
  status: ProviderStatus
}

type ProvidersScreenProps = {
  embedded?: boolean
}

function readProviderId(entry: GatewayModelCatalogEntry): string | null {
  if (typeof entry === 'string') return null
  const provider = typeof entry.provider === 'string' ? entry.provider : ''
  const normalized = normalizeProviderId(provider)
  return normalized || null
}

function buildProviderSummaries(payload: {
  models?: Array<GatewayModelCatalogEntry>
  configuredProviders?: Array<string>
}): Array<ProviderSummary> {
  const modelCounts = new Map<string, number>()

  for (const entry of payload.models ?? []) {
    const providerId = readProviderId(entry)
    if (!providerId) continue

    const current = modelCounts.get(providerId) ?? 0
    modelCounts.set(providerId, current + 1)
  }

  const configuredSet = new Set<string>()
  for (const providerId of payload.configuredProviders ?? []) {
    const normalized = normalizeProviderId(providerId)
    if (normalized) configuredSet.add(normalized)
  }

  for (const providerId of modelCounts.keys()) {
    configuredSet.add(providerId)
  }

  const summaries: Array<ProviderSummary> = []

  for (const providerId of configuredSet) {
    const metadata = getProviderInfo(providerId)
    const modelCount = modelCounts.get(providerId) ?? 0

    summaries.push({
      id: providerId,
      name: getProviderDisplayName(providerId),
      description:
        metadata?.description ||
        'Configured provider in your local OpenClaw setup.',
      modelCount,
      status: modelCount > 0 ? 'active' : 'configured',
    })
  }

  summaries.sort(function sortByName(a, b) {
    return a.name.localeCompare(b.name)
  })

  return summaries
}

function ProviderStatusBadge({ status }: { status: ProviderStatus }) {
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-xs font-medium',
        status === 'active' &&
          'border-green-500/35 bg-green-500/10 text-green-600',
        status === 'configured' &&
          'border-primary-300 bg-primary-100 text-primary-700',
      )}
    >
      <HugeiconsIcon icon={CheckmarkCircle02Icon} size={20} strokeWidth={1.5} />
      {status === 'active' ? 'Active' : 'Configured'}
    </span>
  )
}

export function ProvidersScreen({ embedded = false }: ProvidersScreenProps) {
  const [wizardOpen, setWizardOpen] = useState(false)

  const modelsQuery = useQuery({
    queryKey: ['gateway', 'providers', 'models'],
    queryFn: fetchModels,
    refetchInterval: 60_000,
    retry: false,
  })

  const providerSummaries = useMemo(
    function resolveProviderSummaries() {
      return buildProviderSummaries({
        models: Array.isArray(modelsQuery.data?.models)
          ? modelsQuery.data.models
          : [],
        configuredProviders: Array.isArray(
          modelsQuery.data?.configuredProviders,
        )
          ? modelsQuery.data.configuredProviders
          : [],
      })
    },
    [modelsQuery.data?.configuredProviders, modelsQuery.data?.models],
  )

  return (
    <div
      className={cn(
        'h-full text-primary-900',
        embedded ? 'bg-primary-50' : 'overflow-y-auto bg-surface',
      )}
    >
      <main
        className={cn(
          'mx-auto flex w-full max-w-5xl flex-col gap-4 px-4 pt-6 sm:px-6 lg:pt-8',
          embedded ? 'pb-6 md:pb-6 lg:pt-6' : 'pb-24 md:pb-8',
        )}
      >
        <header className="rounded-2xl border border-primary-200 bg-primary-50/85 p-4 shadow-sm backdrop-blur-xl md:p-5">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="space-y-1.5">
              <h1 className="text-xl font-medium text-primary-900 text-balance md:text-2xl">
                Provider Setup
              </h1>
              <p className="text-sm text-primary-600 text-pretty">
                View configured providers and walk through safe setup
                instructions for new providers.
              </p>
            </div>
            <Button
              size="sm"
              onClick={function onOpenWizard() {
                setWizardOpen(true)
              }}
            >
              <HugeiconsIcon icon={Add01Icon} size={20} strokeWidth={1.5} />
              Add Provider
            </Button>
          </div>

          <div className="mt-3 rounded-xl border border-primary-200 bg-primary-100/70 px-3 py-2">
            <p className="text-xs text-primary-700 text-pretty">
              API keys are stored locally in your OpenClaw config file, never
              sent to Studio.
            </p>
          </div>
        </header>

        <section className="rounded-2xl border border-primary-200 bg-primary-50/80 p-4 shadow-sm backdrop-blur-xl md:p-5">
          <div className="mb-4 flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-base font-medium text-primary-900 text-balance">
              Configured Providers
            </h2>
            <p className="text-xs text-primary-600 tabular-nums">
              {providerSummaries.length} provider
              {providerSummaries.length === 1 ? '' : 's'}
            </p>
          </div>

          {modelsQuery.isPending ? (
            <p className="rounded-xl border border-primary-200 bg-primary-100/60 px-3 py-2 text-sm text-primary-600 text-pretty">
              Loading providers from Gateway...
            </p>
          ) : null}

          {modelsQuery.error ? (
            <div className="rounded-xl border border-red-200 bg-red-50/60 px-4 py-3">
              <p className="mb-2 text-sm text-red-700 text-pretty">
                Unable to load providers right now. Check your gateway
                connection.
              </p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => modelsQuery.refetch()}
                className="border-red-300 text-red-700 hover:bg-red-100"
              >
                Retry
              </Button>
            </div>
          ) : null}

          {!modelsQuery.isPending &&
          !modelsQuery.error &&
          providerSummaries.length === 0 ? (
            <div className="rounded-xl border border-primary-200 bg-primary-100/60 px-4 py-4">
              <p className="text-sm text-primary-700 text-pretty">
                No providers are configured yet. Use Add Provider to open setup
                instructions.
              </p>
            </div>
          ) : null}

          {providerSummaries.length > 0 ? (
            <div className="grid gap-3 md:grid-cols-2">
              {providerSummaries.map(function mapProvider(provider) {
                return (
                  <article
                    key={provider.id}
                    className="rounded-2xl border border-primary-200 bg-primary-50/70 p-3"
                  >
                    <div className="flex items-start justify-between gap-3">
                      <div className="flex min-w-0 items-start gap-2.5">
                        <span className="inline-flex size-9 items-center justify-center rounded-xl border border-primary-200 bg-primary-100/70">
                          <ProviderIcon providerId={provider.id} />
                        </span>
                        <div className="min-w-0">
                          <h3 className="truncate text-sm font-medium text-primary-900 text-balance">
                            {provider.name}
                          </h3>
                          <p className="mt-0.5 text-xs text-primary-600 text-pretty line-clamp-2">
                            {provider.description}
                          </p>
                        </div>
                      </div>
                      <ProviderStatusBadge status={provider.status} />
                    </div>

                    <div className="mt-3 flex items-center justify-between rounded-xl border border-primary-200 bg-primary-100/60 px-2.5 py-2">
                      <span className="text-xs text-primary-600">
                        Available models
                      </span>
                      <span className="text-sm font-medium text-primary-900 tabular-nums">
                        {provider.modelCount}
                      </span>
                    </div>
                  </article>
                )
              })}
            </div>
          ) : null}
        </section>
      </main>

      <ProviderWizard open={wizardOpen} onOpenChange={setWizardOpen} />
    </div>
  )
}
