import { memo } from 'react'
import {
  Activity01Icon,
  AiBookIcon,
  ChartLineData02Icon,
  Clock01Icon,
  Notification03Icon,
  Task01Icon,
  UserGroupIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type { ActivityEvent } from '@/types/activity-event'
import { cn } from '@/lib/utils'

function getEventIcon(eventType: ActivityEvent['type']) {
  if (eventType === 'gateway') return Activity01Icon
  if (eventType === 'model') return AiBookIcon
  if (eventType === 'usage') return ChartLineData02Icon
  if (eventType === 'cron') return Clock01Icon
  if (eventType === 'tool') return Task01Icon
  if (eventType === 'error') return Notification03Icon
  return UserGroupIcon
}

function getLevelDotClass(level: ActivityEvent['level']): string {
  if (level === 'debug') return 'bg-primary-400'
  if (level === 'info') return 'bg-blue-500'
  if (level === 'warn') return 'bg-amber-500'
  return 'bg-red-500'
}

function getLevelBorderClass(level: ActivityEvent['level']): string {
  if (level === 'error') return 'border-l-red-500'
  if (level === 'warn') return 'border-l-amber-500'
  if (level === 'info') return 'border-l-blue-500'
  return 'border-l-primary-400'
}

function getTypeLabel(eventType: ActivityEvent['type']): string {
  if (eventType === 'gateway') return 'Gateway'
  if (eventType === 'model') return 'Model'
  if (eventType === 'usage') return 'Usage'
  if (eventType === 'cron') return 'Cron'
  if (eventType === 'tool') return 'Tool'
  if (eventType === 'error') return 'Error'
  return 'Session'
}

export function formatRelativeTimestamp(timestamp: number): string {
  const diffMs = Math.max(0, Date.now() - timestamp)
  const seconds = Math.floor(diffMs / 1000)

  if (seconds < 60) return `${seconds}s ago`

  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`

  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`

  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

export const ActivityEventRow = memo(function ActivityEventRow({
  event,
}: {
  event: ActivityEvent
}) {
  return (
    <article
      className={cn(
        'rounded-lg border border-primary-200 border-l-2 bg-primary-50/80 px-2.5 py-2',
        getLevelBorderClass(event.level),
      )}
    >
      <div className="flex items-start gap-2.5">
        <span
          className={cn(
            'mt-1.5 inline-flex size-2 shrink-0 rounded-full',
            getLevelDotClass(event.level),
          )}
        />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <HugeiconsIcon
              icon={getEventIcon(event.type)}
              size={20}
              strokeWidth={1.5}
            />
            <span className="rounded-md border border-primary-200 bg-primary-100/70 px-1.5 py-0.5 text-[11px] text-primary-700 tabular-nums">
              {getTypeLabel(event.type)}
            </span>
            <span className="text-[11px] text-primary-600 tabular-nums">
              {formatRelativeTimestamp(event.timestamp)}
            </span>
          </div>
          <p className="mt-1 line-clamp-2 text-sm font-medium text-primary-900 text-pretty">
            {event.title}
          </p>

          {event.detail ? (
            <details className="mt-1.5">
              <summary className="cursor-pointer text-[11px] text-primary-600 tabular-nums">
                Detail
              </summary>
              <p className="mt-1 rounded-md border border-primary-200 bg-primary-100/60 px-2 py-1.5 text-xs text-primary-700 text-pretty">
                {event.detail}
              </p>
            </details>
          ) : null}
        </div>
      </div>
    </article>
  )
})
