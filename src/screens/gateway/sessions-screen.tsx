import { useQuery } from '@tanstack/react-query'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  AlertDiamondIcon,
  ArrowTurnBackwardIcon,
} from '@hugeicons/core-free-icons'

type SessionEntry = {
  key: string
  kind?: string
  displayName?: string
  label?: string
  model?: string
  modelProvider?: string
  origin?: { surface?: string; chatType?: string; label?: string }
  updatedAt?: number
  totalTokens?: number
  contextTokens?: number
  status?: string
}

type SessionsData = {
  count?: number
  sessions?: SessionEntry[]
}

function timeAgo(ts?: number) {
  if (!ts) return '—'
  const diff = Date.now() - ts
  const mins = Math.floor(diff / 60000)
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m ago`
  const hrs = Math.floor(mins / 60)
  if (hrs < 24) return `${hrs}h ago`
  const days = Math.floor(hrs / 24)
  return `${days}d ago`
}

function KindBadge({ kind }: { kind?: string }) {
  const colors: Record<string, string> = {
    direct: 'bg-blue-100 text-blue-700',
    cron: 'bg-purple-100 text-purple-700',
    subagent: 'bg-amber-100 text-amber-700',
  }
  return (
    <span
      className={`inline-flex px-1.5 py-0.5 rounded text-[10px] font-medium ${colors[kind || ''] || 'bg-primary-100 text-primary-600'}`}
    >
      {kind || 'unknown'}
    </span>
  )
}

export function SessionsScreen() {
  const query = useQuery({
    queryKey: ['gateway', 'sessions-gateway'],
    queryFn: async () => {
      const res = await fetch('/api/gateway/sessions')
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const json = await res.json()
      if (!json.ok) throw new Error(json.error || 'Gateway error')
      return json.data as SessionsData
    },
    refetchInterval: 10_000,
    retry: 1,
  })

  const lastUpdated = query.dataUpdatedAt
    ? new Date(query.dataUpdatedAt).toLocaleTimeString()
    : null
  const sessions = query.data?.sessions || []

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-6 py-4 border-b border-primary-200">
        <div className="flex items-center gap-3">
          <h1 className="text-[15px] font-semibold text-ink">Sessions</h1>
          <span className="text-[11px] text-primary-500">
            {sessions.length} active
          </span>
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
        ) : sessions.length === 0 ? (
          <p className="text-sm text-primary-500 text-center py-8">
            No active sessions.
          </p>
        ) : (
          <table className="w-full text-[13px]">
            <thead>
              <tr className="border-b border-primary-200 text-left">
                <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                  Session
                </th>
                <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                  Kind
                </th>
                <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                  Model
                </th>
                <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                  Origin
                </th>
                <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider text-right">
                  Updated
                </th>
              </tr>
            </thead>
            <tbody>
              {sessions.map((s) => (
                <tr
                  key={s.key}
                  className="border-b border-primary-100 hover:bg-primary-50 transition-colors"
                >
                  <td className="py-3">
                    <div className="font-medium text-ink truncate max-w-[280px]">
                      {s.label || s.displayName || s.key}
                    </div>
                    <div className="text-[11px] text-primary-500 truncate max-w-[280px]">
                      {s.key}
                    </div>
                  </td>
                  <td className="py-3">
                    <KindBadge kind={s.kind} />
                  </td>
                  <td className="py-3 text-primary-700">{s.model || '—'}</td>
                  <td className="py-3 text-primary-600">
                    {s.origin?.surface || '—'}
                  </td>
                  <td className="py-3 text-primary-600 text-right">
                    {timeAgo(s.updatedAt)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}
