import { useEffect, useMemo, useRef, useState } from 'react'
import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type {
  GatewaySession,
  GatewaySessionStatusResponse,
} from '@/lib/gateway-api'
import { fetchSessions } from '@/lib/gateway-api'
import { assignPersona } from '@/lib/agent-personas'

export type AgentModel = string

export type ActiveAgent = {
  id: string
  name: string
  task: string
  model: AgentModel
  status: string
  progress: number
  startedAtMs: number
  tokenCount: number
  estimatedCost: number
  isLive: boolean
}

export type QueuePriority = 'high' | 'normal' | 'low'

export type QueuedAgentTask = {
  id: string
  name: string
  description: string
  priority: QueuePriority
}

export type AgentHistoryStatus = 'success' | 'failed'

export type AgentHistoryItem = {
  id: string
  name: string
  description: string
  model: AgentModel
  status: AgentHistoryStatus
  runtimeSeconds: number
  tokenCount: number
  cost: number
}

type AgentViewState = {
  isOpen: boolean
  queueOpen: boolean
  historyOpen: boolean
  setOpen: (isOpen: boolean) => void
  toggleOpen: () => void
  setQueueOpen: (isOpen: boolean) => void
  setHistoryOpen: (isOpen: boolean) => void
}

const PANEL_WIDTH_PX = 320
const MIN_DESKTOP_WIDTH = 1024
const AUTO_OPEN_WIDTH = 1440
const REFRESH_INTERVAL_MS = 5000

function createDemoActiveAgents(): Array<ActiveAgent> {
  const now = Date.now()
  return [
    {
      id: 'demo-dashboard-infra',
      name: '🎨 Roger — Frontend Developer',
      task: 'Building dashboard widget grid with responsive layout',
      model: 'gpt-5.3-codex',
      status: 'running',
      progress: 67,
      startedAtMs: now - 204_000,
      tokenCount: 38_240,
      estimatedCost: 0.218,
      isLive: false,
    },
    {
      id: 'demo-skills-browser',
      name: '🏗️ Sally — Backend Architect',
      task: 'Creating API routes for skills marketplace',
      model: 'gpt-5.3-codex',
      status: 'thinking',
      progress: 42,
      startedAtMs: now - 131_000,
      tokenCount: 21_915,
      estimatedCost: 0.131,
      isLive: false,
    },
    {
      id: 'demo-terminal-integration',
      name: '🔍 Ada — QA Engineer',
      task: 'Running integration tests on terminal panel',
      model: 'gpt-5.3-codex',
      status: 'running',
      progress: 85,
      startedAtMs: now - 242_000,
      tokenCount: 47_609,
      estimatedCost: 0.286,
      isLive: false,
    },
  ]
}

function createDemoQueue(): Array<QueuedAgentTask> {
  return [
    {
      id: 'demo-queue-1',
      name: 'release-notes',
      description: 'Drafting release notes and migration checklist',
      priority: 'high',
    },
    {
      id: 'demo-queue-2',
      name: 'theme-pass',
      description: 'Applying dark theme polish to diagnostics screens',
      priority: 'normal',
    },
  ]
}

function createDemoHistory(): Array<AgentHistoryItem> {
  return [
    {
      id: 'demo-history-1',
      name: 'api-telemetry',
      description: 'Instrumented API telemetry dashboard',
      model: 'gpt-5-codex',
      status: 'success',
      runtimeSeconds: 452,
      tokenCount: 62_430,
      cost: 0.348,
    },
    {
      id: 'demo-history-2',
      name: 'auth-hardening',
      description: 'Added auth guardrails for session endpoints',
      model: 'claude-3-5-sonnet',
      status: 'success',
      runtimeSeconds: 311,
      tokenCount: 48_920,
      cost: 0.284,
    },
  ]
}

function inferInitialOpenState(): boolean {
  if (typeof window === 'undefined') return true
  return window.innerWidth >= AUTO_OPEN_WIDTH
}

function readString(value: unknown): string {
  if (typeof value !== 'string') return ''
  return value.trim()
}

function readNumber(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) return 0
  return value
}

function readTimestamp(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value < 1_000_000_000_000 ? value * 1000 : value
  }
  if (typeof value === 'string') {
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return parsed
  }
  return null
}

function readSessionKey(session: GatewaySession): string {
  const key = readString(session.key)
  if (key.length > 0) return key
  const friendly = readString(session.friendlyId)
  if (friendly.length > 0) return friendly
  return ''
}

function readSessionName(session: GatewaySession): string {
  // Assign persona based on session key + task for named agent display
  const key = readSessionKey(session)
  const taskText =
    readString(session.task) ||
    readString(session.initialMessage) ||
    readString(session.label)
  if (key.length > 0) {
    const persona = assignPersona(key, taskText)
    return `${persona.emoji} ${persona.name} — ${persona.role}`
  }

  const label = readString(session.label)
  if (label.length > 0) return label
  const title = readString(session.title)
  if (title.length > 0) return title
  const derived = readString(session.derivedTitle)
  if (derived.length > 0) return derived
  const friendly = readString(session.friendlyId)
  if (friendly.length > 0) return friendly
  return 'session'
}

function isAgentSession(session: GatewaySession): boolean {
  const key = readSessionKey(session).toLowerCase()

  // Must be a subagent session (spawned by the orchestrator)
  if (key.includes('subagent:')) return true

  // Also accept sessions explicitly marked as isolated
  const kind = readString(session.kind).toLowerCase()
  if (kind === 'isolated') return true

  // Filter out main sessions, cron jobs, etc.
  if (key === 'main' || key.includes(':main')) return false

  const friendlyId = readString(session.friendlyId).toLowerCase()
  if (friendlyId === 'main') return false

  // Accept Codex agent sessions (agent:codex:UUID but not agent:codex:main or cron)
  if (key.startsWith('agent:') && !key.includes('cron')) return true

  // Accept labeled sessions (user-spawned with a label)
  const label = readString(session.label)
  if (label.length > 0 && !key.includes('cron')) return true

  return false
}

function readTaskText(session: GatewaySession): string {
  const explicitTask = readString(session.task)
  if (explicitTask.length > 0) return explicitTask

  const initialMessage = readString(session.initialMessage)
  if (initialMessage.length > 0) return initialMessage

  const lastMessage = session.lastMessage
  if (lastMessage && typeof lastMessage === 'object') {
    const directText = readString((lastMessage as { text?: unknown }).text)
    if (directText.length > 0) return directText

    const content = (lastMessage as { content?: unknown }).content
    if (Array.isArray(content)) {
      const text = content
        .map(function mapPart(part) {
          if (!part || typeof part !== 'object') return ''
          return readString((part as { text?: unknown }).text)
        })
        .join(' ')
        .trim()
      if (text.length > 0) return text
    }
  }

  return 'Agent session in progress'
}

function readTokenCount(
  session: GatewaySession,
  status: GatewaySessionStatusResponse | null,
): number {
  const statusTokenCount = readNumber(status?.tokenCount)
  if (statusTokenCount > 0) return statusTokenCount

  const statusTotalTokens = readNumber(status?.totalTokens)
  if (statusTotalTokens > 0) return statusTotalTokens

  const statusUsageTokens = readNumber(status?.usage?.tokens)
  if (statusUsageTokens > 0) return statusUsageTokens

  const statusUsageTotal = readNumber(status?.usage?.totalTokens)
  if (statusUsageTotal > 0) return statusUsageTotal

  const sessionTokenCount = readNumber(session.tokenCount)
  if (sessionTokenCount > 0) return sessionTokenCount

  const sessionTotalTokens = readNumber(session.totalTokens)
  if (sessionTotalTokens > 0) return sessionTotalTokens

  const sessionUsageTokens = readNumber(session.usage?.tokens)
  if (sessionUsageTokens > 0) return sessionUsageTokens

  return readNumber(session.usage?.totalTokens)
}

function readEstimatedCost(
  session: GatewaySession,
  status: GatewaySessionStatusResponse | null,
  tokenCount: number,
): number {
  const statusCost = readNumber(status?.usage?.cost)
  if (statusCost > 0) return statusCost

  const sessionCost = readNumber(session.cost)
  if (sessionCost > 0) return sessionCost

  const usageCost = readNumber(session.usage?.cost)
  if (usageCost > 0) return usageCost

  return Number((tokenCount * 0.000004).toFixed(3))
}

function readProgress(
  session: GatewaySession,
  status: GatewaySessionStatusResponse | null,
): number {
  const statusProgress = readNumber(status?.progress)
  if (statusProgress > 0)
    return Math.max(1, Math.min(99, Math.round(statusProgress)))

  const sessionProgress = readNumber(session.progress)
  if (sessionProgress > 0)
    return Math.max(1, Math.min(99, Math.round(sessionProgress)))

  const sessionStatus = readStatus(session, status)
  if (isQueuedStatus(sessionStatus)) return 5
  if (isFailedStatus(sessionStatus)) return 100
  if (isCompletedStatus(sessionStatus)) return 100
  return 35
}

function readStatus(
  session: GatewaySession,
  status: GatewaySessionStatusResponse | null,
): string {
  const statusText = readString(status?.status)
  if (statusText.length > 0) return statusText.toLowerCase()

  const sessionStatus = readString(session.status)
  if (sessionStatus.length > 0) return sessionStatus.toLowerCase()

  // Heuristic: detect completion from staleness when gateway has no explicit status
  const updatedAt = readTimestamp(session.updatedAt)
  if (updatedAt) {
    const staleness = Date.now() - updatedAt
    const tokens =
      readNumber(session.totalTokens) || readNumber(session.tokenCount)
    if (tokens > 0 && staleness > 30_000) return 'complete'
    if (tokens === 0 && staleness > 120_000) return 'idle'
  }

  return 'running'
}

function readModel(
  session: GatewaySession,
  status: GatewaySessionStatusResponse | null,
): string {
  const statusModel = readString(status?.model)
  if (statusModel.length > 0) return statusModel

  const sessionModel = readString(session.model)
  if (sessionModel.length > 0) return sessionModel

  return 'unknown'
}

function readStartTimeMs(session: GatewaySession): number {
  const startedAt = readTimestamp(session.startedAt)
  if (startedAt) return startedAt

  const createdAt = readTimestamp(session.createdAt)
  if (createdAt) return createdAt

  const updatedAt = readTimestamp(session.updatedAt)
  if (updatedAt) return updatedAt

  return Date.now()
}

function isQueuedStatus(status: string): boolean {
  return ['queued', 'pending', 'waiting'].includes(status)
}

function isRunningStatus(status: string): boolean {
  return [
    'running',
    'active',
    'started',
    'streaming',
    'processing',
    'in_progress',
    'thinking',
  ].includes(status)
}

function isCompletedStatus(status: string): boolean {
  return ['complete', 'completed', 'success', 'succeeded', 'done'].includes(
    status,
  )
}

function isFailedStatus(status: string): boolean {
  return ['failed', 'error', 'cancelled', 'canceled', 'killed'].includes(status)
}

function mapSessionToActiveAgent(
  session: GatewaySession,
  status: GatewaySessionStatusResponse | null,
): ActiveAgent {
  const tokenCount = readTokenCount(session, status)
  return {
    id: readSessionKey(session) || crypto.randomUUID(),
    name: readSessionName(session),
    task: readTaskText(session),
    model: readModel(session, status),
    status: readStatus(session, status),
    progress: readProgress(session, status),
    startedAtMs: readStartTimeMs(session),
    tokenCount,
    estimatedCost: readEstimatedCost(session, status, tokenCount),
    isLive: true,
  }
}

function mapSessionToQueuedTask(session: GatewaySession): QueuedAgentTask {
  return {
    id: readSessionKey(session) || `queued-${crypto.randomUUID()}`,
    name: readSessionName(session),
    description: readTaskText(session),
    priority: 'normal',
  }
}

function mapSessionToHistoryItem(
  session: GatewaySession,
  status: GatewaySessionStatusResponse | null,
): AgentHistoryItem {
  const startMs = readStartTimeMs(session)
  const endMs = readTimestamp(session.updatedAt) ?? Date.now()
  const tokenCount = readTokenCount(session, status)
  const statusText = readStatus(session, status)

  return {
    id: readSessionKey(session) || `history-${crypto.randomUUID()}`,
    name: readSessionName(session),
    description: readTaskText(session),
    model: readModel(session, status),
    status: isFailedStatus(statusText) ? 'failed' : 'success',
    runtimeSeconds: Math.max(1, Math.floor((endMs - startMs) / 1000)),
    tokenCount,
    cost: readEstimatedCost(session, status, tokenCount),
  }
}

export const useAgentViewStore = create<AgentViewState>()(
  persist(
    (set) => ({
      isOpen: inferInitialOpenState(),
      queueOpen: true,
      historyOpen: false,
      setOpen: function setOpen(isOpen) {
        set({ isOpen })
      },
      toggleOpen: function toggleOpen() {
        set((state) => ({ isOpen: !state.isOpen }))
      },
      setQueueOpen: function setQueueOpen(isOpen) {
        set({ queueOpen: isOpen })
      },
      setHistoryOpen: function setHistoryOpen(isOpen) {
        set({ historyOpen: isOpen })
      },
    }),
    {
      name: 'agent-view-state',
    },
  ),
)

export type AgentViewResult = {
  isOpen: boolean
  queueOpen: boolean
  historyOpen: boolean
  isDesktop: boolean
  shouldAutoOpen: boolean
  panelVisible: boolean
  showFloatingToggle: boolean
  panelWidth: number
  panelOffset: number
  nowMs: number
  lastRefreshedMs: number
  activeAgents: Array<ActiveAgent>
  queuedAgents: Array<QueuedAgentTask>
  historyAgents: Array<AgentHistoryItem>
  activeCount: number
  isLoading: boolean
  isDemoMode: boolean
  isLiveConnected: boolean
  errorMessage: string | null
  setOpen: (isOpen: boolean) => void
  toggleOpen: () => void
  setQueueOpen: (isOpen: boolean) => void
  setHistoryOpen: (isOpen: boolean) => void
  killAgent: (agentId: string) => void
  cancelQueueTask: (taskId: string) => void
}

export function useAgentView(): AgentViewResult {
  const isOpen = useAgentViewStore((state) => state.isOpen)
  const queueOpen = useAgentViewStore((state) => state.queueOpen)
  const historyOpen = useAgentViewStore((state) => state.historyOpen)
  const setOpen = useAgentViewStore((state) => state.setOpen)
  const toggleOpen = useAgentViewStore((state) => state.toggleOpen)
  const setQueueOpen = useAgentViewStore((state) => state.setQueueOpen)
  const setHistoryOpen = useAgentViewStore((state) => state.setHistoryOpen)

  const [viewportWidth, setViewportWidth] = useState(() => {
    if (typeof window === 'undefined') return AUTO_OPEN_WIDTH
    return window.innerWidth
  })
  const [nowMs, setNowMs] = useState(() => Date.now())
  const [lastRefreshedMs, setLastRefreshedMs] = useState(() => Date.now())
  const [activeAgents, setActiveAgents] = useState<Array<ActiveAgent>>([])
  const [queuedAgents, setQueuedAgents] = useState<Array<QueuedAgentTask>>([])
  const [historyAgents, setHistoryAgents] = useState<Array<AgentHistoryItem>>(
    [],
  )
  const [isLoading, setIsLoading] = useState(true)
  const [isDemoMode, setIsDemoMode] = useState(false)
  const [isLiveConnected, setIsLiveConnected] = useState(false)
  const [errorMessage, setErrorMessage] = useState<string | null>(null)

  const previousAutoOpenRef = useRef(false)

  useEffect(() => {
    function handleResize() {
      setViewportWidth(window.innerWidth)
    }

    handleResize()
    window.addEventListener('resize', handleResize)
    return function cleanupResize() {
      window.removeEventListener('resize', handleResize)
    }
  }, [])

  useEffect(() => {
    const timer = window.setInterval(() => {
      setNowMs(Date.now())
    }, 5000) // Every 5s instead of 1s to reduce re-renders

    return function cleanupTimer() {
      window.clearInterval(timer)
    }
  }, [])

  useEffect(() => {
    let isDisposed = false

    async function refresh() {
      try {
        const sessionsPayload = await fetchSessions()
        const sessions = Array.isArray(sessionsPayload.sessions)
          ? sessionsPayload.sessions
          : []

        // Skip per-session status fetch — /api/sessions/:key/status route
        // doesn't exist, causing 404 spam. Use session data directly.
        const statusEntries = sessions.map(function loadStatus(session) {
          const key = readSessionKey(session)
          return [key, null] as const
        })

        const statusMap = new Map<string, GatewaySessionStatusResponse | null>(
          statusEntries,
        )

        const nextActiveAgents: Array<ActiveAgent> = []
        const nextQueuedAgents: Array<QueuedAgentTask> = []
        const nextHistoryAgents: Array<AgentHistoryItem> = []

        sessions.forEach(function classifySession(session) {
          if (!isAgentSession(session)) return
          const key = readSessionKey(session)
          const status = key ? (statusMap.get(key) ?? null) : null
          const statusText = readStatus(session, status)

          if (isQueuedStatus(statusText)) {
            nextQueuedAgents.push(mapSessionToQueuedTask(session))
            return
          }

          if (isCompletedStatus(statusText) || isFailedStatus(statusText)) {
            nextHistoryAgents.push(mapSessionToHistoryItem(session, status))
            return
          }

          if (isRunningStatus(statusText) || statusText.length === 0) {
            nextActiveAgents.push(mapSessionToActiveAgent(session, status))
          }
        })

        if (isDisposed) return

        setActiveAgents(nextActiveAgents)
        setQueuedAgents(nextQueuedAgents)
        setHistoryAgents(nextHistoryAgents.slice(0, 10))
        setIsDemoMode(false)
        setIsLiveConnected(true)
        setErrorMessage(null)
      } catch (error) {
        if (isDisposed) return

        setActiveAgents(createDemoActiveAgents())
        setQueuedAgents(createDemoQueue())
        setHistoryAgents(createDemoHistory())
        setIsDemoMode(true)
        setIsLiveConnected(false)
        setErrorMessage(
          error instanceof Error ? error.message : 'Gateway unavailable',
        )
      } finally {
        if (!isDisposed) {
          setLastRefreshedMs(Date.now())
          setIsLoading(false)
        }
      }
    }

    void refresh()
    const refreshTimer = window.setInterval(() => {
      void refresh()
    }, REFRESH_INTERVAL_MS)

    return function cleanupRefresh() {
      isDisposed = true
      window.clearInterval(refreshTimer)
    }
  }, [])

  const shouldAutoOpen = viewportWidth >= AUTO_OPEN_WIDTH
  useEffect(() => {
    const isCrossingToLargeDesktop =
      shouldAutoOpen && previousAutoOpenRef.current !== shouldAutoOpen
    previousAutoOpenRef.current = shouldAutoOpen
    if (isCrossingToLargeDesktop) {
      setOpen(true)
    }
  }, [setOpen, shouldAutoOpen])

  const isDesktop = viewportWidth >= MIN_DESKTOP_WIDTH
  const panelVisible = isDesktop && isOpen
  const showFloatingToggle = isDesktop && !isOpen
  const panelOffset = panelVisible ? PANEL_WIDTH_PX : 0

  function killAgent(agentId: string) {
    setActiveAgents((previous) => {
      const killedAgent = previous.find((agent) => agent.id === agentId)
      if (!killedAgent) return previous

      const runtimeSeconds = Math.max(
        1,
        Math.floor((Date.now() - killedAgent.startedAtMs) / 1000),
      )
      const historyEntry: AgentHistoryItem = {
        id: `history-${crypto.randomUUID()}`,
        name: killedAgent.name,
        description: killedAgent.task,
        model: killedAgent.model,
        status: 'failed',
        runtimeSeconds,
        tokenCount: killedAgent.tokenCount,
        cost: killedAgent.estimatedCost,
      }

      setHistoryAgents((current) => [historyEntry, ...current].slice(0, 10))
      return previous.filter((agent) => agent.id !== agentId)
    })
  }

  function cancelQueueTask(taskId: string) {
    setQueuedAgents((previous) => previous.filter((task) => task.id !== taskId))
  }

  return useMemo(
    () => ({
      isOpen,
      queueOpen,
      historyOpen,
      isDesktop,
      shouldAutoOpen,
      panelVisible,
      showFloatingToggle,
      panelWidth: PANEL_WIDTH_PX,
      panelOffset,
      nowMs,
      lastRefreshedMs,
      activeAgents,
      queuedAgents,
      historyAgents,
      activeCount: activeAgents.length,
      isLoading,
      isDemoMode,
      isLiveConnected,
      errorMessage,
      setOpen,
      toggleOpen,
      setQueueOpen,
      setHistoryOpen,
      killAgent,
      cancelQueueTask,
    }),
    [
      activeAgents,
      cancelQueueTask,
      errorMessage,
      historyAgents,
      historyOpen,
      isDemoMode,
      isDesktop,
      isLiveConnected,
      isLoading,
      isOpen,
      killAgent,
      lastRefreshedMs,
      nowMs,
      panelOffset,
      panelVisible,
      queueOpen,
      queuedAgents,
      setHistoryOpen,
      setOpen,
      setQueueOpen,
      shouldAutoOpen,
      showFloatingToggle,
      toggleOpen,
    ],
  )
}

export function formatRuntime(totalSeconds: number): string {
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return `${minutes}m ${String(seconds).padStart(2, '0')}s`
}

export function formatCost(cost: number): string {
  return `$${cost.toFixed(3)}`
}
