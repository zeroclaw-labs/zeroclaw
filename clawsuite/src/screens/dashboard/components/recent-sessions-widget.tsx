import { ArrowRight01Icon, Clock01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { DashboardGlassCard } from './dashboard-glass-card'
import type { RecentSession } from './dashboard-types'
import type { SessionMeta } from '@/screens/chat/types'
import { Button } from '@/components/ui/button'
import { chatQueryKeys, fetchSessions } from '@/screens/chat/chat-queries'
import { getMessageTimestamp, textFromMessage } from '@/screens/chat/utils'
import { cn } from '@/lib/utils'

type RecentSessionsWidgetProps = {
  onOpenSession: (sessionKey: string) => void
  draggable?: boolean
  onRemove?: () => void
}

function formatSessionTimestamp(value: number): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return 'Unknown'

  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  }).format(date)
}

function cleanTitle(raw: string): string {
  if (!raw) return ''
  if (/^a new session was started/i.test(raw)) return ''
  let cleaned = raw.replace(/^\[.*?\]\s*/, '')
  cleaned = cleaned.replace(/\[?message_id:\s*\S+\]?/gi, '').trim()
  return cleaned
}

function toSessionTitle(session: SessionMeta): string {
  const label = cleanTitle(session.label ?? '')
  if (label) return label
  const title = cleanTitle(session.title ?? '')
  if (title) return title
  const derived = cleanTitle(session.derivedTitle ?? '')
  if (derived) return derived
  return session.friendlyId === 'main'
    ? 'Main Session'
    : `Session ${session.friendlyId}`
}

function toSessionPreview(session: SessionMeta): string {
  if (session.lastMessage) {
    const preview = textFromMessage(session.lastMessage)
    if (preview.length > 0 && !/^a new session was started/i.test(preview)) {
      return preview.length > 120 ? `${preview.slice(0, 117)}…` : preview
    }
  }
  const title = (session.label ?? session.title ?? '').toLowerCase()
  if (title.startsWith('cron:') || title.includes('cron'))
    return 'Scheduled task'
  return 'New session'
}

function toSessionUpdatedAt(session: SessionMeta): number {
  if (typeof session.updatedAt === 'number') return session.updatedAt
  if (session.lastMessage) return getMessageTimestamp(session.lastMessage)
  return 0
}

export function RecentSessionsWidget({
  onOpenSession,
  draggable = false,
  onRemove,
}: RecentSessionsWidgetProps) {
  const sessionsQuery = useQuery({
    queryKey: chatQueryKeys.sessions,
    queryFn: fetchSessions,
    refetchInterval: 30_000,
  })

  const sessions = useMemo(
    function buildRecentSessions() {
      const rows = Array.isArray(sessionsQuery.data) ? sessionsQuery.data : []

      return [...rows]
        .sort(function sortByMostRecent(a, b) {
          return toSessionUpdatedAt(b) - toSessionUpdatedAt(a)
        })
        .slice(0, 5)
        .map(function mapSession(session): RecentSession {
          return {
            friendlyId: session.friendlyId,
            title: toSessionTitle(session),
            preview: toSessionPreview(session),
            updatedAt: toSessionUpdatedAt(session),
          }
        })
    },
    [sessionsQuery.data],
  )

  const isLoading = sessionsQuery.isLoading && sessions.length === 0

  return (
    <DashboardGlassCard
      title="Recent Sessions"
      description=""
      icon={Clock01Icon}
      titleAccessory={
        <span className="inline-flex items-center rounded-full border border-primary-200 bg-primary-100/70 px-2 py-0.5 text-[11px] font-medium text-primary-500 tabular-nums">
          {sessions.length}
        </span>
      }
      draggable={draggable}
      onRemove={onRemove}
      className="h-full rounded-xl border-primary-200 p-3.5 md:p-4 shadow-sm [&_h2]:text-sm [&_h2]:font-semibold [&_h2]:normal-case [&_h2]:text-ink"
    >
      {isLoading ? (
        <div className="flex h-32 items-center justify-center gap-3 rounded-lg border border-primary-200 bg-primary-100/45">
          <span
            className="size-4 animate-spin rounded-full border-2 border-primary-300 border-t-accent-600"
            role="status"
            aria-label="Loading"
          />
          <span className="text-sm text-primary-600">Loading sessions…</span>
        </div>
      ) : sessions.length === 0 ? (
        <div className="flex h-32 flex-col items-center justify-center gap-1 rounded-lg border border-primary-200 bg-primary-100/45">
          <p className="text-sm font-semibold text-ink">No sessions yet</p>
          <p className="text-xs text-primary-500">
            Start a conversation to see recent sessions here
          </p>
        </div>
      ) : (
        <div className="space-y-2.5">
          {sessions.map(function mapSession(session, index) {
            return (
              <Button
                key={session.friendlyId}
                variant="outline"
                className={cn(
                  'group h-auto w-full flex-col items-start rounded-lg border border-primary-200 px-3.5 py-3 text-left shadow-sm transition-all hover:-translate-y-[1px] hover:border-accent-200',
                  index % 2 === 0
                    ? 'bg-primary-50/90 hover:bg-primary-50'
                    : 'bg-primary-100/55 hover:bg-primary-100/70',
                )}
                onClick={function onSessionClick() {
                  onOpenSession(session.friendlyId)
                }}
              >
                <div className="flex w-full items-center justify-between gap-3">
                  <span className="line-clamp-1 text-sm font-semibold text-ink text-balance">
                    {session.title}
                  </span>
                  <span className="flex shrink-0 items-center gap-1">
                    <span className="font-mono text-xs text-primary-500 tabular-nums">
                      {formatSessionTimestamp(session.updatedAt)}
                    </span>
                    <HugeiconsIcon
                      icon={ArrowRight01Icon}
                      size={14}
                      strokeWidth={1.5}
                      className="text-accent-500 opacity-0 transition-opacity group-hover:opacity-100"
                    />
                  </span>
                </div>
                <p className="mt-1 line-clamp-2 w-full text-left text-sm text-primary-600 text-pretty">
                  {session.preview}
                </p>
              </Button>
            )
          })}
        </div>
      )}
    </DashboardGlassCard>
  )
}
