/**
 * Notifications bell icon + dropdown popover for the dashboard header.
 * Shows session lifecycle events (starts, errors, cron runs).
 */
import { Notification03Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useQuery } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState } from 'react'
import { cn } from '@/lib/utils'

type SessionsApiResponse = {
  sessions?: Array<Record<string, unknown>>
}

type NotificationItem = {
  id: string
  label: string
  detail: string
  occurredAt: number
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function normalizeTimestamp(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value < 1_000_000_000_000 ? value * 1000 : value
  }
  if (typeof value === 'string') {
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return parsed
  }
  return Date.now()
}

function formatRelativeTime(timestamp: number): string {
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

async function fetchSessions(): Promise<Array<Record<string, unknown>>> {
  const response = await fetch('/api/sessions')
  if (!response.ok) return []
  const payload = (await response.json()) as SessionsApiResponse
  return Array.isArray(payload.sessions) ? payload.sessions : []
}

function toNotifications(
  rows: Array<Record<string, unknown>>,
): NotificationItem[] {
  return rows
    .map((session, index) => {
      const key = readString(session.friendlyId) || `session-${index}`
      const updatedAt = normalizeTimestamp(
        session.updatedAt ?? session.startedAt ?? session.createdAt,
      )
      const status = readString(session.status).toLowerCase()
      const label =
        readString(session.label) ||
        readString(session.title) ||
        readString(session.derivedTitle) ||
        key

      if (status.includes('error')) {
        return {
          id: `${key}-err`,
          label: 'Error',
          detail: `${label} reported an error`,
          occurredAt: updatedAt,
        }
      }
      return {
        id: `${key}-start`,
        label: 'Session',
        detail: `Session started: ${label}`,
        occurredAt: updatedAt,
      }
    })
    .sort((a, b) => b.occurredAt - a.occurredAt)
    .slice(0, 8)
}

export function NotificationsPopover() {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  const query = useQuery({
    queryKey: ['dashboard', 'notifications-popover'],
    queryFn: fetchSessions,
    refetchInterval: 20_000,
  })

  const notifications = useMemo(() => {
    return toNotifications(Array.isArray(query.data) ? query.data : [])
  }, [query.data])

  const hasErrors = notifications.some((n) => n.label === 'Error')

  useEffect(() => {
    if (!open) return
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', handleClick)
    return () => document.removeEventListener('mousedown', handleClick)
  }, [open])

  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className="relative inline-flex size-7 items-center justify-center rounded-md text-primary-400 transition-colors hover:text-primary-700 dark:hover:text-primary-300"
        aria-label="Notifications"
        title="Notifications"
      >
        <HugeiconsIcon icon={Notification03Icon} size={15} strokeWidth={1.5} />
        {hasErrors ? (
          <span className="absolute -top-0.5 -right-0.5 size-2 rounded-full bg-red-500" />
        ) : null}
      </button>

      {open ? (
        <div className="absolute right-0 top-full z-[9999] mt-2 w-72 rounded-xl border border-primary-200 bg-primary-50 p-3 shadow-xl backdrop-blur-xl dark:bg-primary-100">
          <h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-primary-500">
            Notifications
          </h3>

          {notifications.length === 0 ? (
            <p className="py-4 text-center text-[13px] text-primary-400">
              No recent activity
            </p>
          ) : (
            <div className="max-h-64 space-y-1.5 overflow-y-auto">
              {notifications.map((item) => (
                <div
                  key={item.id}
                  className="rounded-lg border border-primary-200 bg-primary-50/80 px-2.5 py-2"
                >
                  <div className="flex items-center justify-between gap-2">
                    <span
                      className={cn(
                        'text-[11px] font-medium',
                        item.label === 'Error'
                          ? 'text-red-600'
                          : 'text-primary-600',
                      )}
                    >
                      {item.label}
                    </span>
                    <span className="text-[10px] text-primary-400 tabular-nums">
                      {formatRelativeTime(item.occurredAt)}
                    </span>
                  </div>
                  <p className="mt-0.5 line-clamp-2 text-[11px] text-primary-500">
                    {item.detail}
                  </p>
                </div>
              ))}
            </div>
          )}
        </div>
      ) : null}
    </div>
  )
}
