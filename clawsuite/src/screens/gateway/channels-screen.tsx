import { useQuery } from '@tanstack/react-query'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  AlertDiamondIcon,
  ArrowTurnBackwardIcon,
  Chat01Icon,
} from '@hugeicons/core-free-icons'
import { EmptyState } from '@/components/empty-state'

type ChannelInfo = {
  configured?: boolean
  running?: boolean
  mode?: string
  lastStartAt?: number | null
  lastStopAt?: number | null
  lastError?: string | null
}

type ChannelsData = {
  channels?: Record<string, ChannelInfo>
  channelLabels?: Record<string, string>
  channelDetailLabels?: Record<string, string>
}

function StatusDot({ running }: { running?: boolean }) {
  return (
    <span
      className={`inline-block size-2 rounded-full ${running ? 'bg-emerald-500' : 'bg-red-500'}`}
    />
  )
}

function formatTime(ts?: number | null) {
  if (!ts) return '—'
  return new Date(ts).toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  })
}

export function ChannelsScreen() {
  const query = useQuery({
    queryKey: ['gateway', 'channels'],
    queryFn: async () => {
      const res = await fetch('/api/gateway/channels')
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const json = await res.json()
      if (!json.ok) throw new Error(json.error || 'Gateway error')
      return json.data as ChannelsData
    },
    refetchInterval: 5_000,
    retry: 1,
  })

  const lastUpdated = query.dataUpdatedAt
    ? new Date(query.dataUpdatedAt).toLocaleTimeString()
    : null

  const channels = query.data?.channels || {}
  const labels = query.data?.channelLabels || {}
  const detailLabels = query.data?.channelDetailLabels || {}
  const channelEntries = Object.entries(channels)

  return (
    <div className="flex h-full min-h-0 flex-col overflow-x-hidden">
      <div className="flex items-center justify-between border-b border-primary-200 px-3 py-2 md:px-6 md:py-4">
        <div className="flex items-center gap-3">
          <h1 className="text-sm font-semibold text-ink md:text-[15px]">
            Channels
          </h1>
          {query.isFetching && !query.isLoading ? (
            <span className="text-[10px] text-primary-500 animate-pulse">
              syncing…
            </span>
          ) : null}
        </div>
        <div className="flex items-center gap-2 md:gap-3">
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

      <div className="flex-1 overflow-auto px-3 pt-3 pb-24 md:px-6 md:pt-4 md:pb-0">
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
        ) : channelEntries.length === 0 ? (
          <EmptyState
            icon={Chat01Icon}
            title="No channels configured"
            description="Connect Telegram, Discord, or other messaging platforms in settings."
          />
        ) : (
          <>
            <div className="space-y-3 md:hidden">
              {channelEntries.map(([key, ch]) => (
                <article
                  key={key}
                  className="rounded-xl border border-primary-200 bg-primary-50/70 p-3"
                >
                  <div className="flex items-center justify-between gap-2">
                    <h2 className="text-sm font-medium text-ink">
                      {labels[key] || key}
                    </h2>
                    <span className="inline-flex items-center gap-1.5 text-xs">
                      <StatusDot running={ch.running} />
                      <span
                        className={
                          ch.running ? 'text-emerald-700' : 'text-red-600'
                        }
                      >
                        {ch.running ? 'Running' : 'Stopped'}
                      </span>
                    </span>
                  </div>
                  <dl className="mt-3 grid grid-cols-2 gap-2 text-xs">
                    <div className="rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                      <dt className="text-primary-500">Mode</dt>
                      <dd className="truncate text-primary-700">
                        {ch.mode || '—'}
                      </dd>
                    </div>
                    <div className="rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                      <dt className="text-primary-500">Type</dt>
                      <dd className="truncate text-primary-700">
                        {detailLabels[key] || '—'}
                      </dd>
                    </div>
                    <div className="col-span-2 rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                      <dt className="text-primary-500">Last started</dt>
                      <dd className="text-primary-700">
                        {formatTime(ch.lastStartAt)}
                      </dd>
                    </div>
                    <div className="col-span-2 rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                      <dt className="text-primary-500">Error</dt>
                      <dd className="text-red-600">{ch.lastError || '—'}</dd>
                    </div>
                  </dl>
                </article>
              ))}
            </div>

            <div className="hidden overflow-x-auto md:block">
              <table className="w-full min-w-[760px] text-[13px]">
                <thead>
                  <tr className="border-b border-primary-200 text-left">
                    <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                      Channel
                    </th>
                    <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                      Status
                    </th>
                    <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                      Mode
                    </th>
                    <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                      Type
                    </th>
                    <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                      Last Started
                    </th>
                    <th className="pb-2 text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                      Error
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {channelEntries.map(([key, ch]) => (
                    <tr
                      key={key}
                      className="border-b border-primary-100 transition-colors hover:bg-primary-50"
                    >
                      <td className="py-3 font-medium text-ink">
                        {labels[key] || key}
                      </td>
                      <td className="py-3">
                        <span className="inline-flex items-center gap-1.5">
                          <StatusDot running={ch.running} />
                          <span
                            className={
                              ch.running ? 'text-emerald-700' : 'text-red-600'
                            }
                          >
                            {ch.running ? 'Running' : 'Stopped'}
                          </span>
                        </span>
                      </td>
                      <td className="py-3 text-primary-600">{ch.mode || '—'}</td>
                      <td className="py-3 text-primary-600">
                        {detailLabels[key] || '—'}
                      </td>
                      <td className="py-3 text-primary-600">
                        {formatTime(ch.lastStartAt)}
                      </td>
                      <td className="py-3 text-xs text-red-600">
                        {ch.lastError || '—'}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
      </div>
    </div>
  )
}
