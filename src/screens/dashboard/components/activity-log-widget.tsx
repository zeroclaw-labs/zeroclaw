import { Activity01Icon } from '@hugeicons/core-free-icons'
import { useNavigate } from '@tanstack/react-router'
import { useMemo } from 'react'
import { WidgetShell } from './widget-shell'
import type { ActivityEvent } from '@/types/activity-event'
import { useActivityEvents } from '@/screens/activity/use-activity-events'
import { cn } from '@/lib/utils'

type ActivityLogWidgetProps = {
  draggable?: boolean
  onRemove?: () => void
  editMode?: boolean
}

type ActivityPreviewRow = {
  id: string
  icon: string
  iconClassName: string
  sourceLabel: string
  summary: string
  timestamp: number
}

type ParsedActivityItem = {
  id: string
  title: string
  subtitle: string
  timeAgo: string
  statusIcon: 'success' | 'warning' | 'error' | 'info'
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function looksLikeJson(raw: string): boolean {
  const text = raw.trim()
  if (!text) return false
  return text.startsWith('{') || text.startsWith('[')
}

function parseJsonRecord(raw: string): Record<string, unknown> | null {
  try {
    const parsed = JSON.parse(raw) as unknown
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return null
    }
    return parsed as Record<string, unknown>
  } catch {
    return null
  }
}

function toFriendlySource(source?: string): string {
  const text = readString(source)
  if (!text) return 'Gateway'
  const segments = text.split(':').filter(Boolean)
  const tail = segments[segments.length - 1] ?? text
  if (!tail) return 'Gateway'
  return tail.charAt(0).toUpperCase() + tail.slice(1)
}

function formatModelName(raw: string): string {
  if (!raw) return 'Unknown model'
  const lower = raw.toLowerCase()
  if (lower.includes('opus')) {
    const match = raw.match(/opus[- ]?(\d+)[- ]?(\d+)/i)
    return match ? `Opus ${match[1]}.${match[2]}` : 'Opus'
  }
  if (lower.includes('sonnet')) {
    const match = raw.match(/sonnet[- ]?(\d+)[- ]?(\d+)/i)
    return match ? `Sonnet ${match[1]}.${match[2]}` : 'Sonnet'
  }
  if (lower.includes('gpt')) return raw.replace('gpt-', 'GPT-')
  if (raw.includes('/')) return raw.split('/').pop() ?? raw
  return raw
}

function formatRelativeTime(timestamp: number): string {
  const diffMs = Math.max(0, Date.now() - timestamp)
  const seconds = Math.floor(diffMs / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h`
  const days = Math.floor(hours / 24)
  return `${days}d`
}

function parseKnownJsonEvent(event: ActivityEvent): ActivityPreviewRow | null {
  const rawJson =
    (looksLikeJson(event.detail ?? '') && (event.detail ?? '')) ||
    (looksLikeJson(event.title) ? event.title : '')
  if (!rawJson) return null

  const parsed = parseJsonRecord(rawJson)
  if (!parsed) return null

  const parsedError = readString(parsed.error)
  const parsedMessage = readString(parsed.message)
  const parsedModel =
    readString(parsed.model) ||
    readString(parsed.currentModel) ||
    readString(parsed.modelAlias)
  const parsedEventType = readString(parsed.event).toLowerCase()
  const summarySource = toFriendlySource(event.source)

  if (
    event.level === 'error' ||
    event.type === 'error' ||
    parsedError.length > 0 ||
    parsedEventType.includes('error') ||
    parsed.ok === false
  ) {
    return {
      id: event.id,
      icon: '⚠',
      iconClassName: 'text-amber-600',
      sourceLabel: summarySource,
      summary: parsedError || parsedMessage || 'Error event',
      timestamp: event.timestamp,
    }
  }

  const hasGatewayTick =
    event.type === 'gateway' &&
    (parsed.ok === true ||
      typeof parsed.ts === 'number' ||
      typeof parsed.timestamp === 'number' ||
      parsedEventType.includes('tick') ||
      parsedEventType.includes('health'))
  if (hasGatewayTick) {
    return {
      id: event.id,
      icon: '✓',
      iconClassName: 'text-emerald-600',
      sourceLabel: summarySource,
      summary: 'Gateway health check',
      timestamp: event.timestamp,
    }
  }

  if (event.type === 'session' || parsedEventType.includes('session')) {
    return {
      id: event.id,
      icon: '•',
      iconClassName: 'text-primary-500',
      sourceLabel: summarySource,
      summary: `Session started: ${formatModelName(parsedModel || 'Unknown model')}`,
      timestamp: event.timestamp,
    }
  }

  return null
}

function toPreviewRow(event: ActivityEvent): ActivityPreviewRow | null {
  const parsedJsonEvent = parseKnownJsonEvent(event)
  if (parsedJsonEvent) return parsedJsonEvent

  if (looksLikeJson(event.detail ?? '') || looksLikeJson(event.title)) {
    return null
  }

  const sourceLabel = toFriendlySource(event.source)
  if (event.level === 'error' || event.type === 'error') {
    return {
      id: event.id,
      icon: '⚠',
      iconClassName: 'text-amber-600',
      sourceLabel,
      summary: readString(event.title) || readString(event.detail) || 'Error event',
      timestamp: event.timestamp,
    }
  }

  if (event.type === 'gateway') {
    return {
      id: event.id,
      icon: '✓',
      iconClassName: 'text-emerald-600',
      sourceLabel,
      summary: 'Gateway health check',
      timestamp: event.timestamp,
    }
  }

  if (event.type === 'session') {
    return {
      id: event.id,
      icon: '•',
      iconClassName: 'text-primary-500',
      sourceLabel,
      summary: readString(event.title) || 'Session started',
      timestamp: event.timestamp,
    }
  }

  const fallbackSummary = readString(event.title) || readString(event.detail)
  if (!fallbackSummary) return null

  return {
    id: event.id,
    icon: '•',
    iconClassName: 'text-primary-500',
    sourceLabel,
    summary: fallbackSummary,
    timestamp: event.timestamp,
  }
}

function parseActivityItem(event: ActivityEvent): ParsedActivityItem {
  const parsed = toPreviewRow(event)
  const timeAgo = formatRelativeTime(event.timestamp)
  const fallbackSubtitle = toFriendlySource(event.source)

  if (!parsed) {
    return {
      id: event.id,
      title: 'New activity received',
      subtitle: fallbackSubtitle,
      timeAgo,
      statusIcon: event.level === 'error' || event.type === 'error' ? 'error' : 'info',
    }
  }

  const statusIcon: ParsedActivityItem['statusIcon'] =
    event.level === 'error' || event.type === 'error'
      ? 'error'
      : parsed.icon === '✓'
        ? 'success'
        : parsed.icon === '⚠'
          ? 'warning'
          : 'info'

  return {
    id: parsed.id,
    title: parsed.summary,
    subtitle: parsed.sourceLabel,
    timeAgo: formatRelativeTime(parsed.timestamp),
    statusIcon,
  }
}

function activityStatusDotClass(statusIcon: ParsedActivityItem['statusIcon']): string {
  if (statusIcon === 'success') return 'bg-emerald-500'
  if (statusIcon === 'warning') return 'bg-amber-500'
  if (statusIcon === 'error') return 'bg-red-500'
  return 'bg-primary-400'
}

export function ActivityLogWidget({
  draggable: _draggable = false,
  onRemove,
  editMode,
}: ActivityLogWidgetProps) {
  const navigate = useNavigate()
  const { events, isConnected, isLoading } = useActivityEvents({
    initialCount: 20,
    maxEvents: 100,
  })

  const previewRows = useMemo(
    function buildPreviewRows() {
      const rows: Array<ActivityPreviewRow> = []

      for (let index = events.length - 1; index >= 0; index -= 1) {
        const event = events[index]
        if (!event) continue
        const row = toPreviewRow(event)
        if (!row) continue
        rows.push(row)
        if (rows.length >= 4) break
      }

      return rows
    },
    [events],
  )

  const mobileRows = useMemo(
    function buildMobileRows() {
      const rows: Array<ParsedActivityItem> = []

      for (let index = events.length - 1; index >= 0; index -= 1) {
        const event = events[index]
        if (!event) continue
        rows.push(parseActivityItem(event))
        if (rows.length >= 4) break
      }

      return rows
    },
    [events],
  )

  return (
    <WidgetShell
      size="large"
      title="Activity Log"
      icon={Activity01Icon}
      action={
        <span className="inline-flex items-center rounded-full border border-primary-200 bg-primary-100/70 px-2 py-0.5 text-[11px] font-medium text-primary-500 tabular-nums">
          {Math.max(previewRows.length, mobileRows.length)}
        </span>
      }
      onRemove={onRemove}
      editMode={editMode}
      className="h-full"
    >
      <div className="mb-2">
        <span
          className={cn(
            'inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium',
            isConnected
              ? 'border-emerald-200 bg-emerald-100/70 text-emerald-700'
              : 'border-red-200 bg-red-100/80 text-red-700',
          )}
        >
          <span
            className={cn(
              'size-1.5 rounded-full',
              isConnected ? 'animate-pulse bg-emerald-500' : 'bg-red-500',
            )}
          />
          {isConnected ? 'Live' : 'Disconnected'}
        </span>
      </div>

      {isLoading && mobileRows.length === 0 ? (
        <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
          Loading activity…
        </div>
      ) : mobileRows.length === 0 ? (
        <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
          No activity events yet
        </div>
      ) : (
        <>
          <div className="space-y-1.5 md:hidden">
            {mobileRows.slice(0, 4).map(function renderMobileRow(row) {
              return (
                <article
                  key={row.id}
                  className="flex items-center gap-2 rounded-xl border border-white/30 bg-white/55 px-3 py-2 dark:border-white/10 dark:bg-neutral-900/45"
                >
                  <span
                    className={cn(
                      'size-2 shrink-0 rounded-full',
                      activityStatusDotClass(row.statusIcon),
                    )}
                  />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium text-neutral-800 dark:text-neutral-100">
                      {row.title}
                    </p>
                    <p className="truncate text-xs text-neutral-500 dark:text-neutral-400">
                      {row.subtitle}
                    </p>
                  </div>
                  <span className="shrink-0 text-xs text-neutral-500 dark:text-neutral-400">
                    {row.timeAgo}
                  </span>
                </article>
              )
            })}
          </div>

          <div className="hidden space-y-1.5 md:block">
            {previewRows.map(function renderRow(row) {
              return (
                <article
                  key={row.id}
                  className="rounded-lg border border-primary-200/80 bg-primary-50/70 px-3 py-2"
                >
                  <div className="flex items-start gap-2">
                    <span className={cn('mt-0.5 text-xs', row.iconClassName)}>
                      {row.icon}
                    </span>
                    <div className="min-w-0 flex-1">
                      <p className="line-clamp-2 text-sm text-primary-700">
                        <span className="font-semibold text-ink">{row.sourceLabel}</span>{' '}
                        <span>{row.summary}</span>
                      </p>
                    </div>
                    <span className="shrink-0 text-[11px] text-primary-400">
                      {formatRelativeTime(row.timestamp)}
                    </span>
                  </div>
                </article>
              )
            })}
          </div>
        </>
      )}

      <div className="mt-2 flex justify-end">
        <button
          type="button"
          onClick={() => void navigate({ to: '/activity' })}
          className="inline-flex items-center gap-1 text-xs font-medium text-primary-500 transition-colors hover:text-accent-600"
        >
          View all →
        </button>
      </div>
    </WidgetShell>
  )
}
