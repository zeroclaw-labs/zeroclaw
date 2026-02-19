import { useQuery } from '@tanstack/react-query'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  AlertDiamondIcon,
  ArrowTurnBackwardIcon,
} from '@hugeicons/core-free-icons'

type GatewayApiResponse = {
  ok: boolean
  data?: unknown
  error?: string
}

async function fetchGatewayEndpoint(
  endpoint: string,
): Promise<GatewayApiResponse> {
  const res = await fetch(endpoint)
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json() as Promise<GatewayApiResponse>
}

function formatValue(value: unknown, depth = 0): React.ReactNode {
  if (value === null || value === undefined) {
    return <span className="text-primary-500 italic">null</span>
  }
  if (typeof value === 'boolean') {
    return (
      <span className={value ? 'text-emerald-600' : 'text-red-500'}>
        {String(value)}
      </span>
    )
  }
  if (typeof value === 'number') {
    return <span className="text-accent-600">{value}</span>
  }
  if (typeof value === 'string') {
    if (value.length > 200)
      return <span className="text-ink break-all">{value.slice(0, 200)}…</span>
    return <span className="text-ink">{value}</span>
  }
  if (Array.isArray(value)) {
    if (value.length === 0)
      return <span className="text-primary-500 italic">[]</span>
    return (
      <div className={depth > 0 ? 'ml-4 border-l border-primary-200 pl-3' : ''}>
        {value.map((item, i) => (
          <div key={i} className="py-0.5">
            <span className="text-primary-500 text-[10px] mr-1">[{i}]</span>
            {formatValue(item, depth + 1)}
          </div>
        ))}
      </div>
    )
  }
  if (typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>)
    if (entries.length === 0)
      return <span className="text-primary-500 italic">{'{}'}</span>
    return (
      <div className={depth > 0 ? 'ml-4 border-l border-primary-200 pl-3' : ''}>
        {entries.map(([key, val]) => (
          <div key={key} className="py-0.5 flex gap-2">
            <span className="text-primary-600 font-medium text-xs shrink-0">
              {key}:
            </span>
            <div className="min-w-0">{formatValue(val, depth + 1)}</div>
          </div>
        ))}
      </div>
    )
  }
  return <span>{String(value)}</span>
}

export function GatewayDataScreen({
  title,
  endpoint,
  queryKey,
  pollInterval = 10_000,
}: {
  title: string
  endpoint: string
  queryKey: string
  pollInterval?: number
}) {
  const query = useQuery({
    queryKey: ['gateway', queryKey],
    queryFn: () => fetchGatewayEndpoint(endpoint),
    refetchInterval: pollInterval,
    retry: 1,
  })

  const lastUpdated = query.dataUpdatedAt
    ? new Date(query.dataUpdatedAt).toLocaleTimeString()
    : null

  return (
    <div className="flex flex-col h-full">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-4 border-b border-primary-200">
        <div className="flex items-center gap-3">
          <h1 className="text-[15px] font-semibold text-ink">{title}</h1>
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
            className={`inline-block size-2 rounded-full ${
              query.isError
                ? 'bg-red-500'
                : query.isSuccess
                  ? 'bg-emerald-500'
                  : 'bg-amber-500'
            }`}
            title={
              query.isError
                ? 'Disconnected'
                : query.isSuccess
                  ? 'Connected'
                  : 'Connecting'
            }
          />
        </div>
      </div>

      {/* Content */}
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
              className="inline-flex items-center gap-1.5 rounded-md border border-primary-200 px-3 py-1.5 text-xs font-medium text-primary-700 hover:bg-primary-100 transition-colors"
            >
              <HugeiconsIcon
                icon={ArrowTurnBackwardIcon}
                size={14}
                strokeWidth={1.5}
              />
              Retry
            </button>
          </div>
        ) : query.data?.ok === false ? (
          <div className="flex flex-col items-center justify-center h-32 gap-3">
            <HugeiconsIcon
              icon={AlertDiamondIcon}
              size={24}
              strokeWidth={1.5}
              className="text-amber-500"
            />
            <p className="text-sm text-primary-600">
              {query.data.error || 'Gateway returned an error'}
            </p>
            <button
              type="button"
              onClick={() => query.refetch()}
              className="inline-flex items-center gap-1.5 rounded-md border border-primary-200 px-3 py-1.5 text-xs font-medium text-primary-700 hover:bg-primary-100 transition-colors"
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
          <div className="text-xs font-mono leading-relaxed">
            {formatValue(query.data?.data)}
          </div>
        )}
      </div>
    </div>
  )
}
