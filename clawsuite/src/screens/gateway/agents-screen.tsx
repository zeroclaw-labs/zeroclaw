import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  AlertDiamondIcon,
  ArrowTurnBackwardIcon,
  BotIcon,
} from '@hugeicons/core-free-icons'
import { EmptyState } from '@/components/empty-state'
import {
  AgentRegistryCard,
  type AgentRegistryCardData,
  type AgentRegistryStatus,
} from '@/components/agent-view/agent-registry-card'
import { toggleAgentPause } from '@/lib/gateway-api'
import { toast } from '@/components/ui/toast'

type AgentGatewayEntry = {
  id?: string
  name?: string
  role?: string
  category?: string
  color?: string
  [key: string]: unknown
}

type AgentsData = {
  defaultId?: string
  mainKey?: string
  scope?: string
  agents?: AgentGatewayEntry[]
  [key: string]: unknown
}

type SessionEntry = {
  key?: string
  friendlyId?: string
  label?: string
  displayName?: string
  title?: string
  derivedTitle?: string
  task?: string
  status?: string
  updatedAt?: number | string
  enabled?: boolean
  [key: string]: unknown
}

type AgentDefinition = {
  id: string
  name: string
  category: string
  role: string
  color: AgentRegistryCardData['color']
  aliases: Array<string>
}

type AgentRuntime = AgentRegistryCardData & {
  matchedSessions: Array<SessionEntry>
}

const CATEGORY_ORDER = ['Core', 'Coding', 'System', 'Integrations'] as const

const STATUS_SORT_ORDER: Record<AgentRegistryStatus, number> = {
  active: 0,
  idle: 1,
  available: 2,
  paused: 3,
}

const RUNNING_STATUSES = new Set([
  'running',
  'active',
  'thinking',
  'processing',
  'streaming',
  'in-progress',
  'inprogress',
])

const PAUSED_STATUSES = new Set(['paused', 'pause', 'suspended'])

const ACTIVE_HEARTBEAT_MS = 30_000

// TODO: Replace with gateway-backed config once a dedicated agent registry schema is available.
const FALLBACK_AGENT_REGISTRY: Array<AgentDefinition> = [
  {
    id: 'aurora-main',
    name: 'Aurora/Main',
    category: 'Core',
    role: 'Orchestrator',
    color: 'orange',
    aliases: ['aurora-main', 'aurora'],
  },
  {
    id: 'codex',
    name: 'Codex',
    category: 'Coding',
    role: 'Coding specialist',
    color: 'blue',
    aliases: ['codex', 'coding'],
  },
  {
    id: 'memory-consolidator',
    name: 'Memory consolidator',
    category: 'System',
    role: 'Memory service',
    color: 'violet',
    aliases: ['memory-consolidator', 'memory'],
  },
  {
    id: 'telegram-gateway',
    name: 'Telegram gateway',
    category: 'Integrations',
    role: 'Channel bridge',
    color: 'cyan',
    aliases: ['telegram-gateway', 'telegram'],
  },
]

function readString(value: unknown): string {
  if (typeof value !== 'string') return ''
  return value.trim()
}

function readTimestamp(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value < 1_000_000_000_000 ? value * 1000 : value
  }
  if (typeof value === 'string') {
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return parsed
  }
  return 0
}

function normalizeToken(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
}

function deriveFriendlyIdFromKey(key: string): string {
  const trimmed = key.trim()
  if (!trimmed) return ''
  const parts = trimmed.split(':')
  const tail = parts[parts.length - 1]
  return tail && tail.trim().length > 0 ? tail.trim() : trimmed
}

function inferCategoryFromText(text: string): string {
  const normalized = normalizeToken(text)
  if (
    normalized.includes('codex') ||
    normalized.includes('coding') ||
    normalized.includes('developer')
  ) {
    return 'Coding'
  }
  if (
    normalized.includes('memory') ||
    normalized.includes('system') ||
    normalized.includes('ops')
  ) {
    return 'System'
  }
  if (
    normalized.includes('telegram') ||
    normalized.includes('discord') ||
    normalized.includes('slack') ||
    normalized.includes('integration') ||
    normalized.includes('gateway')
  ) {
    return 'Integrations'
  }
  return 'Core'
}

function normalizeCategoryLabel(category: string): string {
  const normalized = normalizeToken(category)
  if (normalized === 'core') return 'Core'
  if (normalized === 'coding') return 'Coding'
  if (normalized === 'system') return 'System'
  if (normalized === 'integrations' || normalized === 'integration') {
    return 'Integrations'
  }
  return category
}

function inferRoleFromCategory(category: string): string {
  if (category === 'Coding') return 'Coding agent'
  if (category === 'System') return 'System agent'
  if (category === 'Integrations') return 'Integration agent'
  return 'Core agent'
}

function inferColorFromCategory(
  category: string,
): AgentRegistryCardData['color'] {
  if (category === 'Coding') return 'blue'
  if (category === 'System') return 'violet'
  if (category === 'Integrations') return 'cyan'
  return 'orange'
}

function dedupe(values: Array<string>): Array<string> {
  const result: Array<string> = []
  const seen = new Set<string>()

  values.forEach((value) => {
    const normalized = normalizeToken(value)
    if (!normalized || seen.has(normalized)) return
    seen.add(normalized)
    result.push(normalized)
  })

  return result
}

function toAgentDefinition(
  value: unknown,
  index: number,
): AgentDefinition | null {
  const record =
    value && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : null

  if (!record) return null

  const id = readString(record.id || record.key || record.agentId)
  const name = readString(record.name || record.label || record.displayName)

  const fallbackId = normalizeToken(id || name)
  if (!fallbackId) return null

  const categoryRaw = readString(record.category || record.group || record.kind)
  const roleRaw = readString(record.role || record.description)
  const colorRaw = normalizeToken(readString(record.color))

  const category = normalizeCategoryLabel(
    categoryRaw.length > 0
      ? categoryRaw
      : inferCategoryFromText(`${fallbackId} ${name}`),
  )

  let color = inferColorFromCategory(category)
  if (
    colorRaw === 'orange' ||
    colorRaw === 'blue' ||
    colorRaw === 'cyan' ||
    colorRaw === 'purple' ||
    colorRaw === 'violet'
  ) {
    color = colorRaw
  }

  const aliasParts = [
    id,
    name,
    fallbackId,
    readString(record.profile),
    readString(record.handle),
  ]

  const primaryNameToken = normalizeToken(name).split('-')[0] || ''
  if (primaryNameToken) aliasParts.push(primaryNameToken)

  return {
    id: fallbackId || `agent-${index + 1}`,
    name: name || id || `Agent ${index + 1}`,
    category,
    role: roleRaw || inferRoleFromCategory(category),
    color,
    aliases: dedupe(aliasParts),
  }
}

function parseAgentDefinitions(data: AgentsData | undefined): Array<AgentDefinition> | null {
  if (!data || typeof data !== 'object') return null

  const directAgents = Array.isArray(data.agents) ? data.agents : null
  if (directAgents) {
    return directAgents
      .map((entry, index) => toAgentDefinition(entry, index))
      .filter((entry): entry is AgentDefinition => entry !== null)
  }

  const record = data as Record<string, unknown>
  const alternateLists = ['registry', 'agentDefinitions']

  for (const key of alternateLists) {
    const list = record[key]
    if (!Array.isArray(list)) continue

    return list
      .map((entry, index) => toAgentDefinition(entry, index))
      .filter((entry): entry is AgentDefinition => entry !== null)
  }

  const profiles = record.profiles
  if (profiles && typeof profiles === 'object' && !Array.isArray(profiles)) {
    const entries = Object.entries(profiles).map(([profileId, profileValue]) => {
      const profileRecord =
        profileValue &&
        typeof profileValue === 'object' &&
        !Array.isArray(profileValue)
          ? (profileValue as Record<string, unknown>)
          : {}
      return {
        ...profileRecord,
        id: profileId,
        name: readString(profileRecord.name) || profileId,
      }
    })

    return entries
      .map((entry, index) => toAgentDefinition(entry, index))
      .filter((entry): entry is AgentDefinition => entry !== null)
  }

  return null
}

function getSessionSearchBlob(session: SessionEntry): string {
  const values = [
    readString(session.key),
    readString(session.friendlyId),
    readString(session.label),
    readString(session.displayName),
    readString(session.title),
    readString(session.derivedTitle),
    readString(session.task),
    readString(session.agentId),
    readString(session.agent),
    readString(session.profile),
  ]

  return normalizeToken(values.join(' '))
}

function getSessionFriendlyId(session: SessionEntry | undefined): string {
  if (!session) return ''
  const friendlyId = readString(session.friendlyId)
  if (friendlyId) return friendlyId
  return deriveFriendlyIdFromKey(readString(session.key))
}

function getSessionTitle(session: SessionEntry): string {
  return (
    readString(session.label) ||
    readString(session.displayName) ||
    readString(session.title) ||
    readString(session.derivedTitle) ||
    getSessionFriendlyId(session) ||
    readString(session.key) ||
    'Session'
  )
}

function scoreSessionMatch(agent: AgentDefinition, session: SessionEntry): number {
  const sessionKey = normalizeToken(readString(session.key))
  const friendlyId = normalizeToken(readString(session.friendlyId))
  const blob = getSessionSearchBlob(session)

  let best = 0

  for (const alias of agent.aliases) {
    if (!alias) continue

    if (sessionKey === alias || friendlyId === alias) {
      best = Math.max(best, 100)
      continue
    }

    if (
      sessionKey.startsWith(`${alias}-`) ||
      sessionKey.includes(`:${alias}:`) ||
      sessionKey.endsWith(`:${alias}`) ||
      friendlyId.startsWith(`${alias}-`)
    ) {
      best = Math.max(best, 85)
      continue
    }

    if (blob.includes(alias)) {
      best = Math.max(best, 65)
    }
  }

  return best
}

function isPausedSession(session: SessionEntry): boolean {
  const status = normalizeToken(readString(session.status))
  if (PAUSED_STATUSES.has(status)) return true
  if (typeof session.enabled === 'boolean') return session.enabled === false
  return false
}

function deriveAgentStatus(
  session: SessionEntry | undefined,
  pausedOverride: boolean | undefined,
): AgentRegistryStatus {
  if (typeof pausedOverride === 'boolean') {
    if (pausedOverride) return 'paused'
    if (!session) return 'available'
  }

  if (!session) return 'available'

  if (isPausedSession(session)) return 'paused'

  const status = normalizeToken(readString(session.status))
  const updatedAt = readTimestamp(session.updatedAt)
  const staleMs = updatedAt > 0 ? Date.now() - updatedAt : 0
  const runningLike = RUNNING_STATUSES.has(status) || status.length === 0

  if (runningLike && (updatedAt <= 0 || staleMs <= ACTIVE_HEARTBEAT_MS)) {
    return 'active'
  }

  return 'idle'
}

function formatRelativeTime(value: unknown): string {
  const timestamp = readTimestamp(value)
  if (!timestamp) return 'No activity timestamp'

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

async function readResponseError(response: Response): Promise<string> {
  try {
    const payload = (await response.json()) as Record<string, unknown>
    if (typeof payload.error === 'string' && payload.error.trim()) {
      return payload.error
    }
  } catch {
    // no-op
  }

  return response.statusText || `HTTP ${response.status}`
}

export function AgentsScreen() {
  const navigate = useNavigate()
  const [optimisticPausedByAgentId, setOptimisticPausedByAgentId] = useState<
    Record<string, boolean>
  >({})
  const [spawningByAgentId, setSpawningByAgentId] = useState<
    Record<string, boolean>
  >({})
  const [historyAgentId, setHistoryAgentId] = useState<string | null>(null)

  const agentsQuery = useQuery({
    queryKey: ['gateway', 'agents'],
    queryFn: async () => {
      const res = await fetch('/api/gateway/agents')
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const json = await res.json()
      if (!json.ok) throw new Error(json.error || 'Gateway error')
      return json.data as AgentsData
    },
    refetchInterval: 15_000,
    retry: 1,
  })

  const sessionsQuery = useQuery({
    queryKey: ['agent-registry', 'sessions'],
    queryFn: async () => {
      const res = await fetch('/api/sessions')
      if (!res.ok) return [] as Array<SessionEntry>
      const payload = (await res.json()) as { sessions?: Array<SessionEntry> }
      return Array.isArray(payload.sessions) ? payload.sessions : []
    },
    refetchInterval: 10_000,
    retry: false,
  })

  const parsedDefinitions = useMemo(
    () => parseAgentDefinitions(agentsQuery.data),
    [agentsQuery.data],
  )

  const usingFallbackRegistry =
    !agentsQuery.isLoading && parsedDefinitions === null

  const registryDefinitions = parsedDefinitions ?? FALLBACK_AGENT_REGISTRY

  const runtimeAgents = useMemo(() => {
    const sessions = Array.isArray(sessionsQuery.data) ? sessionsQuery.data : []

    return registryDefinitions.map((definition) => {
      const matchedSessions = sessions
        .map((session) => {
          const score = scoreSessionMatch(definition, session)
          return {
            session,
            score,
            updatedAt: readTimestamp(session.updatedAt),
          }
        })
        .filter((candidate) => candidate.score > 0)
        .sort((left, right) => {
          if (right.score !== left.score) return right.score - left.score
          return right.updatedAt - left.updatedAt
        })
        .map((candidate) => candidate.session)

      const primarySession = matchedSessions[0]
      const hasOverride = Object.prototype.hasOwnProperty.call(
        optimisticPausedByAgentId,
        definition.id,
      )
      const pausedOverride = hasOverride
        ? optimisticPausedByAgentId[definition.id]
        : undefined

      const sessionKey = readString(primarySession?.key)
      const friendlyId = getSessionFriendlyId(primarySession)
      const status = deriveAgentStatus(primarySession, pausedOverride)

      return {
        id: definition.id,
        name: definition.name,
        role: definition.role,
        category: definition.category,
        color: definition.color,
        status,
        sessionKey: sessionKey || undefined,
        friendlyId: friendlyId || undefined,
        controlKey: sessionKey || definition.id,
        matchedSessions,
      } satisfies AgentRuntime
    })
  }, [registryDefinitions, sessionsQuery.data, optimisticPausedByAgentId])

  const groupedSections = useMemo(() => {
    const grouped = new Map<string, Array<AgentRuntime>>()

    runtimeAgents.forEach((agent) => {
      const existing = grouped.get(agent.category) ?? []
      existing.push(agent)
      grouped.set(agent.category, existing)
    })

    const orderedCategories = [
      ...CATEGORY_ORDER.filter((category) => grouped.has(category)),
      ...Array.from(grouped.keys())
        .filter((category) => !CATEGORY_ORDER.includes(category as never))
        .sort((left, right) => left.localeCompare(right)),
    ]

    return orderedCategories.map((category) => {
      const agentsInCategory = (grouped.get(category) ?? []).sort((left, right) => {
        const leftPriority = STATUS_SORT_ORDER[left.status] ?? 9
        const rightPriority = STATUS_SORT_ORDER[right.status] ?? 9
        if (leftPriority !== rightPriority) return leftPriority - rightPriority
        return left.name.localeCompare(right.name)
      })

      return {
        category,
        agents: agentsInCategory,
      }
    })
  }, [runtimeAgents])

  const selectedHistoryAgent = useMemo(
    () => runtimeAgents.find((agent) => agent.id === historyAgentId) ?? null,
    [historyAgentId, runtimeAgents],
  )

  async function spawnSessionForAgent(
    agent: AgentRegistryCardData,
  ): Promise<{ sessionKey: string; friendlyId: string } | null> {
    if (spawningByAgentId[agent.id]) return null

    setSpawningByAgentId((previous) => ({ ...previous, [agent.id]: true }))

    try {
      const baseFriendlyId = normalizeToken(agent.id || agent.name || 'agent')
      const friendlyId = `${baseFriendlyId}-${Math.random().toString(36).slice(2, 8)}`

      const response = await fetch('/api/sessions', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          friendlyId,
          label: agent.name,
        }),
      })

      if (!response.ok) {
        throw new Error(await readResponseError(response))
      }

      const payload = (await response.json()) as {
        sessionKey?: string
        friendlyId?: string
      }

      const sessionKey = readString(payload.sessionKey)
      const resolvedFriendlyId =
        readString(payload.friendlyId) || deriveFriendlyIdFromKey(sessionKey)

      if (!sessionKey || !resolvedFriendlyId) {
        throw new Error('Failed to create a session for this agent')
      }

      toast(`${agent.name} session started`, { type: 'success' })
      void sessionsQuery.refetch()

      return { sessionKey, friendlyId: resolvedFriendlyId }
    } catch (error) {
      const message =
        error instanceof Error ? error.message : 'Failed to spawn agent session'
      toast(message, { type: 'error' })
      return null
    } finally {
      setSpawningByAgentId((previous) => {
        const next = { ...previous }
        delete next[agent.id]
        return next
      })
    }
  }

  async function handleChat(agent: AgentRegistryCardData) {
    const existingFriendlyId =
      readString(agent.friendlyId) || deriveFriendlyIdFromKey(readString(agent.sessionKey))

    if (existingFriendlyId) {
      void navigate({
        to: '/chat/$sessionKey',
        params: { sessionKey: existingFriendlyId },
      })
      return
    }

    const spawned = await spawnSessionForAgent(agent)
    if (!spawned) return

    void navigate({
      to: '/chat/$sessionKey',
      params: { sessionKey: spawned.friendlyId },
    })
  }

  async function handleSpawn(agent: AgentRegistryCardData) {
    await spawnSessionForAgent(agent)
  }

  function handleHistory(agent: AgentRegistryCardData) {
    setHistoryAgentId(agent.id)
  }

  async function handlePauseToggle(
    agent: AgentRegistryCardData,
    nextPaused: boolean,
  ) {
    const controlKey = readString(agent.controlKey)
    if (!controlKey) {
      toast('No control key available for this agent', { type: 'warning' })
      return
    }

    const hadPrevious = Object.prototype.hasOwnProperty.call(
      optimisticPausedByAgentId,
      agent.id,
    )
    const previousValue = optimisticPausedByAgentId[agent.id]

    setOptimisticPausedByAgentId((previous) => ({
      ...previous,
      [agent.id]: nextPaused,
    }))

    try {
      const payload = await toggleAgentPause(controlKey, nextPaused)
      const paused =
        typeof payload.paused === 'boolean' ? payload.paused : nextPaused

      setOptimisticPausedByAgentId((previous) => ({
        ...previous,
        [agent.id]: paused,
      }))

      toast(`${agent.name} ${paused ? 'paused' : 'resumed'}`, {
        type: 'success',
      })
      void sessionsQuery.refetch()
    } catch (error) {
      setOptimisticPausedByAgentId((previous) => {
        const next = { ...previous }
        if (hadPrevious) {
          next[agent.id] = previousValue
        } else {
          delete next[agent.id]
        }
        return next
      })

      const message =
        error instanceof Error
          ? error.message
          : `Failed to ${nextPaused ? 'pause' : 'resume'} agent`
      toast(message, { type: 'error' })
    }
  }

  function handleKilled(agent: AgentRegistryCardData) {
    setOptimisticPausedByAgentId((previous) => {
      const next = { ...previous }
      delete next[agent.id]
      return next
    })
    void sessionsQuery.refetch()
  }

  const lastUpdated = agentsQuery.dataUpdatedAt
    ? new Date(agentsQuery.dataUpdatedAt).toLocaleTimeString()
    : null

  const desktopAgents = agentsQuery.data?.agents || []

  return (
    <>
      <div className="flex h-full min-h-0 flex-col overflow-x-hidden md:hidden">
        <div className="border-b border-primary-200 px-3 py-2">
          <div className="flex items-center justify-between">
            <div>
              <h1 className="text-sm font-semibold text-ink">Agent Hub</h1>
              <p className="text-[11px] text-primary-500">Registry</p>
            </div>
            <div className="flex items-center gap-2">
              {agentsQuery.isFetching && !agentsQuery.isLoading ? (
                <span className="text-[10px] text-primary-500 animate-pulse">
                  syncing...
                </span>
              ) : null}
              <span
                className={`inline-block size-2 rounded-full ${
                  agentsQuery.isError
                    ? 'bg-red-500'
                    : agentsQuery.isSuccess
                      ? 'bg-emerald-500'
                      : 'bg-amber-500'
                }`}
              />
            </div>
          </div>
        </div>

        <div className="flex-1 overflow-auto px-3 pt-3 pb-24">
          {agentsQuery.isLoading && !agentsQuery.data ? (
            <div className="flex items-center justify-center h-32">
              <div className="flex items-center gap-2 text-primary-500">
                <div className="size-4 border-2 border-primary-300 border-t-primary-600 rounded-full animate-spin" />
                <span className="text-sm">Loading registry...</span>
              </div>
            </div>
          ) : registryDefinitions.length === 0 ? (
            <div className="rounded-2xl bg-white/60 dark:bg-neutral-900/50 backdrop-blur-md border border-white/30 dark:border-white/10 shadow-sm p-5">
              <h2 className="text-base font-semibold text-neutral-900 dark:text-neutral-100">
                Add your first agent
              </h2>
              <ul className="mt-3 space-y-2 text-sm text-neutral-600 dark:text-neutral-300">
                <li>Create an agent profile</li>
                <li>Connect a gateway</li>
                <li>Spawn your first session</li>
              </ul>
              <button
                type="button"
                onClick={() => {
                  void navigate({ to: '/settings' })
                }}
                className="mt-4 inline-flex h-9 items-center rounded-xl bg-accent-500 px-4 text-sm font-medium text-white shadow-sm hover:bg-accent-600"
              >
                Open Settings
              </button>
            </div>
          ) : (
            <div className="space-y-4">
              {usingFallbackRegistry ? (
                <div className="rounded-xl border border-amber-300/50 bg-amber-50/70 px-3 py-2 text-[11px] font-medium text-amber-800 dark:border-amber-500/40 dark:bg-amber-900/20 dark:text-amber-200">
                  Gateway registry unavailable. Showing fallback definitions.
                </div>
              ) : null}

              {groupedSections.map((section) => (
                <section key={section.category} className="space-y-2">
                  <div className="flex items-center justify-between px-1">
                    <h2 className="text-xs font-semibold tracking-wide text-neutral-500 dark:text-neutral-400">
                      {section.category}
                    </h2>
                    <span className="text-[11px] font-medium text-neutral-500 dark:text-neutral-400">
                      {section.agents.length}
                    </span>
                  </div>

                  <div className="grid grid-cols-1 gap-3">
                    {section.agents.map((agent) => (
                      <AgentRegistryCard
                        key={agent.id}
                        agent={agent}
                        isSpawning={Boolean(spawningByAgentId[agent.id])}
                        onChat={handleChat}
                        onSpawn={handleSpawn}
                        onHistory={handleHistory}
                        onPauseToggle={handlePauseToggle}
                        onKilled={handleKilled}
                      />
                    ))}
                  </div>
                </section>
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="hidden h-full min-h-0 flex-col overflow-x-hidden md:flex">
        <div className="flex items-center justify-between border-b border-primary-200 px-3 py-2 md:px-6 md:py-4">
          <div className="flex items-center gap-3">
            <h1 className="text-sm font-semibold text-ink md:text-[15px]">
              Agents
            </h1>
            {agentsQuery.isFetching && !agentsQuery.isLoading ? (
              <span className="text-[10px] text-primary-500 animate-pulse">
                syncing...
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
              className={`inline-block size-2 rounded-full ${agentsQuery.isError ? 'bg-red-500' : agentsQuery.isSuccess ? 'bg-emerald-500' : 'bg-amber-500'}`}
            />
          </div>
        </div>

        <div className="flex-1 overflow-auto px-3 pt-3 pb-24 md:px-6 md:pt-4 md:pb-0">
          {agentsQuery.isLoading ? (
            <div className="flex items-center justify-center h-32">
              <div className="flex items-center gap-2 text-primary-500">
                <div className="size-4 border-2 border-primary-300 border-t-primary-600 rounded-full animate-spin" />
                <span className="text-sm">Connecting to gateway...</span>
              </div>
            </div>
          ) : agentsQuery.isError ? (
            <div className="flex flex-col items-center justify-center h-32 gap-3">
              <HugeiconsIcon
                icon={AlertDiamondIcon}
                size={24}
                strokeWidth={1.5}
                className="text-red-500"
              />
              <p className="text-sm text-primary-600">
                {agentsQuery.error instanceof Error
                  ? agentsQuery.error.message
                  : 'Failed to fetch'}
              </p>
              <button
                type="button"
                onClick={() => agentsQuery.refetch()}
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
          ) : (
            <>
              <div className="mb-4 grid gap-3 text-[13px] sm:grid-cols-2 lg:mb-6 lg:grid-cols-3 lg:gap-6">
                <div>
                  <span className="text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                    Default Agent
                  </span>
                  <p className="font-medium text-ink mt-0.5">
                    {agentsQuery.data?.defaultId || '-'}
                  </p>
                </div>
                <div>
                  <span className="text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                    Main Key
                  </span>
                  <p className="font-medium text-ink mt-0.5">
                    {agentsQuery.data?.mainKey || '-'}
                  </p>
                </div>
                <div>
                  <span className="text-[11px] font-medium text-primary-500 uppercase tracking-wider">
                    Scope
                  </span>
                  <p className="font-medium text-ink mt-0.5">
                    {agentsQuery.data?.scope || '-'}
                  </p>
                </div>
              </div>

              {desktopAgents.length === 0 ? (
                <EmptyState
                  icon={BotIcon}
                  title="No agents detected"
                  description="Start a conversation and let the AI orchestrate sub-agents."
                />
              ) : (
                <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
                  {desktopAgents.map((agent) => {
                    const isDefault = agent.id === agentsQuery.data?.defaultId
                    return (
                      <div
                        key={agent.id}
                        className={`rounded-lg border p-4 transition-colors ${
                          isDefault
                            ? 'border-accent-300 bg-accent-50/50'
                            : 'border-primary-200 hover:bg-primary-50'
                        }`}
                      >
                        <div className="flex items-center justify-between">
                          <span className="font-medium text-[13px] text-ink">
                            {agent.name || agent.id}
                          </span>
                          {isDefault ? (
                            <span className="text-[10px] font-medium bg-accent-100 text-accent-700 px-1.5 py-0.5 rounded">
                              default
                            </span>
                          ) : null}
                        </div>
                        <p className="text-[11px] text-primary-500 mt-1 font-mono">
                          {agent.id}
                        </p>
                      </div>
                    )
                  })}
                </div>
              )}
            </>
          )}
        </div>
      </div>

      {selectedHistoryAgent ? (
        <div className="fixed inset-0 z-[90] md:hidden">
          <button
            type="button"
            aria-label="Close history"
            className="absolute inset-0 bg-black/40 backdrop-blur-sm"
            onClick={() => setHistoryAgentId(null)}
          />

          <div className="absolute inset-x-4 top-[12vh] rounded-2xl border border-white/30 bg-white/90 p-4 shadow-lg backdrop-blur-md dark:border-white/10 dark:bg-neutral-900/90">
            <div className="mb-3 flex items-center justify-between">
              <h3 className="text-sm font-semibold text-neutral-900 dark:text-neutral-100">
                {selectedHistoryAgent.name} history
              </h3>
              <button
                type="button"
                className="rounded-lg px-2 py-1 text-xs font-medium text-neutral-600 hover:bg-neutral-100 dark:text-neutral-300 dark:hover:bg-neutral-800"
                onClick={() => setHistoryAgentId(null)}
              >
                Close
              </button>
            </div>

            {selectedHistoryAgent.matchedSessions.length === 0 ? (
              <p className="text-xs text-neutral-600 dark:text-neutral-300">
                No recent sessions for this agent yet.
              </p>
            ) : (
              <div className="max-h-[48vh] space-y-2 overflow-auto">
                {selectedHistoryAgent.matchedSessions
                  .slice(0, 8)
                  .map((session, index) => {
                  const friendlyId = getSessionFriendlyId(session)
                  return (
                    <div
                      key={`${readString(session.key)}-${readString(session.friendlyId)}-${index}`}
                      className="rounded-xl border border-white/30 bg-white/60 p-2.5 dark:border-white/10 dark:bg-neutral-900/40"
                    >
                      <div className="flex items-center justify-between gap-2">
                        <p className="truncate text-xs font-medium text-neutral-900 dark:text-neutral-100">
                          {getSessionTitle(session)}
                        </p>
                        <span className="text-[10px] text-neutral-500 dark:text-neutral-400">
                          {formatRelativeTime(session.updatedAt)}
                        </span>
                      </div>

                      <div className="mt-1 flex items-center justify-between">
                        <span className="text-[10px] font-medium text-neutral-600 dark:text-neutral-300">
                          {readString(session.status) || 'unknown'}
                        </span>
                        {friendlyId ? (
                          <button
                            type="button"
                            onClick={() => {
                              setHistoryAgentId(null)
                              void navigate({
                                to: '/chat/$sessionKey',
                                params: { sessionKey: friendlyId },
                              })
                            }}
                            className="rounded-lg px-2 py-1 text-[10px] font-medium text-accent-700 hover:bg-accent-50 dark:text-accent-300 dark:hover:bg-accent-950/30"
                          >
                            Open Chat
                          </button>
                        ) : null}
                      </div>
                    </div>
                  )
                  })}
              </div>
            )}
          </div>
        </div>
      ) : null}
    </>
  )
}
