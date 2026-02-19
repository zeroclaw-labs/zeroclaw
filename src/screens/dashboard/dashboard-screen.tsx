import {
  Activity01Icon,
  ArrowDown01Icon,
  ArrowUp02Icon,
  ChartLineData02Icon,
  Moon02Icon,
  PencilEdit02Icon,
  RefreshIcon,
  Settings01Icon,
  Sun02Icon,
  Timer02Icon,
  UserGroupIcon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import {
  type MouseEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from 'react'
import { AgentStatusWidget } from './components/agent-status-widget'
import { ActivityLogWidget } from './components/activity-log-widget'
import { CollapsibleWidget } from './components/collapsible-widget'
import { MetricsWidget } from './components/metrics-widget'
import { NowCard } from './components/now-card'
import { NotificationsWidget } from './components/notifications-widget'
import { RecentSessionsWidget } from './components/recent-sessions-widget'
import { SkillsWidget, fetchInstalledSkills } from './components/skills-widget'
// SystemInfoWidget removed — not useful enough for dashboard real estate
import { TasksWidget } from './components/tasks-widget'
import { UsageMeterWidget, fetchUsage } from './components/usage-meter-widget'
import { AddWidgetPopover } from './components/add-widget-popover'
import { WidgetGrid, type WidgetGridItem } from './components/widget-grid'
import { ActivityTicker } from '@/components/activity-ticker'
import { HeaderAmbientStatus } from './components/header-ambient-status'
import { NotificationsPopover } from './components/notifications-popover'
import { useVisibleWidgets } from './hooks/use-visible-widgets'
import { OpenClawStudioIcon } from '@/components/icons/clawsuite'
import { ThemeToggle } from '@/components/theme-toggle'
import { SettingsDialog } from '@/components/settings-dialog'
import { DashboardOverflowPanel } from '@/components/dashboard-overflow-panel'
import {
  chatQueryKeys,
  fetchGatewayStatus,
  fetchSessions,
} from '@/screens/chat/chat-queries'
import { fetchCronJobs } from '@/lib/cron-api'
import { cn } from '@/lib/utils'
import { toast } from '@/components/ui/toast'
import { useSettingsStore } from '@/hooks/use-settings'
import {
  type DashboardWidgetOrderId,
  useWidgetReorder,
} from '@/hooks/use-widget-reorder'

type SessionStatusPayload = {
  ok?: boolean
  payload?: {
    model?: string
    currentModel?: string
    modelAlias?: string
    sessions?: {
      defaults?: { model?: string; contextTokens?: number }
      count?: number
      recent?: Array<{ age?: number; model?: string; percentUsed?: number }>
    }
  }
}

type DashboardCostSummaryPayload = {
  ok?: boolean
  cost?: {
    timeseries?: Array<{
      date?: string
      amount?: number | string
    }>
  }
}

type MobileWidgetSection = {
  id: DashboardWidgetOrderId
  label: string
  content: ReactNode
}

type DashboardSignalChip = {
  id: string
  text: string
  severity: 'amber' | 'red'
}

// Pull-to-refresh constants removed

function readNumeric(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string') {
    const parsed = Number(value)
    if (Number.isFinite(parsed)) return parsed
  }
  return 0
}

function toLocalDateKey(date: Date): string {
  const year = date.getFullYear()
  const month = `${date.getMonth() + 1}`.padStart(2, '0')
  const day = `${date.getDate()}`.padStart(2, '0')
  return `${year}-${month}-${day}`
}

function formatCurrency(amount: number): string {
  return new Intl.NumberFormat(undefined, {
    style: 'currency',
    currency: 'USD',
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(amount)
}

function formatTokenCount(amount: number): string {
  return new Intl.NumberFormat().format(Math.max(0, Math.round(amount)))
}

function formatUptime(seconds: number): string {
  if (seconds <= 0) return '—'
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${minutes}m`
  return `${minutes}m`
}

function normalizeTimestamp(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value > 1_000_000_000_000 ? value : value * 1000
  }
  if (typeof value === 'string') {
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return parsed
    const asNumber = Number(value)
    if (Number.isFinite(asNumber)) {
      return asNumber > 1_000_000_000_000 ? asNumber : asNumber * 1000
    }
  }
  return 0
}

function toSessionDisplayName(session: Record<string, unknown>): string {
  const label =
    typeof session.label === 'string' && session.label.trim().length > 0
      ? session.label.trim()
      : ''
  if (label) return label

  const derived =
    typeof session.derivedTitle === 'string' &&
    session.derivedTitle.trim().length > 0
      ? session.derivedTitle.trim()
      : ''
  if (derived) return derived

  const title =
    typeof session.title === 'string' && session.title.trim().length > 0
      ? session.title.trim()
      : ''
  if (title) return title

  const friendlyId =
    typeof session.friendlyId === 'string' && session.friendlyId.trim().length > 0
      ? session.friendlyId.trim()
      : 'main'
  return friendlyId === 'main' ? 'Main Session' : friendlyId
}

function toTaskSummaryStatus(
  job: Awaited<ReturnType<typeof fetchCronJobs>>[number],
): 'backlog' | 'in_progress' | 'review' | 'done' {
  if (!job.enabled) return 'backlog'
  const status = job.lastRun?.status
  if (status === 'running' || status === 'queued') return 'in_progress'
  if (status === 'error') return 'review'
  if (status === 'success') return 'done'
  return 'backlog'
}

async function fetchCostTimeseries(): Promise<
  Array<{ date: string; amount: number }>
> {
  const response = await fetch('/api/cost')
  if (!response.ok) throw new Error('Unable to load cost summary')
  const payload = (await response.json()) as DashboardCostSummaryPayload
  if (!payload.ok || !payload.cost) throw new Error('Unable to load cost summary')

  const rows = Array.isArray(payload.cost.timeseries) ? payload.cost.timeseries : []
  return rows
    .map(function mapCostPoint(point) {
      return {
        date: typeof point.date === 'string' ? point.date : '',
        amount: readNumeric(point.amount),
      }
    })
    .filter(function hasDate(point) {
      return point.date.length > 0
    })
}

async function fetchSessionStatus(): Promise<SessionStatusPayload> {
  try {
    const response = await fetch('/api/session-status')
    if (!response.ok) {
      toast('Failed to fetch session status', { type: 'error' })
      return {}
    }
    return response.json() as Promise<SessionStatusPayload>
  } catch (err) {
    toast('Failed to fetch session status', { type: 'error' })
    return {}
  }
}

async function fetchHeroCost(): Promise<string> {
  try {
    const response = await fetch('/api/cost')
    if (!response.ok) {
      toast('Failed to fetch cost data', { type: 'error' })
      return '—'
    }
    const data = (await response.json()) as Record<string, unknown>
    const cost = data.cost as Record<string, unknown> | undefined
    const total = cost?.total as Record<string, unknown> | undefined
    const amount = total?.amount
    if (typeof amount === 'number') return `$${amount.toFixed(2)}`
    if (typeof amount === 'string') return `$${amount}`
    return '—'
  } catch {
    toast('Failed to fetch cost data', { type: 'error' })
    return '—'
  }
}

function formatModelName(raw: string): string {
  if (!raw) return '—'
  // claude-opus-4-6 → Opus 4.6, claude-sonnet-4-5 → Sonnet 4.5, gpt-5.2-codex → GPT-5.2 Codex
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
  if (lower.includes('gemini')) return raw.split('/').pop() ?? raw
  return raw
}

// Removed mockSystemStatus - now built entirely from real API data

export function DashboardScreen() {
  const navigate = useNavigate()
  const [dashSettingsOpen, setDashSettingsOpen] = useState(false)
  const [overflowOpen, setOverflowOpen] = useState(false)
  const { visibleIds, addWidget, removeWidget, resetVisible } =
    useVisibleWidgets()
  const { order: widgetOrder, moveWidget, resetOrder } = useWidgetReorder()
  const theme = useSettingsStore((state) => state.settings.theme)
  const updateSettings = useSettingsStore((state) => state.updateSettings)
  const [isMobile, setIsMobile] = useState(false)
  const [mobileEditMode, setMobileEditMode] = useState(false)
  const [nowMs, setNowMs] = useState(() => Date.now())
  const [showLogoTip, setShowLogoTip] = useState(() => {
    if (typeof window === 'undefined') return false
    try {
      return localStorage.getItem('clawsuite-logo-tip-seen') !== 'true'
    } catch {
      return false
    }
  })
  // Pull-to-refresh removed (was buggy on mobile)

  useEffect(() => {
    const media = window.matchMedia('(max-width: 767px)')
    const update = () => setIsMobile(media.matches)
    update()
    media.addEventListener('change', update)
    return () => media.removeEventListener('change', update)
  }, [])

  useEffect(() => {
    const interval = window.setInterval(() => setNowMs(Date.now()), 60_000)
    return () => window.clearInterval(interval)
  }, [])

  useEffect(() => {
    if (!isMobile || !showLogoTip) return
    const timeout = window.setTimeout(() => {
      setShowLogoTip(false)
      try {
        localStorage.setItem('clawsuite-logo-tip-seen', 'true')
      } catch {}
    }, 4_000)
    return () => window.clearTimeout(timeout)
  }, [isMobile, showLogoTip])

  const handleResetLayout = useCallback(() => {
    resetVisible()
    resetOrder()
    setMobileEditMode(false)
  }, [resetOrder, resetVisible])

  const sessionsQuery = useQuery({
    queryKey: chatQueryKeys.sessions,
    queryFn: fetchSessions,
    refetchInterval: 30_000,
  })

  const gatewayStatusQuery = useQuery({
    queryKey: ['gateway', 'dashboard-status'],
    queryFn: fetchGatewayStatus,
    retry: false,
    refetchInterval: 15_000,
  })

  const sessionStatusQuery = useQuery({
    queryKey: ['gateway', 'session-status'],
    queryFn: fetchSessionStatus,
    retry: false,
    refetchInterval: 30_000,
  })

  const heroCostQuery = useQuery({
    queryKey: ['dashboard', 'hero-cost'],
    queryFn: fetchHeroCost,
    staleTime: 60_000,
    refetchInterval: 60_000,
  })

  const cronJobsQuery = useQuery({
    queryKey: ['cron', 'jobs'],
    queryFn: fetchCronJobs,
    retry: false,
    refetchInterval: 30_000,
  })

  const skillsSummaryQuery = useQuery({
    queryKey: ['dashboard', 'skills'],
    queryFn: fetchInstalledSkills,
    staleTime: 60_000,
    refetchInterval: 60_000,
  })

  const usageSummaryQuery = useQuery({
    queryKey: ['dashboard', 'usage'],
    queryFn: fetchUsage,
    retry: false,
    refetchInterval: 30_000,
  })

  const costTimeseriesQuery = useQuery({
    queryKey: ['dashboard', 'cost-timeseries'],
    queryFn: fetchCostTimeseries,
    retry: false,
    staleTime: 60_000,
    refetchInterval: 60_000,
  })

  const systemStatus = useMemo(
    function buildSystemStatus() {
      const nowIso = new Date().toISOString()
      const sessions = Array.isArray(sessionsQuery.data)
        ? sessionsQuery.data
        : []
      const ssPayload = sessionStatusQuery.data?.payload?.sessions

      // Get active model from main session, fall back to gateway default
      const mainSessionModel = ssPayload?.recent?.[0]?.model ?? ''
      const payloadModel = sessionStatusQuery.data?.payload?.model ?? ''
      const payloadCurrentModel =
        sessionStatusQuery.data?.payload?.currentModel ?? ''
      const payloadAlias = sessionStatusQuery.data?.payload?.modelAlias ?? ''
      const rawModel =
        mainSessionModel ||
        payloadModel ||
        payloadCurrentModel ||
        payloadAlias ||
        ssPayload?.defaults?.model ||
        ''
      const currentModel = formatModelName(rawModel)

      // Derive uptime from main session age (milliseconds → seconds)
      const mainSession = ssPayload?.recent?.[0]
      const uptimeSeconds = mainSession?.age
        ? Math.floor(mainSession.age / 1000)
        : 0

      const totalSessions = ssPayload?.count ?? sessions.length
      const activeAgents = ssPayload?.recent?.length ?? sessions.length

      return {
        gateway: {
          connected: gatewayStatusQuery.data?.ok ?? !gatewayStatusQuery.isError,
          checkedAtIso: nowIso,
        },
        uptimeSeconds,
        currentModel,
        totalSessions,
        activeAgents,
      }
    },
    [gatewayStatusQuery.data?.ok, sessionsQuery.data, sessionStatusQuery.data],
  )

  const taskSummary = useMemo(
    function buildTaskSummary() {
      const jobs = Array.isArray(cronJobsQuery.data) ? cronJobsQuery.data : []
      const counts = {
        backlog: 0,
        inProgress: 0,
        done: 0,
      }

      for (const job of jobs) {
        const status = toTaskSummaryStatus(job)
        if (status === 'backlog') counts.backlog += 1
        if (status === 'in_progress') counts.inProgress += 1
        if (status === 'done') counts.done += 1
      }

      return counts
    },
    [cronJobsQuery.data],
  )

  const enabledSkillsCount = useMemo(
    function countEnabledSkills() {
      const skills = Array.isArray(skillsSummaryQuery.data)
        ? skillsSummaryQuery.data
        : []
      return skills.filter((skill) => skill.enabled).length
    },
    [skillsSummaryQuery.data],
  )

  const usageSummary = useMemo(
    function buildUsageSummary() {
      const usage = usageSummaryQuery.data
      if (usageSummaryQuery.isError || usage?.kind === 'error' || usage?.kind === 'unavailable') {
        return {
          state: 'error' as const,
          text: 'Usage unavailable',
          tokensToday: 0,
          todayCost: 0,
        }
      }

      if (!usage || usage.kind !== 'ok') {
        return {
          state: 'loading' as const,
          text: 'Usage: loading…',
          tokensToday: 0,
          todayCost: 0,
        }
      }

      const tokensToday = usage.data.totalUsage
      const todayDateKey = toLocalDateKey(new Date(nowMs))
      const timeseries = Array.isArray(costTimeseriesQuery.data)
        ? costTimeseriesQuery.data
        : []
      const todayPoint =
        timeseries.find((point) => point.date.startsWith(todayDateKey)) ??
        timeseries[timeseries.length - 1]
      const todayCost = todayPoint ? Math.max(0, todayPoint.amount) : 0

      return {
        state: 'ok' as const,
        text: `Usage: ${formatCurrency(todayCost)} today • ${formatTokenCount(tokensToday)} tokens`,
        tokensToday,
        todayCost,
      }
    },
    [costTimeseriesQuery.data, nowMs, usageSummaryQuery.data, usageSummaryQuery.isError],
  )

  const stalledAgentName = useMemo(
    function findStalledAgentName() {
      const sessions = Array.isArray(sessionsQuery.data) ? sessionsQuery.data : []
      if (sessions.length === 0) return null

      const now = Date.now()
      const stalled = sessions
        .map(function mapSession(session) {
          const raw = session as Record<string, unknown>
          const updatedAt = normalizeTimestamp(raw.updatedAt)
          if (updatedAt <= 0) return null

          const staleForMs = now - updatedAt
          if (staleForMs <= 30 * 60 * 1000) return null

          return {
            name: toSessionDisplayName(raw),
            staleForMs,
          }
        })
        .filter((entry): entry is { name: string; staleForMs: number } => entry !== null)
        .sort((left, right) => right.staleForMs - left.staleForMs)

      return stalled[0]?.name ?? null
    },
    [sessionsQuery.data],
  )

  const contextUsagePercent = useMemo(
    function readContextUsagePercent() {
      const usage = usageSummaryQuery.data
      if (!usage || usage.kind !== 'ok') return 0
      const percent = Math.round(usage.data.usagePercent ?? 0)
      return Math.max(0, percent)
    },
    [usageSummaryQuery.data],
  )

  const dashboardSignalChips = useMemo<Array<DashboardSignalChip>>(
    function buildDashboardSignalChips() {
      const chips: Array<DashboardSignalChip> = []

      if (usageSummary.state === 'ok' && usageSummary.todayCost > 50) {
        chips.push({
          id: 'high-spend',
          text: `⚠ High spend today: ${formatCurrency(usageSummary.todayCost)}`,
          severity: 'amber',
        })
      }

      if (stalledAgentName) {
        chips.push({
          id: 'stalled-agent',
          text: `⚠ Agent stalled: ${stalledAgentName}`,
          severity: 'red',
        })
      }

      if (contextUsagePercent >= 75) {
        chips.push({
          id: 'context-pressure',
          text: `Memory pressure: ${contextUsagePercent}%`,
          severity: 'amber',
        })
      }

      return chips
    },
    [contextUsagePercent, stalledAgentName, usageSummary.state, usageSummary.todayCost],
  )

  const nextTheme = useMemo(
    () => (theme === 'light' ? 'dark' : theme === 'dark' ? 'system' : 'light'),
    [theme],
  )
  const mobileThemeIsDark =
    theme === 'dark' ||
    (theme === 'system' &&
      typeof document !== 'undefined' &&
      document.documentElement.classList.contains('dark'))
  const mobileThemeIcon = mobileThemeIsDark ? Moon02Icon : Sun02Icon

  const markLogoTipSeen = useCallback(function markLogoTipSeen() {
    setShowLogoTip(false)
    try {
      localStorage.setItem('clawsuite-logo-tip-seen', 'true')
    } catch {}
  }, [])
  const shouldShowLogoTip = isMobile && showLogoTip

  const handleLogoTap = useCallback(function handleLogoTap() {
    markLogoTipSeen()
    setOverflowOpen(true)
  }, [markLogoTipSeen])

  const retryUsageSummary = useCallback(
    function retryUsageSummary(event?: MouseEvent<HTMLButtonElement>) {
      event?.stopPropagation()
      void Promise.allSettled([usageSummaryQuery.refetch(), costTimeseriesQuery.refetch()])
    },
    [costTimeseriesQuery, usageSummaryQuery],
  )

  const visibleWidgetSet = useMemo(() => {
    return new Set(visibleIds)
  }, [visibleIds])

  const retryHeroCost = useCallback(() => {
    void heroCostQuery.refetch()
  }, [heroCostQuery])

  const costTrendPct = useMemo(() => {
    const points = Array.isArray(costTimeseriesQuery.data)
      ? [...costTimeseriesQuery.data]
      : []
    if (points.length < 2) return undefined

    points.sort((left, right) => left.date.localeCompare(right.date))
    const latest = points[points.length - 1]
    const previous = points[points.length - 2]
    if (!latest || !previous) return undefined
    if (previous.amount <= 0) return undefined
    return ((latest.amount - previous.amount) / previous.amount) * 100
  }, [costTimeseriesQuery.data])

  const latestCostAmount = useMemo(() => {
    const points = Array.isArray(costTimeseriesQuery.data)
      ? [...costTimeseriesQuery.data]
      : []
    if (points.length === 0) return null

    points.sort((left, right) => left.date.localeCompare(right.date))
    const latest = points[points.length - 1]
    if (!latest) return null
    return Math.max(0, latest.amount)
  }, [costTimeseriesQuery.data])

  const metricItems = useMemo<Array<WidgetGridItem>>(
    function buildMetricItems() {
      return [
        {
          id: 'metric-sessions',
          size: 'small',
          node: (
            <MetricsWidget
              title="Sessions"
              value={systemStatus.totalSessions}
              subtitle="Total sessions"
              icon={Activity01Icon}
              accent="cyan"
              description="Total sessions observed by the gateway."
              rawValue={`${systemStatus.totalSessions} sessions`}
            />
          ),
        },
        {
          id: 'metric-agents',
          size: 'small',
          node: (
            <MetricsWidget
              title="Active Agents"
              value={systemStatus.activeAgents}
              subtitle="Currently active"
              icon={UserGroupIcon}
              accent="orange"
              description="Agents currently running or processing work."
              rawValue={`${systemStatus.activeAgents} active agents`}
            />
          ),
        },
        {
          id: 'metric-cost',
          size: 'small',
          node: (
            <MetricsWidget
              title="Cost"
              value={heroCostQuery.isError ? '—' : (heroCostQuery.data ?? '—')}
              subtitle="Billing period"
              icon={ChartLineData02Icon}
              accent="emerald"
              isError={heroCostQuery.isError}
              onRetry={retryHeroCost}
              trendPct={heroCostQuery.isError ? undefined : costTrendPct}
              trendLabel={costTrendPct === undefined ? undefined : 'vs prev day'}
              description="Estimated spend from gateway cost telemetry."
              rawValue={
                latestCostAmount === null
                  ? heroCostQuery.data ?? 'Unavailable'
                  : formatCurrency(latestCostAmount)
              }
            />
          ),
        },
        {
          id: 'metric-uptime',
          size: 'small',
          node: (
            <MetricsWidget
              title="Uptime"
              value={formatUptime(systemStatus.uptimeSeconds)}
              subtitle="Gateway runtime"
              icon={Timer02Icon}
              accent="violet"
              description="Time since the active gateway session started."
              rawValue={`${systemStatus.uptimeSeconds}s`}
            />
          ),
        },
      ]
    },
    [
      heroCostQuery.data,
      heroCostQuery.isError,
      latestCostAmount,
      retryHeroCost,
      costTrendPct,
      systemStatus.activeAgents,
      systemStatus.totalSessions,
      systemStatus.uptimeSeconds,
    ],
  )

  const desktopWidgetItems = useMemo<Array<WidgetGridItem>>(
    function buildDesktopWidgetItems() {
      const items: Array<WidgetGridItem> = []

      for (const widgetId of visibleIds) {
        if (widgetId === 'skills') {
          items.push({
            id: widgetId,
            size: 'medium',
            node: <SkillsWidget onRemove={() => removeWidget('skills')} />,
          })
          continue
        }

        if (widgetId === 'usage-meter') {
          items.push({
            id: widgetId,
            size: 'medium',
            node: <UsageMeterWidget onRemove={() => removeWidget('usage-meter')} />,
          })
          continue
        }

        if (widgetId === 'tasks') {
          items.push({
            id: widgetId,
            size: 'medium',
            node: <TasksWidget onRemove={() => removeWidget('tasks')} />,
          })
          continue
        }

        if (widgetId === 'agent-status') {
          items.push({
            id: widgetId,
            size: 'medium',
            node: <AgentStatusWidget onRemove={() => removeWidget('agent-status')} />,
          })
          continue
        }

        if (widgetId === 'recent-sessions') {
          items.push({
            id: widgetId,
            size: 'medium',
            node: (
              <RecentSessionsWidget
                onOpenSession={(sessionKey) =>
                  navigate({
                    to: '/chat/$sessionKey',
                    params: { sessionKey },
                  })
                }
                onRemove={() => removeWidget('recent-sessions')}
              />
            ),
          })
          continue
        }

        if (widgetId === 'notifications') {
          items.push({
            id: widgetId,
            size: 'medium',
            node: <NotificationsWidget onRemove={() => removeWidget('notifications')} />,
          })
          continue
        }

        if (widgetId === 'activity-log') {
          items.push({
            id: widgetId,
            size: 'large',
            node: <ActivityLogWidget onRemove={() => removeWidget('activity-log')} />,
          })
        }
      }

      return items
    },
    [navigate, removeWidget, visibleIds],
  )

  const mobileDeepSections = useMemo<Array<MobileWidgetSection>>(
    function buildMobileDeepSections() {
      const sections: Array<MobileWidgetSection> = []
      const deepTierOrder = widgetOrder.filter((id) =>
        ['activity', 'agents', 'sessions', 'tasks', 'skills', 'usage'].includes(id),
      )

      for (const widgetId of deepTierOrder) {
        if (widgetId === 'activity') {
          if (!visibleWidgetSet.has('activity-log')) continue
          sections.push({
            id: widgetId,
            label: 'Activity',
            content: (
              <div className="w-full">
                <ActivityLogWidget onRemove={() => removeWidget('activity-log')} />
              </div>
            ),
          })
          continue
        }

        if (widgetId === 'agents') {
          if (!visibleWidgetSet.has('agent-status')) continue
          sections.push({
            id: widgetId,
            label: 'Agents',
            content: (
              <div className="w-full">
                <AgentStatusWidget onRemove={() => removeWidget('agent-status')} />
              </div>
            ),
          })
          continue
        }

        if (widgetId === 'sessions') {
          if (!visibleWidgetSet.has('recent-sessions')) continue
          sections.push({
            id: widgetId,
            label: 'Sessions',
            content: (
              <div className="w-full">
                <RecentSessionsWidget
                  onOpenSession={(sessionKey) =>
                    navigate({
                      to: '/chat/$sessionKey',
                      params: { sessionKey },
                    })
                  }
                  onRemove={() => removeWidget('recent-sessions')}
                />
              </div>
            ),
          })
          continue
        }

        if (widgetId === 'tasks') {
          if (!visibleWidgetSet.has('tasks')) continue
          sections.push({
            id: widgetId,
            label: 'Tasks',
            content: (
              <div className="w-full">
                <CollapsibleWidget
                  title="Tasks"
                  summary={`Tasks: ${taskSummary.inProgress} in progress • ${taskSummary.done} done`}
                  defaultOpen
                >
                  <TasksWidget onRemove={() => removeWidget('tasks')} />
                </CollapsibleWidget>
              </div>
            ),
          })
          continue
        }

        if (widgetId === 'skills') {
          if (!visibleWidgetSet.has('skills')) continue
          sections.push({
            id: widgetId,
            label: 'Skills',
            content: (
              <div className="w-full">
                <CollapsibleWidget
                  title="Skills"
                  summary={`Skills: ${enabledSkillsCount} enabled`}
                  defaultOpen={false}
                >
                  <SkillsWidget onRemove={() => removeWidget('skills')} />
                </CollapsibleWidget>
              </div>
            ),
          })
          continue
        }

        if (widgetId === 'usage') {
          if (!visibleWidgetSet.has('usage-meter')) continue
          sections.push({
            id: widgetId,
            label: 'Usage',
            content: (
              <div className="w-full">
                <CollapsibleWidget
                  title="Usage Meter"
                  summary={usageSummary.text}
                  defaultOpen={false}
                  action={
                    usageSummary.state === 'error' ? (
                      <button
                        type="button"
                        onClick={retryUsageSummary}
                        className="rounded-md border border-red-200 bg-red-50/80 px-1.5 py-0.5 text-[10px] font-medium text-red-700 transition-colors hover:bg-red-100"
                      >
                        Retry
                      </button>
                    ) : null
                  }
                >
                  {usageSummary.state === 'error' ? (
                    <div className="rounded-lg border border-red-200 bg-red-50/80 px-3 py-2 text-sm text-red-700">
                      <p className="font-medium">Usage unavailable</p>
                      <button
                        type="button"
                        onClick={retryUsageSummary}
                        className="mt-2 rounded-md border border-red-200 bg-red-100/80 px-2 py-1 text-xs font-medium transition-colors hover:bg-red-100"
                      >
                        Retry
                      </button>
                    </div>
                  ) : (
                    <UsageMeterWidget onRemove={() => removeWidget('usage-meter')} />
                  )}
                </CollapsibleWidget>
              </div>
            ),
          })
        }
      }

      return sections
    },
    [
      enabledSkillsCount,
      navigate,
      removeWidget,
      retryUsageSummary,
      taskSummary.done,
      taskSummary.inProgress,
      usageSummary.state,
      usageSummary.text,
      visibleWidgetSet,
      widgetOrder,
    ],
  )

  const moveMobileSection = useCallback(
    (fromVisibleIndex: number, toVisibleIndex: number) => {
      const fromSection = mobileDeepSections[fromVisibleIndex]
      const toSection = mobileDeepSections[toVisibleIndex]
      if (!fromSection || !toSection || fromSection.id === toSection.id) return

      const fromOrderIndex = widgetOrder.indexOf(fromSection.id)
      const toOrderIndex = widgetOrder.indexOf(toSection.id)
      if (fromOrderIndex === -1 || toOrderIndex === -1) return

      moveWidget(fromOrderIndex, toOrderIndex)
    },
    [mobileDeepSections, moveWidget, widgetOrder],
  )

  return (
    <>
      <main
        className="h-full overflow-x-hidden overflow-y-auto bg-primary-100/45 px-4 pt-3 pb-[calc(env(safe-area-inset-bottom)+6rem)] text-primary-900 md:px-6 md:pt-8 md:pb-8"
      >
        <section className="mx-auto w-full max-w-[1600px]">
          <header className="relative z-20 mb-3 rounded-xl border border-primary-200 bg-primary-50/95 px-3 py-2 shadow-sm md:mb-5 md:px-5 md:py-3">
            <div className="flex items-center justify-between gap-3">
              {/* Left: Logo + name + status */}
              <div className="flex min-w-0 items-center gap-2.5">
                {isMobile ? (
                  <button
                    type="button"
                    onClick={handleLogoTap}
                    className="shrink-0 cursor-pointer rounded-xl transition-transform active:scale-95"
                    aria-label="Open quick menu"
                  >
                    <OpenClawStudioIcon className="size-8 rounded-xl shadow-sm" />
                    {shouldShowLogoTip ? (
                      <div className="absolute !left-1/2 top-full z-30 mt-2 -translate-x-1/2 animate-in fade-in-0 slide-in-from-top-1 duratrion-300">
                        <div className="relative rounded bg-primary-900 px-2 py-1 text-xs font-medium text-white shadow-md ">
                          <span
                            role="button"
                            tabIndex={0}
                            className="whitespace-nowrap cursor-pointer"
                            onClick={(e) => { e.stopPropagation(); markLogoTipSeen(); }}
                            onKeyDown={(e) => { if (e.key === 'Enter') markLogoTipSeen(); }}
                            aria-label="Dismiss quick menu tip"
                          >
                            Tap for quick menu
                          </span>
                          <div className="absolute left-1/2 top-0 size-2 -translate-x-1/2 -translate-y-1/2 rotate-45 bg-primary-900 shadow-md" />
                        </div>
                      </div>
                    ) : null}
                  </button>
                ) : (
                  <OpenClawStudioIcon className="size-8 shrink-0 rounded-xl shadow-sm" />
                )}
                <div className="flex min-w-0 items-center gap-2">
                  <h1 className="text-sm font-semibold text-ink text-balance md:text-base truncate">
                    ClawSuite
                  </h1>
                  {isMobile ? (
                    /* Mobile: simple status dot — tooltip via title */
                    <span
                      className={cn(
                        'size-2 shrink-0 rounded-full',
                        systemStatus.gateway.connected
                          ? 'bg-emerald-500'
                          : 'bg-red-500',
                      )}
                      title={systemStatus.gateway.connected ? 'Connected' : 'Disconnected'}
                    />
                  ) : (
                    <span
                      className={cn(
                        'inline-flex items-center gap-1.5 rounded-full border px-2 py-0.5 text-[11px] font-medium',
                        systemStatus.gateway.connected
                          ? 'border-emerald-200 bg-emerald-100/70 text-emerald-700'
                          : 'border-red-200 bg-red-100/80 text-red-700',
                      )}
                    >
                      <span
                        className={cn(
                          'size-1.5 shrink-0 rounded-full',
                          systemStatus.gateway.connected
                            ? 'bg-emerald-500'
                            : 'bg-red-500',
                        )}
                      />
                      {systemStatus.gateway.connected ? 'Connected' : 'Disconnected'}
                    </span>
                  )}
                </div>
              </div>

              {/* Right controls */}
              <div className="ml-auto flex items-center gap-2">
                {!isMobile && <HeaderAmbientStatus />}
                {!isMobile && <ThemeToggle />}
                {!isMobile && (
                  <div className="flex items-center gap-1 rounded-full border border-primary-200 bg-primary-100/65 p-1">
                    <NotificationsPopover />
                    <button
                      type="button"
                      onClick={() => setDashSettingsOpen(true)}
                      className="inline-flex size-7 items-center justify-center rounded-full text-primary-600 dark:text-primary-400 transition-colors hover:bg-primary-50 dark:hover:bg-gray-800 hover:text-accent-600 dark:hover:text-accent-400"
                      aria-label="Settings"
                      title="Settings"
                    >
                      <HugeiconsIcon
                        icon={Settings01Icon}
                        size={20}
                        strokeWidth={1.5}
                      />
                    </button>
                  </div>
                )}
                {isMobile && (
                  <>
                    {mobileEditMode ? (
                      <>
                        <AddWidgetPopover
                          visibleIds={visibleIds}
                          onAdd={addWidget}
                          compact
                          buttonClassName="size-8 !px-0 !py-0 justify-center rounded-full border border-primary-200 bg-primary-100/80 text-primary-500 shadow-sm"
                        />
                        <button
                          type="button"
                          onClick={handleResetLayout}
                          className="inline-flex size-8 items-center justify-center rounded-full border border-primary-200 bg-primary-100/80 text-primary-500 shadow-sm transition-colors hover:text-primary-700 active:scale-95"
                          aria-label="Reset Layout"
                          title="Reset Layout"
                        >
                          <HugeiconsIcon icon={RefreshIcon} size={14} strokeWidth={1.5} />
                        </button>
                      </>
                    ) : null}
                    <button
                      type="button"
                      onClick={() => setMobileEditMode((p) => !p)}
                      className={cn(
                        'inline-flex size-8 items-center justify-center rounded-full border shadow-sm transition-colors active:scale-95',
                        mobileEditMode
                          ? 'border-accent-300 bg-accent-50 text-accent-600'
                          : 'border-primary-200 bg-primary-100/80 text-primary-500 hover:text-primary-700',
                      )}
                      aria-label={mobileEditMode ? 'Done editing' : 'Edit layout'}
                      title={mobileEditMode ? 'Done editing' : 'Edit layout'}
                    >
                      <HugeiconsIcon icon={PencilEdit02Icon} size={14} strokeWidth={1.6} />
                    </button>
                    <button
                      type="button"
                      onClick={() => updateSettings({ theme: nextTheme })}
                      className="inline-flex size-8 items-center justify-center rounded-full border border-primary-200 bg-primary-100/80 text-primary-600 shadow-sm transition-colors hover:bg-primary-50 active:scale-95"
                      aria-label={`Switch theme to ${nextTheme}`}
                      title={`Theme: ${theme} (tap for ${nextTheme})`}
                    >
                      <HugeiconsIcon
                        icon={mobileThemeIcon}
                        size={16}
                        strokeWidth={1.6}
                      />
                    </button>
                    <button
                      type="button"
                      onClick={() => setDashSettingsOpen(true)}
                      className="inline-flex size-8 items-center justify-center rounded-full border border-primary-200 bg-primary-100/80 text-primary-600 shadow-sm transition-colors hover:bg-primary-50 active:scale-95"
                      aria-label="Dashboard settings"
                      title="Settings"
                    >
                      <HugeiconsIcon
                        icon={Settings01Icon}
                        size={16}
                        strokeWidth={1.5}
                      />
                    </button>
                  </>
                )}
              </div>
            </div>

          </header>

          {/* Activity ticker — keep full banner behavior on desktop */}
          <div className="hidden md:block">
            <ActivityTicker />
          </div>

          {!isMobile && dashboardSignalChips.length > 0 ? (
            <div className="mb-3 flex flex-wrap gap-2">
              {dashboardSignalChips.map((chip) => (
                <span
                  key={chip.id}
                  className={cn(
                    'inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium',
                    chip.severity === 'red'
                      ? 'border-red-200 bg-red-100/75 text-red-700'
                      : 'border-amber-200 bg-amber-100/75 text-amber-700',
                  )}
                >
                  {chip.text}
                </span>
              ))}
            </div>
          ) : null}

          {!isMobile ? <WidgetGrid items={metricItems} className="mb-3 md:mb-4" /> : null}

          {/* Inline widget controls — desktop only (mobile controls are in header) */}
          {!isMobile && (
            <div className="mb-3 flex items-center justify-end gap-2">
              <AddWidgetPopover visibleIds={visibleIds} onAdd={addWidget} />
              <button
                type="button"
                onClick={handleResetLayout}
                className="inline-flex items-center gap-1 rounded-lg border border-primary-200 bg-primary-50 px-2.5 py-1 text-[11px] text-primary-600 transition-colors hover:border-accent-200 hover:text-accent-600 dark:border-gray-700 dark:bg-gray-800 dark:text-primary-400 dark:hover:border-accent-600 dark:hover:text-accent-400"
                aria-label="Reset Layout"
                title="Reset Layout"
              >
                <HugeiconsIcon icon={RefreshIcon} size={20} strokeWidth={1.5} />
                <span>Reset</span>
              </button>
            </div>
          )}

          <div>
            {isMobile ? (
              <div className="flex flex-col gap-3">
                <div className="space-y-1.5">
                  <NowCard
                    gatewayConnected={systemStatus.gateway.connected}
                    activeAgents={systemStatus.activeAgents}
                    activeTasks={taskSummary.inProgress}
                  />
                </div>

                <div className="space-y-1.5">
                  {dashboardSignalChips.length > 0 ? (
                    <div className="flex flex-wrap gap-2">
                      {dashboardSignalChips.map((chip) => (
                        <span
                          key={chip.id}
                          className={cn(
                            'inline-flex items-center rounded-full border px-2.5 py-1 text-xs font-medium',
                            chip.severity === 'red'
                              ? 'border-red-200 bg-red-100/75 text-red-700'
                              : 'border-amber-200 bg-amber-100/75 text-amber-700',
                          )}
                        >
                          {chip.text}
                        </span>
                      ))}
                    </div>
                  ) : null}
                  <WidgetGrid items={metricItems} className="gap-3" />
                </div>

                <div className="space-y-1.5">
                  {mobileDeepSections.map((section, visibleIndex) => {
                    const canMoveUp = visibleIndex > 0
                    const canMoveDown =
                      visibleIndex < mobileDeepSections.length - 1

                    return (
                      <div key={section.id} className="relative w-full rounded-xl">
                        {mobileEditMode ? (
                          <div className="absolute right-1 top-1 z-10 flex gap-0.5 rounded-full border border-primary-200/80 bg-primary-50/90 p-0.5 shadow-sm">
                            {canMoveUp ? (
                              <button
                                type="button"
                                onClick={() =>
                                  moveMobileSection(visibleIndex, visibleIndex - 1)
                                }
                                className="inline-flex size-5 items-center justify-center rounded-full text-primary-400 transition-colors hover:text-primary-600"
                                aria-label={`Move ${section.label} up`}
                                title={`Move ${section.label} up`}
                              >
                                <HugeiconsIcon
                                  icon={ArrowUp02Icon}
                                  size={12}
                                  strokeWidth={1.8}
                                />
                              </button>
                            ) : null}
                            {canMoveDown ? (
                              <button
                                type="button"
                                onClick={() =>
                                  moveMobileSection(visibleIndex, visibleIndex + 1)
                                }
                                className="inline-flex size-5 items-center justify-center rounded-full text-primary-400 transition-colors hover:text-primary-600"
                                aria-label={`Move ${section.label} down`}
                                title={`Move ${section.label} down`}
                              >
                                <HugeiconsIcon
                                  icon={ArrowDown01Icon}
                                  size={12}
                                  strokeWidth={1.8}
                                />
                              </button>
                            ) : null}
                          </div>
                        ) : null}
                        {section.content}
                      </div>
                    )
                  })}
                </div>
              </div>
            ) : (
              <WidgetGrid items={desktopWidgetItems} />
            )}
          </div>
        </section>
      </main>

      <SettingsDialog
        open={dashSettingsOpen}
        onOpenChange={setDashSettingsOpen}
      />
      <DashboardOverflowPanel
        open={overflowOpen}
        onClose={() => setOverflowOpen(false)}
      />
    </>
  )
}
