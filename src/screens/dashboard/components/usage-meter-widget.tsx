import { ChartLineData02Icon } from '@hugeicons/core-free-icons'
import { useQuery } from '@tanstack/react-query'
import { useMemo, useState } from 'react'
import { WidgetShell } from './widget-shell'
import { cn } from '@/lib/utils'

type ProviderUsage = {
  provider: string
  total: number
  inputOutput: number
  cached: number
  cost: number
  directCost: number
  percentUsed?: number
}

export type UsageMeterData = {
  usagePercent?: number
  usageLimit?: number
  totalCost: number
  totalDirectCost: number
  totalUsage: number
  totalInputOutput: number
  totalCached: number
  providers: Array<ProviderUsage>
}

type UsageApiResponse = {
  ok?: boolean
  usage?: unknown
  unavailable?: boolean
  error?: unknown
}

export type UsageQueryResult =
  | { kind: 'ok'; data: UsageMeterData }
  | { kind: 'unavailable'; message: string }
  | { kind: 'error'; message: string }

function readNumber(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string') {
    const parsed = Number(value)
    if (Number.isFinite(parsed)) return parsed
  }
  return 0
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function toRecord(value: unknown): Record<string, unknown> {
  if (value && typeof value === 'object' && !Array.isArray(value)) {
    return value as Record<string, unknown>
  }
  return {}
}

function parseProviderUsage(provider: string, value: unknown): ProviderUsage {
  const source = toRecord(value)
  const input = readNumber(source.input)
  const output = readNumber(source.output)
  const cacheRead = readNumber(source.cacheRead)
  const cacheWrite = readNumber(source.cacheWrite)
  const inputCost = readNumber(source.inputCost)
  const outputCost = readNumber(source.outputCost)

  return {
    provider,
    total: readNumber(source.total),
    inputOutput: input + output,
    cached: cacheRead + cacheWrite,
    cost: readNumber(source.cost),
    directCost: inputCost + outputCost,
    percentUsed: readNumber(source.percentUsed) || undefined,
  }
}

function parseUsagePayload(payload: unknown): UsageMeterData {
  const root = toRecord(payload)
  const totalSource = toRecord(root.total)
  const byProviderSource = toRecord(root.byProvider)

  const providers = Object.entries(byProviderSource)
    .map(function mapProvider([provider, value]) {
      return parseProviderUsage(provider, value)
    })
    .sort(function sortProvidersByUsage(left, right) {
      return right.total - left.total
    })

  const totalUsageRaw = readNumber(totalSource.total)
  const totalUsage =
    totalUsageRaw > 0
      ? totalUsageRaw
      : providers.reduce(function sumUsage(total, provider) {
          return total + provider.total
        }, 0)

  const totalInputOutput = providers.reduce(function sumIO(total, provider) {
    return total + provider.inputOutput
  }, 0)

  const totalCached = providers.reduce(function sumCached(total, provider) {
    return total + provider.cached
  }, 0)

  const totalCostRaw = readNumber(totalSource.cost)
  const totalCost =
    totalCostRaw > 0
      ? totalCostRaw
      : providers.reduce(function sumCost(total, provider) {
          return total + provider.cost
        }, 0)

  const totalDirectCost = providers.reduce(function sumDirectCost(
    total,
    provider,
  ) {
    return total + provider.directCost
  }, 0)

  const totalPercent = readNumber(totalSource.percentUsed)
  const maxProviderPercent = providers.reduce(function readMaxPercent(
    currentMax,
    provider,
  ) {
    if (provider.percentUsed === undefined) return currentMax
    return provider.percentUsed > currentMax ? provider.percentUsed : currentMax
  }, 0)
  const usagePercent =
    totalPercent > 0
      ? totalPercent
      : maxProviderPercent > 0
        ? maxProviderPercent
        : undefined

  const usageLimitRaw =
    readNumber(totalSource.limit) ||
    readNumber(totalSource.max) ||
    readNumber(totalSource.quota) ||
    readNumber(totalSource.tokenLimit)
  const usageLimit = usageLimitRaw > 0 ? usageLimitRaw : undefined

  return {
    usagePercent,
    usageLimit,
    totalCost,
    totalDirectCost,
    totalUsage,
    totalInputOutput,
    totalCached,
    providers,
  }
}

function parseErrorMessage(payload: UsageApiResponse): string {
  const message = readString(payload.error)
  return message.length > 0 ? message : 'Usage unavailable'
}

export async function fetchUsage(): Promise<UsageQueryResult> {
  try {
    const response = await fetch('/api/usage')
    const payload = (await response
      .json()
      .catch(() => ({}))) as UsageApiResponse

    if (response.status === 501 || payload.unavailable) {
      return {
        kind: 'unavailable',
        message: 'Unavailable on this Gateway version',
      }
    }

    if (!response.ok || payload.ok === false) {
      return {
        kind: 'error',
        message: parseErrorMessage(payload),
      }
    }

    return {
      kind: 'ok',
      data: parseUsagePayload(payload.usage),
    }
  } catch (error) {
    return {
      kind: 'error',
      message: error instanceof Error ? error.message : 'Usage unavailable',
    }
  }
}

function formatTokens(tokens: number): string {
  return new Intl.NumberFormat().format(Math.max(0, Math.round(tokens)))
}

function formatUsd(amount: number): string {
  return new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(amount)
}

function clampPercent(value: number): number {
  return Math.max(0, Math.min(100, Math.round(value)))
}

type UsageProgress = {
  percent: number | null
  current: number
  max: number | null
}

function resolveUsageProgress(data: UsageMeterData | null): UsageProgress {
  if (!data) {
    return {
      percent: null,
      current: 0,
      max: null,
    }
  }

  const current = Math.max(0, data.totalUsage)
  const explicitMax = data.usageLimit && data.usageLimit > 0 ? data.usageLimit : null

  if (typeof data.usagePercent === 'number' && Number.isFinite(data.usagePercent)) {
    const percent = clampPercent(data.usagePercent)
    const inferredMax =
      percent > 0 ? Math.max(current, Math.round(current / (percent / 100))) : null

    return {
      percent,
      current,
      max: explicitMax ?? inferredMax,
    }
  }

  if (explicitMax && explicitMax > 0) {
    return {
      percent: clampPercent((current / explicitMax) * 100),
      current,
      max: explicitMax,
    }
  }

  return {
    percent: null,
    current,
    max: null,
  }
}

type UsageMeterWidgetProps = {
  draggable?: boolean
  onRemove?: () => void
  editMode?: boolean
}

export function UsageMeterWidget({
  draggable: _draggable = false,
  onRemove,
  editMode,
}: UsageMeterWidgetProps) {
  const [view, setView] = useState<'tokens' | 'cost'>('tokens')
  const usageQuery = useQuery({
    queryKey: ['dashboard', 'usage'],
    queryFn: fetchUsage,
    retry: false,
    refetchInterval: 30_000,
  })

  const queryResult = usageQuery.data
  const usageData = queryResult?.kind === 'ok' ? queryResult.data : null

  const usageProgress = useMemo(
    function readUsageProgress() {
      return resolveUsageProgress(usageData)
    },
    [usageData],
  )

  const tokenSubtitle =
    usageProgress.max !== null
      ? `${formatTokens(usageProgress.current)} / ${formatTokens(usageProgress.max)} tokens`
      : `${formatTokens(usageProgress.current)} tokens`

  return (
    <WidgetShell
      size="medium"
      title="Usage Meter"
      icon={ChartLineData02Icon}
      action={
        <div className="hidden items-center gap-0.5 rounded-full border border-primary-200 bg-primary-100/70 p-0.5 text-[10px] md:inline-flex">
          {(['tokens', 'cost'] as const).map((tab) => (
            <button
              key={tab}
              type="button"
              onClick={(event) => {
                event.stopPropagation()
                setView(tab)
              }}
              className={cn(
                'rounded-full px-2 py-0.5 font-medium transition-colors',
                view === tab
                  ? 'bg-accent-100 text-accent-700 shadow-sm'
                  : 'text-primary-500 hover:text-primary-700',
              )}
            >
              {tab === 'tokens' ? 'Tokens' : 'Cost'}
            </button>
          ))}
        </div>
      }
      onRemove={onRemove}
      editMode={editMode}
      className="h-full"
    >
      {queryResult?.kind === 'unavailable' ? (
        <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
          {queryResult.message}
        </div>
      ) : queryResult?.kind === 'error' ? (
        <div className="rounded-lg border border-red-200 bg-red-50/80 px-3 py-3 text-sm text-red-700">
          {queryResult.message}
        </div>
      ) : !usageData ? (
        <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
          Loading usage data…
        </div>
      ) : (
        <>
          <div className="space-y-3 md:hidden">
            <p className="text-[11px] font-medium tracking-wide text-neutral-500 dark:text-neutral-400">
              Usage
            </p>
            <p className="text-3xl font-semibold leading-none text-neutral-900 dark:text-neutral-50">
              {usageProgress.percent === null ? '—' : `${usageProgress.percent}%`}
            </p>

            <div className="h-2 w-full overflow-hidden rounded-full bg-neutral-200/60 dark:bg-neutral-800/60">
              {usageProgress.percent === null ? (
                <div className="h-2 w-full rounded-full bg-[repeating-linear-gradient(45deg,rgba(16,185,129,0.2),rgba(16,185,129,0.2)_8px,rgba(16,185,129,0.4)_8px,rgba(16,185,129,0.4)_16px)]" />
              ) : (
                <div
                  className="h-2 rounded-full bg-emerald-500/60 transition-[width] duration-500"
                  style={{ width: `${usageProgress.percent}%` }}
                />
              )}
            </div>

            <div className="inline-flex rounded-full bg-neutral-100/70 p-1 dark:bg-neutral-800/50 gap-1">
              {(['tokens', 'cost'] as const).map((tab) => (
                <button
                  key={tab}
                  type="button"
                  onClick={(event) => {
                    event.stopPropagation()
                    setView(tab)
                  }}
                  className={cn(
                    'px-3 py-1 text-xs rounded-full transition-colors',
                    view === tab
                      ? 'bg-white shadow-sm dark:bg-neutral-900'
                      : 'text-neutral-600 dark:text-neutral-300',
                  )}
                >
                  {tab === 'tokens' ? 'Tokens' : 'Cost'}
                </button>
              ))}
            </div>

            {view === 'cost' ? (
              <p className="text-xs text-neutral-600 dark:text-neutral-300">
                {formatUsd(usageData.totalCost)} total • {formatUsd(usageData.totalDirectCost)} direct
              </p>
            ) : (
              <div className="space-y-1">
                <p className="text-xs text-neutral-600 dark:text-neutral-300">{tokenSubtitle}</p>
                <p className="text-xs text-neutral-500 dark:text-neutral-400">
                  In/Out {formatTokens(usageData.totalInputOutput)} • Cached{' '}
                  {formatTokens(usageData.totalCached)}
                </p>
              </div>
            )}
          </div>

          <div className="hidden space-y-2.5 md:block">
            <div>
              <p className="font-mono text-2xl font-bold leading-none text-ink tabular-nums">
                {usageProgress.percent ?? 0}%
              </p>
              <p className="mt-1 text-xs text-primary-500">Usage</p>
            </div>

            <div className="h-1.5 w-full overflow-hidden rounded-full bg-gray-200/80 dark:bg-gray-700/70">
              {usageProgress.percent === null ? (
                <div className="h-1.5 w-full rounded-full bg-[repeating-linear-gradient(45deg,rgba(251,146,60,0.2),rgba(251,146,60,0.2)_8px,rgba(251,146,60,0.4)_8px,rgba(251,146,60,0.4)_16px)]" />
              ) : (
                <div
                  className="h-1.5 rounded-full bg-orange-400 transition-[width] duration-500"
                  style={{ width: `${usageProgress.percent}%` }}
                />
              )}
            </div>

            <p className="text-xs text-primary-500">{tokenSubtitle}</p>

            {view === 'cost' ? (
              <p className="text-xs text-primary-600">
                {formatUsd(usageData.totalDirectCost)} direct • {formatUsd(usageData.totalCost)} total
              </p>
            ) : (
              <p className="text-xs text-primary-600">
                In/Out {formatTokens(usageData.totalInputOutput)} • Cached{' '}
                {formatTokens(usageData.totalCached)}
              </p>
            )}
          </div>
        </>
      )}
    </WidgetShell>
  )
}
