import { useQuery } from '@tanstack/react-query'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  AlertDiamondIcon,
  ArrowTurnBackwardIcon,
} from '@hugeicons/core-free-icons'

type Totals = {
  totalCost?: number
  totalTokens?: number
  inputCost?: number
  outputCost?: number
  cacheReadCost?: number
  cacheWriteCost?: number
  input?: number
  output?: number
  cacheRead?: number
  cacheWrite?: number
}

type UsageData = {
  cost?: { totals?: Totals; days?: number; updatedAt?: number }
  usage?: {
    totals?: Totals
    startDate?: string
    endDate?: string
    updatedAt?: number
  }
}

function formatCost(n?: number) {
  if (typeof n !== 'number') return '—'
  return `$${n.toFixed(2)}`
}

function formatTokens(n?: number) {
  if (typeof n !== 'number') return '—'
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return String(n)
}

function StatCard({
  label,
  value,
  sub,
}: {
  label: string
  value: string
  sub?: string
}) {
  return (
    <div className="rounded-lg border border-primary-200 p-4">
      <div className="text-[11px] font-medium text-primary-500 uppercase tracking-wider">
        {label}
      </div>
      <div className="text-xl font-semibold text-ink mt-1">{value}</div>
      {sub ? (
        <div className="text-[11px] text-primary-500 mt-0.5">{sub}</div>
      ) : null}
    </div>
  )
}

export function UsageScreen() {
  const query = useQuery({
    queryKey: ['gateway', 'usage-gateway'],
    queryFn: async () => {
      const res = await fetch('/api/gateway/usage')
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const json = await res.json()
      if (!json.ok) throw new Error(json.error || 'Gateway error')
      return json.data as UsageData
    },
    refetchInterval: 15_000,
    retry: 1,
  })

  const lastUpdated = query.dataUpdatedAt
    ? new Date(query.dataUpdatedAt).toLocaleTimeString()
    : null
  const cost = query.data?.cost?.totals
  const usage = query.data?.usage?.totals
  const period = query.data?.usage
    ? `${query.data.usage.startDate || '?'} — ${query.data.usage.endDate || '?'}`
    : null

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-6 py-4 border-b border-primary-200">
        <div className="flex items-center gap-3">
          <h1 className="text-[15px] font-semibold text-ink">Usage</h1>
          {query.isFetching && !query.isLoading ? (
            <span className="text-[10px] text-primary-500 animate-pulse">
              syncing…
            </span>
          ) : null}
        </div>
        <div className="flex items-center gap-3">
          {lastUpdated ? (
            <span className="text-[10px] text-primary-500">
              Updated {lastUpdated}
            </span>
          ) : null}
          <span
            className={`inline-block size-2 rounded-full ${query.isError ? 'bg-red-500' : query.isSuccess ? 'bg-emerald-500' : 'bg-amber-500'}`}
          />
        </div>
      </div>

      <div className="flex-1 overflow-auto px-6 py-4">
        {query.isLoading ? (
          <div className="flex items-center justify-center h-32">
            <div className="flex items-center gap-2 text-primary-500">
              <div className="size-4 border-2 border-primary-300 border-t-primary-600 rounded-full animate-spin" />
              <span className="text-sm">Connecting to gateway…</span>
            </div>
          </div>
        ) : query.isError ? (
          <div className="flex flex-col items-center justify-center h-32 gap-3">
            <HugeiconsIcon
              icon={AlertDiamondIcon}
              size={24}
              strokeWidth={1.5}
              className="text-red-500"
            />
            <p className="text-sm text-primary-600">
              {query.error instanceof Error
                ? query.error.message
                : 'Failed to fetch'}
            </p>
            <button
              type="button"
              onClick={() => query.refetch()}
              className="inline-flex items-center gap-1.5 rounded-md border border-primary-200 px-3 py-1.5 text-xs font-medium text-primary-700 hover:bg-primary-100"
            >
              <HugeiconsIcon
                icon={ArrowTurnBackwardIcon}
                size={14}
                strokeWidth={1.5}
              />
              Retry
            </button>
          </div>
        ) : (
          <>
            {period ? (
              <p className="text-[11px] text-primary-500 mb-4">{period}</p>
            ) : null}

            {/* Summary cards */}
            <div className="grid grid-cols-2 lg:grid-cols-4 gap-3 mb-6">
              <StatCard
                label="Total Cost"
                value={formatCost(usage?.totalCost)}
                sub={
                  cost ? `Session: ${formatCost(cost.totalCost)}` : undefined
                }
              />
              <StatCard
                label="Total Tokens"
                value={formatTokens(usage?.totalTokens)}
              />
              <StatCard
                label="Input Cost"
                value={formatCost(usage?.inputCost)}
              />
              <StatCard
                label="Output Cost"
                value={formatCost(usage?.outputCost)}
              />
            </div>

            {/* Breakdown table */}
            <h2 className="text-[13px] font-semibold text-ink mb-3">
              Cost Breakdown
            </h2>
            <table className="w-full text-[13px]">
              <thead>
                <tr className="border-b border-primary-200 text-left">
                  <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                    Category
                  </th>
                  <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider text-right">
                    Tokens
                  </th>
                  <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider text-right">
                    Cost
                  </th>
                </tr>
              </thead>
              <tbody>
                {[
                  {
                    label: 'Input',
                    tokens: usage?.input,
                    cost: usage?.inputCost,
                  },
                  {
                    label: 'Output',
                    tokens: usage?.output,
                    cost: usage?.outputCost,
                  },
                  {
                    label: 'Cache Read',
                    tokens: usage?.cacheRead,
                    cost: usage?.cacheReadCost,
                  },
                  {
                    label: 'Cache Write',
                    tokens: usage?.cacheWrite,
                    cost: usage?.cacheWriteCost,
                  },
                ].map((row) => (
                  <tr
                    key={row.label}
                    className="border-b border-primary-100 hover:bg-primary-50"
                  >
                    <td className="py-2.5 text-ink">{row.label}</td>
                    <td className="py-2.5 text-primary-600 text-right tabular-nums">
                      {formatTokens(row.tokens)}
                    </td>
                    <td className="py-2.5 text-primary-600 text-right tabular-nums">
                      {formatCost(row.cost)}
                    </td>
                  </tr>
                ))}
                <tr className="font-medium">
                  <td className="py-2.5 text-ink">Total</td>
                  <td className="py-2.5 text-ink text-right tabular-nums">
                    {formatTokens(usage?.totalTokens)}
                  </td>
                  <td className="py-2.5 text-ink text-right tabular-nums">
                    {formatCost(usage?.totalCost)}
                  </td>
                </tr>
              </tbody>
            </table>
          </>
        )}
      </div>
    </div>
  )
}
