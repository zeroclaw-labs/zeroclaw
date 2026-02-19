type UsageTotals = {
  input: number
  output: number
  cacheRead: number
  cacheWrite: number
  total: number
  cost: number
  inputCost: number
  outputCost: number
  cacheReadCost: number
  cacheWriteCost: number
  missingCostEntries: number
}

export type UsageProviderSummary = UsageTotals & {
  provider: string
  count: number
  status: 'ok' | 'error'
  message?: string
  percentUsed?: number
}

export type UsageSummary = {
  updatedAt: number
  total: UsageTotals & { percentUsed?: number }
  byProvider: Record<string, UsageProviderSummary>
}

type CostTotals = {
  amount: number
  inputCost: number
  outputCost: number
  cacheReadCost: number
  cacheWriteCost: number
  missingCostEntries: number
}

export type CostPoint = {
  date: string
  amount: number
  input: number
  output: number
  cacheRead: number
  cacheWrite: number
  inputCost: number
  outputCost: number
  cacheReadCost: number
  cacheWriteCost: number
}

export type CostSummary = {
  updatedAt: number
  total: CostTotals
  timeseries?: Array<CostPoint>
}

type UsageSummaryBuildOptions = {
  configuredProviders: Array<string>
  sessionsUsagePayload: unknown
  usageStatusPayload?: unknown
}

type UnknownRecord = Record<string, unknown>

const GATEWAY_UNAVAILABLE_PATTERN =
  /(unknown method|method not found|unsupported method|not implemented|unsupported gateway)/i

export const SENSITIVE_PATTERN = /(token|secret|password|apiKey|refresh)/i

function toRecord(value: unknown): UnknownRecord {
  if (value && typeof value === 'object') {
    return value as UnknownRecord
  }
  return {}
}

function readNumber(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string') {
    const parsed = Number(value)
    if (Number.isFinite(parsed)) return parsed
  }
  return 0
}

function readString(value: unknown): string {
  if (typeof value !== 'string') return ''
  return value.trim()
}

function normalizeProviderName(value: unknown): string {
  return readString(value).toLowerCase()
}

function emptyUsageTotals(): UsageTotals {
  return {
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    total: 0,
    cost: 0,
    inputCost: 0,
    outputCost: 0,
    cacheReadCost: 0,
    cacheWriteCost: 0,
    missingCostEntries: 0,
  }
}

function readUsageTotals(value: unknown): UsageTotals {
  const source = toRecord(value)
  return {
    input: readNumber(source.input),
    output: readNumber(source.output),
    cacheRead: readNumber(source.cacheRead),
    cacheWrite: readNumber(source.cacheWrite),
    total: readNumber(source.totalTokens ?? source.total),
    cost: readNumber(source.totalCost ?? source.cost),
    inputCost: readNumber(source.inputCost),
    outputCost: readNumber(source.outputCost),
    cacheReadCost: readNumber(source.cacheReadCost),
    cacheWriteCost: readNumber(source.cacheWriteCost),
    missingCostEntries: readNumber(source.missingCostEntries),
  }
}

function mergeUsageTotals(target: UsageTotals, source: UsageTotals) {
  target.input += source.input
  target.output += source.output
  target.cacheRead += source.cacheRead
  target.cacheWrite += source.cacheWrite
  target.total += source.total
  target.cost += source.cost
  target.inputCost += source.inputCost
  target.outputCost += source.outputCost
  target.cacheReadCost += source.cacheReadCost
  target.cacheWriteCost += source.cacheWriteCost
  target.missingCostEntries += source.missingCostEntries
}

function createProviderSummary(provider: string): UsageProviderSummary {
  return {
    provider,
    ...emptyUsageTotals(),
    count: 0,
    status: 'ok',
  }
}

function buildProviderTotalsFromSessionsUsage(
  sessionsUsagePayload: unknown,
  configuredProviders: Array<string>,
): Record<string, UsageProviderSummary> {
  const byProvider: Record<string, UsageProviderSummary> = {}
  const providerSet = new Set<string>(
    configuredProviders.map(function mapProvider(provider) {
      return normalizeProviderName(provider)
    }),
  )

  for (const provider of providerSet.values()) {
    byProvider[provider] = createProviderSummary(provider)
  }

  const root = toRecord(sessionsUsagePayload)
  const aggregates = toRecord(root.aggregates)
  const byProviderEntries = Array.isArray(aggregates.byProvider)
    ? aggregates.byProvider
    : []

  for (const entry of byProviderEntries) {
    const row = toRecord(entry)
    const provider = normalizeProviderName(
      row.provider ?? row.name ?? row.id ?? '',
    )
    if (!provider || !providerSet.has(provider)) continue

    const totals = readUsageTotals(row.totals ?? row)
    const target = byProvider[provider] ?? createProviderSummary(provider)
    mergeUsageTotals(target, totals)
    target.count += readNumber(row.count)
    byProvider[provider] = target
  }

  return byProvider
}

function applyProviderStatus(
  byProvider: Record<string, UsageProviderSummary>,
  usageStatusPayload: unknown,
) {
  const root = toRecord(usageStatusPayload)
  const entries = Array.isArray(root.providers) ? root.providers : []

  for (const entry of entries) {
    const row = toRecord(entry)
    const provider = normalizeProviderName(row.provider)
    if (!provider || !byProvider[provider]) continue

    const providerSummary = byProvider[provider]
    const message = readString(row.error ?? row.message)
    if (message) {
      providerSummary.status = 'error'
      providerSummary.message = message
    }

    const windows = Array.isArray(row.windows) ? row.windows : []
    const usedPercents: Array<number> = []
    for (const window of windows) {
      const percent = readNumber(toRecord(window).usedPercent)
      if (percent > 0) usedPercents.push(percent)
    }

    if (usedPercents.length > 0) {
      providerSummary.percentUsed = Math.max(...usedPercents)
    }
  }
}

function readUpdatedAt(value: unknown): number {
  const parsed = readNumber(value)
  return parsed > 0 ? parsed : Date.now()
}

export function isGatewayMethodUnavailable(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error)
  return GATEWAY_UNAVAILABLE_PATTERN.test(message)
}

export function buildUsageSummary(
  options: UsageSummaryBuildOptions,
): UsageSummary {
  const byProvider = buildProviderTotalsFromSessionsUsage(
    options.sessionsUsagePayload,
    options.configuredProviders,
  )

  if (options.usageStatusPayload) {
    applyProviderStatus(byProvider, options.usageStatusPayload)
  }

  const root = toRecord(options.sessionsUsagePayload)
  const totals = readUsageTotals(root.totals)

  const percentCandidates = Object.values(byProvider)
    .map(function mapPercent(provider) {
      return provider.percentUsed
    })
    .filter(function hasPercent(percent): percent is NonNullable<
      UsageProviderSummary['percentUsed']
    > {
      return typeof percent === 'number' && Number.isFinite(percent)
    })

  return {
    updatedAt: readUpdatedAt(root.updatedAt),
    total: {
      ...totals,
      percentUsed:
        percentCandidates.length > 0
          ? Math.max(...percentCandidates)
          : undefined,
    },
    byProvider,
  }
}

function readCostTotals(value: unknown): CostTotals {
  const source = toRecord(value)
  return {
    amount: readNumber(source.totalCost ?? source.amount),
    inputCost: readNumber(source.inputCost),
    outputCost: readNumber(source.outputCost),
    cacheReadCost: readNumber(source.cacheReadCost),
    cacheWriteCost: readNumber(source.cacheWriteCost),
    missingCostEntries: readNumber(source.missingCostEntries),
  }
}

function buildCostPoint(entry: unknown): CostPoint {
  const row = toRecord(entry)
  return {
    date: readString(row.date),
    amount: readNumber(row.totalCost ?? row.amount),
    input: readNumber(row.input),
    output: readNumber(row.output),
    cacheRead: readNumber(row.cacheRead),
    cacheWrite: readNumber(row.cacheWrite),
    inputCost: readNumber(row.inputCost),
    outputCost: readNumber(row.outputCost),
    cacheReadCost: readNumber(row.cacheReadCost),
    cacheWriteCost: readNumber(row.cacheWriteCost),
  }
}

export function buildCostSummary(payload: unknown): CostSummary {
  const root = toRecord(payload)
  const rawTimeseries = Array.isArray(root.daily) ? root.daily : []
  const timeseries = rawTimeseries
    .map(buildCostPoint)
    .filter(function hasDate(point) {
      return point.date.length > 0
    })

  return {
    updatedAt: readUpdatedAt(root.updatedAt),
    total: readCostTotals(root.totals),
    timeseries: timeseries.length > 0 ? timeseries : undefined,
  }
}
