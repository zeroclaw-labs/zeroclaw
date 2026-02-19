import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { BotIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type { GatewaySession } from '@/lib/gateway-api'
import { usePageTitle } from '@/hooks/use-page-title'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

export const Route = createFileRoute('/instances')({
  component: InstancesRoute,
  errorComponent: function InstancesError({ error }) {
    return (
      <div className="flex flex-col items-center justify-center h-full p-6 text-center bg-primary-50">
        <h2 className="text-xl font-semibold text-primary-900 mb-3">
          Failed to Load Instances
        </h2>
        <p className="text-sm text-primary-600 mb-4 max-w-md">
          {error instanceof Error
            ? error.message
            : 'An unexpected error occurred'}
        </p>
        <button
          onClick={() => window.location.reload()}
          className="px-4 py-2 bg-accent-500 text-white rounded-lg hover:bg-accent-600 transition-colors"
        >
          Reload Page
        </button>
      </div>
    )
  },
  pendingComponent: function InstancesPending() {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="text-center">
          <div className="inline-block h-8 w-8 animate-spin rounded-full border-4 border-accent-500 border-r-transparent mb-3" />
          <p className="text-sm text-primary-500">Loading instances...</p>
        </div>
      </div>
    )
  },
})

type GatewaySessionsResponse = {
  ok?: boolean
  error?: string
  sessions?: Array<GatewaySession>
  data?: {
    sessions?: Array<GatewaySession>
  } | null
}

function readString(value: unknown): string {
  if (typeof value !== 'string') return ''
  return value.trim()
}

function readTimestamp(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value < 1_000_000_000_000 ? value * 1000 : value
  }
  if (typeof value === 'string') {
    const dateValue = new Date(value).getTime()
    if (Number.isFinite(dateValue)) return dateValue
  }
  return null
}

function formatAbsoluteTimestamp(timestamp: number | null): string {
  if (!timestamp) return 'N/A'
  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  }).format(timestamp)
}

function formatRelativeTimestamp(timestamp: number | null): string {
  if (!timestamp) return 'N/A'
  const diffMs = Date.now() - timestamp
  if (diffMs < 60_000) return 'just now'
  const minutes = Math.floor(diffMs / 60_000)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

function formatSessionKey(value: string): string {
  if (value.length <= 20) return value
  return `${value.slice(0, 10)}...${value.slice(-8)}`
}

function compareSessions(a: GatewaySession, b: GatewaySession): number {
  const aUpdated = readTimestamp(a.updatedAt) ?? readTimestamp(a.createdAt) ?? 0
  const bUpdated = readTimestamp(b.updatedAt) ?? readTimestamp(b.createdAt) ?? 0
  return bUpdated - aUpdated
}

function resolveChatSessionParam(session: GatewaySession): string | null {
  const friendlyId = readString(session.friendlyId)
  if (friendlyId) return friendlyId
  const key = readString(session.key)
  if (key) return key
  return null
}

function kindTone(kindValue: string): string {
  const normalizedKind = kindValue.toLowerCase()
  if (normalizedKind === 'main')
    return 'border-primary-300 bg-primary-100 text-primary-800'
  if (normalizedKind === 'direct')
    return 'border-primary-200 bg-primary-100 text-primary-700'
  if (normalizedKind === 'isolated')
    return 'border-primary-200 bg-primary-50 text-primary-700'
  return 'border-primary-200 bg-primary-50 text-primary-600'
}

function KindBadge({ kind }: { kind: string }) {
  return (
    <span
      className={cn(
        'inline-flex items-center rounded-full border px-2 py-1 text-xs font-medium',
        kindTone(kind),
      )}
    >
      {kind}
    </span>
  )
}

async function fetchGatewaySessions(): Promise<Array<GatewaySession>> {
  const response = await fetch('/api/gateway/sessions')
  if (!response.ok) throw new Error(`HTTP ${response.status}`)
  const payload = (await response.json()) as GatewaySessionsResponse
  if (payload.ok === false) {
    throw new Error(payload.error || 'Failed to load gateway sessions')
  }
  const sessions = payload.data?.sessions ?? payload.sessions ?? []
  return Array.isArray(sessions) ? sessions : []
}

function InstancesRoute() {
  usePageTitle('Instances')
  const navigate = useNavigate()
  const query = useQuery({
    queryKey: ['gateway', 'instances'],
    queryFn: fetchGatewaySessions,
    refetchInterval: 10_000,
    retry: 1,
  })

  const sessions = useMemo(
    function sortByRecent() {
      const values = [...(query.data ?? [])]
      values.sort(compareSessions)
      return values
    },
    [query.data],
  )

  const lastUpdatedAtLabel = query.dataUpdatedAt
    ? new Date(query.dataUpdatedAt).toLocaleTimeString()
    : null

  function handleOpenChat(session: GatewaySession) {
    const sessionParam = resolveChatSessionParam(session)
    if (!sessionParam) return
    void navigate({
      to: '/chat/$sessionKey',
      params: { sessionKey: sessionParam },
    })
  }

  return (
    <div className="h-full overflow-auto bg-surface px-3 pt-3 pb-24 sm:px-4 sm:pt-4 md:pb-0">
      <div className="mx-auto max-w-6xl space-y-4">
        <header className="rounded-xl border border-primary-200 bg-primary-50/80 px-3 py-3 sm:px-5 sm:py-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex items-center gap-3">
              <h1 className="text-lg font-medium text-balance text-primary-900 md:text-xl">
                Instances
              </h1>
              <span className="inline-flex items-center rounded-full border border-primary-200 bg-primary-100 px-2.5 py-1 text-xs font-medium text-primary-700 tabular-nums">
                {sessions.length}
              </span>
              {query.isFetching && !query.isLoading ? (
                <span className="text-xs text-primary-500">Refreshing...</span>
              ) : null}
            </div>
            {lastUpdatedAtLabel ? (
              <span className="text-xs text-primary-500 tabular-nums">
                Updated {lastUpdatedAtLabel}
              </span>
            ) : null}
          </div>
          <p className="mt-2 text-sm text-pretty text-primary-600">
            Live gateway sessions currently available in chat.
          </p>
        </header>

        {query.isLoading ? (
          <div className="flex h-40 items-center justify-center rounded-xl border border-primary-200 bg-primary-50/60">
            <div className="flex items-center gap-2 text-sm text-primary-600">
              <span className="size-4 animate-spin rounded-full border-2 border-primary-300 border-t-primary-700" />
              Loading active instances...
            </div>
          </div>
        ) : query.isError ? (
          <div className="flex h-40 flex-col items-center justify-center gap-3 rounded-xl border border-primary-200 bg-primary-50/60 px-6">
            <p className="text-sm text-pretty text-primary-700">
              {query.error instanceof Error
                ? query.error.message
                : 'Failed to fetch gateway sessions'}
            </p>
            <Button
              variant="outline"
              size="sm"
              onClick={function onRetry() {
                void query.refetch()
              }}
            >
              Retry
            </Button>
          </div>
        ) : sessions.length === 0 ? (
          <div className="flex h-48 flex-col items-center justify-center gap-3 rounded-xl border border-primary-200 bg-primary-50/60 px-6 text-center">
            <div className="flex size-10 items-center justify-center rounded-full border border-primary-200 bg-primary-100 text-primary-500">
              <HugeiconsIcon icon={BotIcon} size={20} strokeWidth={1.5} />
            </div>
            <p className="text-base font-medium text-balance text-primary-900">
              No active instances
            </p>
            <p className="max-w-lg text-sm text-pretty text-primary-600">
              Start or resume a gateway session to see it listed here.
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            <div className="hidden overflow-hidden rounded-xl border border-primary-200 bg-primary-50/60 md:block">
              <table className="min-w-full table-fixed text-left text-sm">
                <thead className="border-b border-primary-200 bg-primary-100/70">
                  <tr className="text-xs font-medium text-primary-600">
                    <th className="px-4 py-2">Status</th>
                    <th className="px-4 py-2">Session Key</th>
                    <th className="px-4 py-2">Friendly ID</th>
                    <th className="px-4 py-2">Kind</th>
                    <th className="px-4 py-2">Model</th>
                    <th className="px-4 py-2">Created</th>
                    <th className="px-4 py-2">Updated</th>
                    <th className="px-4 py-2 text-right">Action</th>
                  </tr>
                </thead>
                <tbody>
                  {sessions.map(function renderRow(session, index) {
                    const sessionKey = readString(session.key)
                    const friendlyId = readString(session.friendlyId)
                    const kind =
                      readString(session.kind).toLowerCase() || 'unknown'
                    const model = readString(session.model) || 'N/A'
                    const createdAt = readTimestamp(session.createdAt)
                    const updatedAt = readTimestamp(session.updatedAt)
                    const rowKey =
                      sessionKey || friendlyId || `gateway-session-${index}`
                    const chatParam = resolveChatSessionParam(session)

                    return (
                      <tr
                        key={rowKey}
                        className="border-b border-primary-100 text-primary-700 last:border-b-0"
                      >
                        <td className="px-4 py-3">
                          <span className="inline-flex items-center gap-1.5 text-xs font-medium text-primary-700">
                            <span className="size-2 rounded-full bg-emerald-500" />
                            active
                          </span>
                        </td>
                        <td className="px-4 py-3 font-mono text-xs text-primary-700">
                          <span
                            className="block truncate"
                            title={sessionKey || 'N/A'}
                          >
                            {sessionKey ? formatSessionKey(sessionKey) : 'N/A'}
                          </span>
                        </td>
                        <td className="px-4 py-3 text-xs text-primary-700">
                          <span
                            className="block truncate tabular-nums"
                            title={friendlyId || 'N/A'}
                          >
                            {friendlyId || 'N/A'}
                          </span>
                        </td>
                        <td className="px-4 py-3 text-xs">
                          <KindBadge kind={kind} />
                        </td>
                        <td className="px-4 py-3 font-mono text-xs text-primary-700">
                          <span className="block truncate" title={model}>
                            {model}
                          </span>
                        </td>
                        <td className="px-4 py-3 text-xs text-primary-700">
                          <p className="tabular-nums">
                            {formatAbsoluteTimestamp(createdAt)}
                          </p>
                          <p className="tabular-nums text-primary-500">
                            {formatRelativeTimestamp(createdAt)}
                          </p>
                        </td>
                        <td className="px-4 py-3 text-xs text-primary-700">
                          <p className="tabular-nums">
                            {formatAbsoluteTimestamp(updatedAt)}
                          </p>
                          <p className="tabular-nums text-primary-500">
                            {formatRelativeTimestamp(updatedAt)}
                          </p>
                        </td>
                        <td className="px-4 py-3 text-right">
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={!chatParam}
                            onClick={function onOpenChat() {
                              handleOpenChat(session)
                            }}
                          >
                            Open chat
                          </Button>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>

            <div className="space-y-3 md:hidden">
              {sessions.map(function renderCard(session, index) {
                const sessionKey = readString(session.key)
                const friendlyId = readString(session.friendlyId)
                const kind = readString(session.kind).toLowerCase() || 'unknown'
                const model = readString(session.model) || 'N/A'
                const createdAt = readTimestamp(session.createdAt)
                const updatedAt = readTimestamp(session.updatedAt)
                const cardKey = sessionKey || friendlyId || `session-${index}`
                const chatParam = resolveChatSessionParam(session)

                return (
                  <article
                    key={cardKey}
                    className="rounded-xl border border-primary-200 bg-primary-50/60 p-3"
                  >
                    <div className="flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <div className="flex items-center gap-2">
                          <span className="inline-flex items-center gap-1.5 text-xs font-medium text-primary-700">
                            <span className="size-2 rounded-full bg-emerald-500" />
                            active
                          </span>
                          <KindBadge kind={kind} />
                        </div>
                        <p
                          className="mt-2 truncate font-mono text-xs text-primary-700"
                          title={sessionKey || 'N/A'}
                        >
                          {sessionKey ? formatSessionKey(sessionKey) : 'N/A'}
                        </p>
                        <p className="mt-1 truncate text-sm font-medium text-primary-900 tabular-nums">
                          {friendlyId || 'N/A'}
                        </p>
                      </div>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={!chatParam}
                        onClick={function onOpenChat() {
                          handleOpenChat(session)
                        }}
                      >
                        Open
                      </Button>
                    </div>

                    <dl className="mt-3 grid grid-cols-2 gap-2 text-xs">
                      <div className="rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                        <dt className="text-primary-500">Model</dt>
                        <dd className="truncate font-mono text-primary-700">
                          {model}
                        </dd>
                      </div>
                      <div className="rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                        <dt className="text-primary-500">Updated</dt>
                        <dd className="tabular-nums text-primary-700">
                          {formatRelativeTimestamp(updatedAt)}
                        </dd>
                      </div>
                      <div className="rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                        <dt className="text-primary-500">Created</dt>
                        <dd className="tabular-nums text-primary-700">
                          {formatAbsoluteTimestamp(createdAt)}
                        </dd>
                      </div>
                      <div className="rounded-lg border border-primary-200 bg-primary-50 px-2 py-1.5">
                        <dt className="text-primary-500">Last update</dt>
                        <dd className="tabular-nums text-primary-700">
                          {formatAbsoluteTimestamp(updatedAt)}
                        </dd>
                      </div>
                    </dl>
                  </article>
                )
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
