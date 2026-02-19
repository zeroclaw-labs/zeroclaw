import {
  Activity01Icon,
  Copy01Icon,
  Download04Icon,
  Github01Icon,
  Link01Icon,
  Notification03Icon,
  PackageIcon,
  Tick02Icon,
  Wrench01Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { ActivityEvent } from '@/types/activity-event'
import type { DiagnosticsBundle } from '@/lib/diagnostics'
import { DashboardGlassCard } from '@/screens/dashboard/components/dashboard-glass-card'
import { ActivityEventRow } from '@/screens/activity/components/activity-event-row'
import { useActivityEvents } from '@/screens/activity/use-activity-events'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { cn } from '@/lib/utils'
import {
  DIAGNOSTICS_BUNDLE_VERSION,
  buildGitHubIssueUrl,
  downloadBundle,
  redactSensitiveData,
} from '@/lib/diagnostics'

type DebugConnectionState = 'connecting' | 'connected' | 'disconnected'

type DebugConnectionStatus = {
  state: DebugConnectionState
  gatewayUrl: string
  connectedSinceMs: number | null
  lastDisconnectedAtMs: number | null
  nowMs: number
}

type TroubleshooterRule = {
  id: string
  patterns: Array<RegExp>
  suggestion: string
  command: string
}

type TroubleshooterSuggestion = {
  id: string
  suggestion: string
  command: string
  matchedTitle: string
  matchedAt: number
}

type ActivityLogFilterId =
  | 'all'
  | 'errors'
  | 'agent-activity'
  | 'gateway'
  | 'cron'

type ActivityLogFilterOption = {
  id: ActivityLogFilterId
  label: string
}

const MAX_ERROR_EVENTS = 20
const RECENT_ERROR_WINDOW_MS = 5 * 60 * 1000

const ACTIVITY_LOG_FILTERS: Array<ActivityLogFilterOption> = [
  { id: 'all', label: 'All' },
  { id: 'errors', label: 'Errors' },
  { id: 'agent-activity', label: 'Agent Activity' },
  { id: 'gateway', label: 'Gateway' },
  { id: 'cron', label: 'Cron' },
]

const TROUBLESHOOTER_RULES: Array<TroubleshooterRule> = [
  {
    id: 'gateway-closed',
    patterns: [/gateway connection closed/i],
    suggestion:
      'Check if OpenClaw Gateway is running: `openclaw gateway status`',
    command: 'openclaw gateway status',
  },
  {
    id: 'gateway-refused',
    patterns: [/gateway connection refused/i],
    suggestion: 'Start the Gateway: `openclaw gateway start`',
    command: 'openclaw gateway start',
  },
  {
    id: 'authentication-failed',
    patterns: [/authentication failed/i, /\b401\b/i],
    suggestion: 'Verify your Gateway token in openclaw.json',
    command: 'openclaw status',
  },
  {
    id: 'econnrefused',
    patterns: [/econnrefused/i],
    suggestion: 'Gateway may not be running. Try: `openclaw gateway restart`',
    command: 'openclaw gateway restart',
  },
  {
    id: 'timeout',
    patterns: [/timeout/i],
    suggestion: 'Gateway may be overloaded. Check system resources.',
    command: 'openclaw status',
  },
]

const DEFAULT_TROUBLESHOOTER_RULE: TroubleshooterRule = {
  id: 'unknown',
  patterns: [],
  suggestion: 'Run `openclaw status` for diagnostics',
  command: 'openclaw status',
}

const FALLBACK_CONNECTION_STATUS: DebugConnectionStatus = {
  state: 'connecting',
  gatewayUrl: 'Unavailable',
  connectedSinceMs: null,
  lastDisconnectedAtMs: null,
  nowMs: Date.now(),
}

function readErrorMessage(value: unknown): string {
  if (value instanceof Error) return value.message
  return String(value)
}

function toRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object') return null
  if (Array.isArray(value)) return null
  return value as Record<string, unknown>
}

function parseState(value: unknown): DebugConnectionState {
  if (value === 'connected') return 'connected'
  if (value === 'disconnected') return 'disconnected'
  return 'connecting'
}

function parseNumber(value: unknown): number | null {
  if (typeof value !== 'number') return null
  if (!Number.isFinite(value)) return null
  return value
}

function parseText(value: unknown, fallback: string): string {
  if (typeof value !== 'string') return fallback
  const trimmed = value.trim()
  return trimmed.length > 0 ? trimmed : fallback
}

function normalizeConnectionStatus(value: unknown): DebugConnectionStatus {
  const record = toRecord(value)
  if (!record) return FALLBACK_CONNECTION_STATUS

  return {
    state: parseState(record.state),
    gatewayUrl: parseText(
      record.gatewayUrl,
      FALLBACK_CONNECTION_STATUS.gatewayUrl,
    ),
    connectedSinceMs: parseNumber(record.connectedSinceMs),
    lastDisconnectedAtMs: parseNumber(record.lastDisconnectedAtMs),
    nowMs: parseNumber(record.nowMs) ?? Date.now(),
  }
}

async function fetchConnectionStatus(): Promise<DebugConnectionStatus> {
  const response = await fetch('/api/debug/status')
  const payload = (await response.json().catch(function onBadJson() {
    return {}
  })) as unknown

  if (!response.ok) {
    throw new Error('Unable to load debug connection status')
  }

  return normalizeConnectionStatus(payload)
}

async function requestReconnect(): Promise<void> {
  const response = await fetch('/api/debug/reconnect', {
    method: 'POST',
  })

  if (!response.ok) {
    const payload = (await response.json().catch(function onBadJson() {
      return {}
    })) as Record<string, unknown>
    const error =
      typeof payload.error === 'string'
        ? payload.error
        : 'Reconnect attempt failed'
    throw new Error(error)
  }
}

function formatElapsedDuration(valueMs: number): string {
  const safeMs = Math.max(0, valueMs)
  const totalSeconds = Math.floor(safeMs / 1000)
  const days = Math.floor(totalSeconds / 86_400)
  const hours = Math.floor((totalSeconds % 86_400) / 3_600)
  const minutes = Math.floor((totalSeconds % 3_600) / 60)
  const seconds = totalSeconds % 60

  if (days > 0) return `${days}d ${hours}h ${minutes}m`
  if (hours > 0) return `${hours}h ${minutes}m`
  if (minutes > 0) return `${minutes}m ${seconds}s`
  return `${seconds}s`
}

function formatEventTimestamp(timestamp: number): string {
  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(new Date(timestamp))
}

function resolveConnectionTiming(status: DebugConnectionStatus): {
  label: string
  value: string
} {
  if (status.state === 'connected' && status.connectedSinceMs) {
    return {
      label: 'Uptime',
      value: formatElapsedDuration(status.nowMs - status.connectedSinceMs),
    }
  }

  if (status.lastDisconnectedAtMs) {
    return {
      label: 'Since last disconnect',
      value: formatElapsedDuration(status.nowMs - status.lastDisconnectedAtMs),
    }
  }

  return {
    label: 'Since last disconnect',
    value: 'No disconnect event recorded',
  }
}

function getConnectionLabel(state: DebugConnectionState): string {
  if (state === 'connected') return 'Connected'
  if (state === 'disconnected') return 'Disconnected'
  return 'Connecting'
}

function getConnectionBadgeClass(state: DebugConnectionState): string {
  if (state === 'connected') {
    return 'border-emerald-200 bg-emerald-100/70 text-emerald-700'
  }
  if (state === 'disconnected') {
    return 'border-red-200 bg-red-100/70 text-red-700'
  }
  return 'border-amber-200 bg-amber-100/70 text-amber-700'
}

function getConnectionDotClass(state: DebugConnectionState): string {
  if (state === 'connected') return 'bg-emerald-500'
  if (state === 'disconnected') return 'bg-red-500'
  return 'bg-amber-500'
}

function isIssueLevel(level: ActivityEvent['level']): boolean {
  return level === 'error' || level === 'warn'
}

function filterIssueEvents(events: Array<ActivityEvent>): Array<ActivityEvent> {
  const issueEvents = events.filter(function keepIssueEvents(event) {
    return isIssueLevel(event.level)
  })

  return issueEvents.slice(issueEvents.length - MAX_ERROR_EVENTS).reverse()
}

function countRecentErrors(events: Array<ActivityEvent>): number {
  const cutoff = Date.now() - RECENT_ERROR_WINDOW_MS
  return events.reduce(function count(accumulator, event) {
    if (event.timestamp < cutoff) return accumulator
    if (isIssueLevel(event.level)) return accumulator + 1
    return accumulator
  }, 0)
}

function getSourceText(source: ActivityEvent['source']): string {
  if (!source) return ''
  return source.toLowerCase()
}

function matchesLogFilter(
  event: ActivityEvent,
  filterId: ActivityLogFilterId,
): boolean {
  if (filterId === 'all') return true

  if (filterId === 'errors') {
    return isIssueLevel(event.level)
  }

  const sourceText = getSourceText(event.source)
  if (!sourceText) return false

  if (filterId === 'agent-activity') {
    return sourceText.includes('agent') || sourceText.includes('subagent')
  }
  if (filterId === 'gateway') return sourceText.includes('gateway')
  return sourceText.includes('cron')
}

function matchesLogSearch(event: ActivityEvent, searchText: string): boolean {
  if (!searchText) return true

  const content =
    `${event.title}\n${event.detail || ''}\n${event.source || ''}`.toLowerCase()
  return content.includes(searchText)
}

function matchTroubleshooterRule(event: ActivityEvent): TroubleshooterRule {
  const content = `${event.title}\n${event.detail || ''}`

  for (const rule of TROUBLESHOOTER_RULES) {
    for (const pattern of rule.patterns) {
      if (pattern.test(content)) {
        return rule
      }
    }
  }

  return DEFAULT_TROUBLESHOOTER_RULE
}

function buildTroubleshooterSuggestions(
  issueEvents: Array<ActivityEvent>,
): Array<TroubleshooterSuggestion> {
  const seenRuleIds = new Set<string>()
  const suggestions: Array<TroubleshooterSuggestion> = []

  for (const event of issueEvents) {
    const rule = matchTroubleshooterRule(event)
    if (seenRuleIds.has(rule.id)) continue

    seenRuleIds.add(rule.id)
    suggestions.push({
      id: rule.id,
      suggestion: rule.suggestion,
      command: rule.command,
      matchedTitle: event.title,
      matchedAt: event.timestamp,
    })

    if (suggestions.length >= 5) break
  }

  if (suggestions.length > 0) return suggestions

  return [
    {
      id: DEFAULT_TROUBLESHOOTER_RULE.id,
      suggestion: DEFAULT_TROUBLESHOOTER_RULE.suggestion,
      command: DEFAULT_TROUBLESHOOTER_RULE.command,
      matchedTitle: 'No recent warn/error events detected.',
      matchedAt: Date.now(),
    },
  ]
}

export function DebugConsoleScreen() {
  const queryClient = useQueryClient()
  const [copiedSuggestionId, setCopiedSuggestionId] = useState<string | null>(
    null,
  )
  const [selectedLogFilter, setSelectedLogFilter] =
    useState<ActivityLogFilterId>('all')
  const [logSearch, setLogSearch] = useState('')
  const [isAutoScrollPinned, setIsAutoScrollPinned] = useState(true)
  const logViewportRef = useRef<HTMLDivElement | null>(null)

  const connectionQuery = useQuery({
    queryKey: ['debug', 'connection-status'],
    queryFn: fetchConnectionStatus,
    retry: false,
    refetchInterval: 15_000,
  })

  const reconnectMutation = useMutation({
    mutationFn: requestReconnect,
    onSuccess: async function onSuccess() {
      await queryClient.invalidateQueries({
        queryKey: ['debug', 'connection-status'],
      })
    },
  })

  const {
    events,
    isConnected: isActivityConnected,
    isLoading: isEventsLoading,
  } = useActivityEvents({
    initialCount: 80,
    maxEvents: 200,
  })

  const issueEvents = useMemo(
    function memoIssueEvents() {
      return filterIssueEvents(events)
    },
    [events],
  )

  const recentIssueCount = useMemo(
    function memoRecentIssueCount() {
      return countRecentErrors(events)
    },
    [events],
  )

  const normalizedLogSearch = useMemo(
    function memoNormalizedLogSearch() {
      return logSearch.trim().toLowerCase()
    },
    [logSearch],
  )

  const filteredLogEvents = useMemo(
    function memoFilteredLogEvents() {
      return events.filter(function keepMatchingEvent(event) {
        if (!matchesLogFilter(event, selectedLogFilter)) return false
        return matchesLogSearch(event, normalizedLogSearch)
      })
    },
    [events, normalizedLogSearch, selectedLogFilter],
  )

  const troubleshooterSuggestions = useMemo(
    function memoTroubleshooterSuggestions() {
      return buildTroubleshooterSuggestions(issueEvents)
    },
    [issueEvents],
  )

  const connectionStatus = connectionQuery.data || FALLBACK_CONNECTION_STATUS
  const connectionTiming = resolveConnectionTiming(connectionStatus)

  const scrollLogsToBottom = useCallback(function scrollLogsToBottom() {
    const viewport = logViewportRef.current
    if (!viewport) return
    viewport.scrollTop = viewport.scrollHeight
  }, [])

  useEffect(
    function keepLogViewportPinned() {
      if (!isAutoScrollPinned) return
      scrollLogsToBottom()
    },
    [
      filteredLogEvents.length,
      isAutoScrollPinned,
      normalizedLogSearch,
      scrollLogsToBottom,
      selectedLogFilter,
    ],
  )

  async function handleCopyCommand(id: string, command: string) {
    try {
      await navigator.clipboard.writeText(command)
      setCopiedSuggestionId(id)
      window.setTimeout(function clearCopiedState() {
        setCopiedSuggestionId(function resetIfCurrent(currentValue) {
          if (currentValue !== id) return currentValue
          return null
        })
      }, 1400)
    } catch {
      setCopiedSuggestionId(null)
    }
  }

  function handleReconnect() {
    reconnectMutation.mutate()
  }

  function handleToggleAutoScroll() {
    setIsAutoScrollPinned(function toggleAutoScroll(currentValue) {
      const nextValue = !currentValue
      if (nextValue) {
        window.requestAnimationFrame(function scrollAfterPin() {
          scrollLogsToBottom()
        })
      }
      return nextValue
    })
  }

  const [isExporting, setIsExporting] = useState(false)
  const [exportError, setExportError] = useState<string | null>(null)

  const handleExportDiagnostics = useCallback(
    async function exportDiagnostics() {
      setIsExporting(true)
      setExportError(null)

      try {
        // Fetch base diagnostics from server
        const response = await fetch('/api/diagnostics')
        if (!response.ok) throw new Error('Failed to fetch diagnostics')

        const serverBundle = (await response.json()) as DiagnosticsBundle

        // Enrich with client-side data
        const bundle: DiagnosticsBundle = {
          ...serverBundle,
          environment: {
            ...serverBundle.environment,
            userAgent: navigator.userAgent,
          },
          gateway: {
            status:
              connectionStatus.state === 'connected'
                ? 'connected'
                : 'disconnected',
            url: redactSensitiveData(connectionStatus.gatewayUrl),
            uptime:
              connectionTiming.label === 'Uptime'
                ? connectionTiming.value
                : null,
          },
          recentEvents: events.slice(0, 50).map((event) => ({
            timestamp: new Date(event.timestamp).toISOString(),
            level: event.level,
            title: redactSensitiveData(event.title),
            source: event.source ?? '',
          })),
          debugEntries: troubleshooterSuggestions.map((s) => ({
            timestamp: new Date(s.matchedAt).toISOString(),
            suggestion: s.suggestion,
            triggeredBy: redactSensitiveData(s.matchedTitle),
          })),
        }

        downloadBundle(bundle)
      } catch (err) {
        setExportError(err instanceof Error ? err.message : 'Export failed')
      } finally {
        setIsExporting(false)
      }
    },
    [connectionStatus, connectionTiming, events, troubleshooterSuggestions],
  )

  const handleOpenIssue = useCallback(
    function openIssue() {
      const bundle: DiagnosticsBundle = {
        version: DIAGNOSTICS_BUNDLE_VERSION,
        generatedAt: new Date().toISOString(),
        environment: {
          appVersion: '2.0.0',
          os: navigator.platform,
          nodeVersion: 'N/A (browser)',
          userAgent: navigator.userAgent,
        },
        gateway: {
          status:
            connectionStatus.state === 'connected'
              ? 'connected'
              : 'disconnected',
          url: redactSensitiveData(connectionStatus.gatewayUrl),
          uptime:
            connectionTiming.label === 'Uptime' ? connectionTiming.value : null,
        },
        workspace: { folderName: 'N/A' },
        providers: [],
        recentEvents: [],
        debugEntries: [],
      }

      const issueUrl = buildGitHubIssueUrl(bundle)
      window.open(issueUrl, '_blank', 'noopener,noreferrer')
    },
    [connectionStatus, connectionTiming],
  )

  return (
    <main className="h-full overflow-y-auto bg-surface px-4 pt-6 pb-24 text-primary-900 md:px-6 md:pt-8 md:pb-0">
      <div className="mx-auto w-full max-w-6xl space-y-4">
        <header className="rounded-2xl border border-primary-200 bg-primary-50/85 p-4 backdrop-blur-xl md:p-5">
          <h1 className="text-xl font-medium text-ink text-balance md:text-3xl">
            Gateway Debug Console
          </h1>
          <p className="mt-1 text-sm text-primary-600 text-pretty md:text-base">
            Diagnose connection failures, inspect recent issues, and get safe
            troubleshooting suggestions without running commands automatically.
          </p>
        </header>

        <DashboardGlassCard
          title="Connection Status"
          description="Current Gateway health with masked endpoint details."
          icon={Activity01Icon}
        >
          <div className="space-y-2.5 text-sm">
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-primary-200 bg-primary-100/50 px-3 py-2.5">
              <span className="text-primary-700 text-pretty">
                Gateway state
              </span>
              <span
                className={cn(
                  'inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-xs font-medium tabular-nums',
                  getConnectionBadgeClass(connectionStatus.state),
                )}
              >
                <span
                  className={cn(
                    'size-1.5 rounded-full',
                    getConnectionDotClass(connectionStatus.state),
                  )}
                />
                {getConnectionLabel(connectionStatus.state)}
              </span>
            </div>

            <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-primary-200 bg-primary-100/50 px-3 py-2.5">
              <span className="text-primary-700 text-pretty">Gateway URL</span>
              <code className="rounded-md border border-primary-200 bg-primary-50 px-2 py-1 font-mono text-xs text-primary-900 tabular-nums">
                {connectionStatus.gatewayUrl}
              </code>
            </div>

            <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-primary-200 bg-primary-100/50 px-3 py-2.5">
              <span className="text-primary-700 text-pretty">
                {connectionTiming.label}
              </span>
              <span className="font-medium text-ink tabular-nums">
                {connectionTiming.value}
              </span>
            </div>
          </div>

          <div className="mt-3 flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              onClick={handleReconnect}
              disabled={reconnectMutation.isPending}
            >
              {reconnectMutation.isPending ? 'Reconnecting…' : 'Reconnect'}
            </Button>
            {reconnectMutation.isError ? (
              <span className="text-xs text-red-600 text-pretty tabular-nums">
                {readErrorMessage(reconnectMutation.error)}
              </span>
            ) : null}
            {reconnectMutation.isSuccess ? (
              <span className="text-xs text-emerald-700 text-pretty tabular-nums">
                Reconnect attempt sent.
              </span>
            ) : null}
            {connectionQuery.isError ? (
              <span className="text-xs text-red-600 text-pretty tabular-nums">
                Unable to load connection diagnostics.
              </span>
            ) : null}
          </div>
        </DashboardGlassCard>

        <DashboardGlassCard
          title="Activity Logs"
          description="Live stream with level styling, source filters, and search."
          icon={Notification03Icon}
        >
          <div className="mb-2 flex flex-wrap items-center justify-between gap-2 text-xs tabular-nums">
            <span
              className={cn(
                'inline-flex items-center gap-1 rounded-full border px-2 py-0.5',
                isActivityConnected
                  ? 'border-emerald-200 bg-emerald-100/70 text-emerald-700'
                  : 'border-red-200 bg-red-100/70 text-red-700',
              )}
            >
              <span
                className={cn(
                  'size-1.5 rounded-full',
                  isActivityConnected ? 'bg-emerald-500' : 'bg-red-500',
                )}
              />
              {isActivityConnected ? 'Live stream' : 'Stream disconnected'}
            </span>
            <span className="text-primary-600">
              {recentIssueCount} issue(s) in last 5 minutes
            </span>
          </div>

          <div className="mb-2 flex flex-wrap items-center gap-1.5">
            {ACTIVITY_LOG_FILTERS.map(function renderFilter(filterOption) {
              const selected = selectedLogFilter === filterOption.id
              return (
                <Button
                  key={filterOption.id}
                  size="sm"
                  variant={selected ? 'default' : 'outline'}
                  className="h-7 px-2 text-xs tabular-nums"
                  onClick={function onSelectFilter() {
                    setSelectedLogFilter(filterOption.id)
                  }}
                  aria-pressed={selected}
                >
                  {filterOption.label}
                </Button>
              )
            })}
          </div>

          <div className="mb-2 flex flex-wrap items-center gap-2">
            <Input
              type="search"
              size="sm"
              value={logSearch}
              onChange={function onSearchChange(event) {
                setLogSearch(event.target.value)
              }}
              placeholder="Search logs"
              className="w-full max-w-md tabular-nums"
              aria-label="Search logs"
            />
            <Button
              size="sm"
              variant={isAutoScrollPinned ? 'default' : 'outline'}
              className="h-7 px-2 text-xs tabular-nums"
              onClick={handleToggleAutoScroll}
              aria-pressed={isAutoScrollPinned}
            >
              {isAutoScrollPinned ? 'Auto-scroll on' : 'Auto-scroll off'}
            </Button>
            <span className="text-[11px] text-primary-600 tabular-nums">
              {filteredLogEvents.length} log(s)
            </span>
          </div>

          {isEventsLoading && events.length === 0 ? (
            <div className="rounded-xl border border-primary-200 bg-primary-100/50 px-3 py-4 text-sm text-primary-600 text-pretty">
              Loading log events…
            </div>
          ) : filteredLogEvents.length === 0 ? (
            <div className="rounded-xl border border-primary-200 bg-primary-100/50 px-3 py-4 text-sm text-primary-600 text-pretty">
              No log events match the current filter and search.
            </div>
          ) : (
            <div
              ref={logViewportRef}
              className="max-h-[360px] space-y-1.5 overflow-y-auto pr-1"
            >
              {filteredLogEvents.map(function renderFilteredLogEvent(event) {
                return <ActivityEventRow key={event.id} event={event} />
              })}
            </div>
          )}
        </DashboardGlassCard>

        <DashboardGlassCard
          title="LLM Troubleshooter (Safe Mode)"
          description="Read-only pattern matcher that recommends next steps from recent errors."
          icon={Wrench01Icon}
        >
          <div className="rounded-xl border border-amber-200 bg-amber-100/60 px-3 py-2 text-xs text-amber-800 text-pretty">
            ⚠️ Suggestions only — commands are not executed automatically
          </div>

          <div className="mt-3 space-y-2.5">
            {troubleshooterSuggestions.map(
              function renderSuggestion(suggestion, index) {
                const copied = copiedSuggestionId === suggestion.id
                return (
                  <article
                    key={`${suggestion.id}-${index}`}
                    className="rounded-xl border border-primary-200 bg-primary-100/50 px-3 py-2.5"
                  >
                    <p className="text-sm font-medium text-ink text-pretty">
                      {suggestion.suggestion}
                    </p>
                    <p className="mt-1 text-xs text-primary-600 text-pretty tabular-nums">
                      Triggered by: {suggestion.matchedTitle}
                    </p>
                    <p className="mt-0.5 text-xs text-primary-500 tabular-nums">
                      {formatEventTimestamp(suggestion.matchedAt)}
                    </p>
                    <div className="mt-2.5 flex flex-wrap items-center gap-2">
                      <code className="rounded-md border border-primary-200 bg-primary-50 px-2 py-1 font-mono text-xs text-primary-900 tabular-nums">
                        {suggestion.command}
                      </code>
                      <Button
                        variant="outline"
                        size="sm"
                        className="h-7 px-2 text-xs tabular-nums"
                        onClick={function onCopyCommand() {
                          void handleCopyCommand(
                            suggestion.id,
                            suggestion.command,
                          )
                        }}
                      >
                        <HugeiconsIcon
                          icon={copied ? Tick02Icon : Copy01Icon}
                          size={20}
                          strokeWidth={1.5}
                        />
                        {copied ? 'Copied' : 'Copy'}
                      </Button>
                    </div>
                  </article>
                )
              },
            )}
          </div>

          <div className="mt-3">
            <a
              href="https://docs.openclaw.ai"
              target="_blank"
              rel="noreferrer noopener"
              className="inline-flex items-center gap-1 rounded-md border border-primary-200 bg-primary-100/60 px-2.5 py-1.5 text-xs text-primary-700 tabular-nums transition-colors hover:bg-primary-200"
            >
              <HugeiconsIcon icon={Link01Icon} size={20} strokeWidth={1.5} />
              OpenClaw docs
            </a>
          </div>
        </DashboardGlassCard>

        <DashboardGlassCard
          title="Export Diagnostics"
          description="Generate a safe, redacted bundle for troubleshooting and GitHub issues."
          icon={PackageIcon}
        >
          <div className="rounded-xl border border-amber-200 bg-amber-100/60 px-3 py-2 text-xs text-amber-800 text-pretty">
            ⚠️ Never share secrets. This bundle is automatically redacted, but
            always review before sharing.
          </div>

          <div className="mt-3 space-y-3">
            <div className="rounded-xl border border-primary-200 bg-primary-100/50 px-3 py-3">
              <p className="text-sm font-medium text-ink">What's included:</p>
              <ul className="mt-2 space-y-1 text-xs text-primary-600">
                <li>• App version, OS, and Node version</li>
                <li>• Gateway connection status and URL (tokens redacted)</li>
                <li>• Workspace folder name only (not full path)</li>
                <li>• Last 50 activity events (sensitive data redacted)</li>
                <li>• Debug console entries (redacted)</li>
                <li>• Enabled providers by name only (no API keys)</li>
              </ul>
            </div>

            <div className="flex flex-wrap items-center gap-2">
              <Button
                variant="default"
                size="sm"
                onClick={handleExportDiagnostics}
                disabled={isExporting}
              >
                <HugeiconsIcon
                  icon={Download04Icon}
                  size={20}
                  strokeWidth={1.5}
                />
                {isExporting ? 'Exporting…' : 'Export Diagnostics'}
              </Button>

              <Button variant="outline" size="sm" onClick={handleOpenIssue}>
                <HugeiconsIcon
                  icon={Github01Icon}
                  size={20}
                  strokeWidth={1.5}
                />
                Open Issue on GitHub
              </Button>
            </div>

            {exportError ? (
              <p className="text-xs text-red-600 text-pretty">{exportError}</p>
            ) : null}
          </div>
        </DashboardGlassCard>
      </div>
    </main>
  )
}
