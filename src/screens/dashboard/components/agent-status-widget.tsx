import { UserGroupIcon } from '@hugeicons/core-free-icons'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { DashboardGlassCard } from './dashboard-glass-card'
import type { SessionMeta } from '@/screens/chat/types'
import { cn } from '@/lib/utils'

type SessionsApiResponse = {
  sessions?: Array<Record<string, unknown>>
}

type AgentRow = {
  id: string
  name: string
  model: string
  status: string
  elapsedSeconds: number
}

type SessionAgentSource = SessionMeta & Record<string, unknown>

type AgentStatusWidgetProps = {
  draggable?: boolean
  onRemove?: () => void
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function readTimestamp(value: unknown): number {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return value > 1_000_000_000_000 ? value : value * 1000
  }
  if (typeof value === 'string') {
    const asNumber = Number(value)
    if (Number.isFinite(asNumber)) {
      return asNumber > 1_000_000_000_000 ? asNumber : asNumber * 1000
    }
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return parsed
  }
  return 0
}

function toFriendlyId(key: string): string {
  if (key.length === 0) return 'main'
  const parts = key.split(':')
  const tail = parts[parts.length - 1]
  return tail && tail.trim().length > 0 ? tail.trim() : key
}

function normalizeStatus(value: unknown): string {
  const status = readString(value).toLowerCase()
  if (status.length === 0) return 'running'
  if (status === 'in_progress') return 'running'
  if (status === 'streaming') return 'running'
  return status
}

/** Strip raw system-prompt text and internal noise from session names */
function cleanSessionName(raw: string): string {
  if (!raw) return ''
  // Strip leading bracketed timestamps: [Sat 2026-02-07 01:35 EST]
  let cleaned = raw.replace(/^\[.*?\]\s*/, '')
  // Strip "A new session was started via /new or /reset..." style prompt leaks
  if (/^a new session was started/i.test(cleaned)) return ''
  // Strip message_id references
  cleaned = cleaned.replace(/\[?message_id:\s*\S+\]?/gi, '').trim()
  // Truncate at 60 chars
  if (cleaned.length > 60) cleaned = `${cleaned.slice(0, 57)}…`
  return cleaned
}

function deriveName(session: SessionAgentSource): string {
  const label = cleanSessionName(readString(session.label))
  if (label) return label
  const derived = cleanSessionName(readString(session.derivedTitle))
  if (derived) return derived
  const title = cleanSessionName(readString(session.title))
  if (title) return title
  const friendlyId = readString(session.friendlyId)
  return friendlyId === 'main' ? 'Main Session' : `Session ${friendlyId}`
}

function deriveModel(session: SessionAgentSource): string {
  const lastMessage =
    session.lastMessage && typeof session.lastMessage === 'object'
      ? (session.lastMessage as Record<string, unknown>)
      : {}
  const details =
    lastMessage.details && typeof lastMessage.details === 'object'
      ? (lastMessage.details as Record<string, unknown>)
      : {}

  return (
    readString(session.model) ||
    readString(session.currentModel) ||
    readString(details.model) ||
    readString(details.agentModel) ||
    'unknown'
  )
}

function formatRelativeAge(totalSeconds: number): string {
  const safe = Math.max(0, Math.floor(totalSeconds))
  if (safe < 60) return 'just now'
  const minutes = Math.floor(safe / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  const days = Math.floor(hours / 24)
  return `${days}d ago`
}

function formatModelShort(raw: string): string {
  if (!raw || raw === 'unknown') return ''
  // Strip provider prefix: "anthropic/claude-opus-4-6" → "claude-opus-4-6"
  const name = raw.includes('/') ? raw.split('/').pop()! : raw
  const lower = name.toLowerCase()
  if (lower.includes('opus')) {
    const m = name.match(/opus[- ]?(\d+)[- ]?(\d+)/i)
    return m ? `Opus ${m[1]}.${m[2]}` : 'Opus'
  }
  if (lower.includes('sonnet')) {
    const m = name.match(/sonnet[- ]?(\d+)[- ]?(\d+)/i)
    return m ? `Sonnet ${m[1]}.${m[2]}` : 'Sonnet'
  }
  if (lower.includes('haiku')) return 'Haiku'
  if (lower.includes('codex')) return 'Codex'
  if (lower.includes('gpt')) return name.replace('gpt-', 'GPT-')
  if (lower.includes('gemini')) return 'Gemini'
  return name
}

function compareSessionRecency(
  a: SessionAgentSource,
  b: SessionAgentSource,
): number {
  const aTime =
    readTimestamp(a.updatedAt) ||
    readTimestamp(a.startedAt) ||
    readTimestamp(a.createdAt)
  const bTime =
    readTimestamp(b.updatedAt) ||
    readTimestamp(b.startedAt) ||
    readTimestamp(b.createdAt)
  return bTime - aTime
}

function toAgentRow(session: SessionAgentSource, now: number): AgentRow {
  const status = normalizeStatus(session.status)
  const startedAt =
    readTimestamp(session.startedAt) ||
    readTimestamp(session.createdAt) ||
    readTimestamp(session.updatedAt)
  const elapsedSeconds =
    startedAt > 0 ? Math.floor(Math.max(0, now - startedAt) / 1000) : 0

  return {
    id:
      readString(session.key) ||
      readString(session.friendlyId) ||
      `agent-${now}`,
    name: deriveName(session),
    model: deriveModel(session),
    status,
    elapsedSeconds,
  }
}

async function fetchSessions(): Promise<Array<SessionAgentSource>> {
  const response = await fetch('/api/sessions')
  if (!response.ok) return []

  const payload = (await response.json()) as SessionsApiResponse
  const rows = Array.isArray(payload.sessions) ? payload.sessions : []

  return rows.map(function mapSession(row, index) {
    const key = readString(row.key) || `session-${index + 1}`
    const friendlyId = readString(row.friendlyId) || toFriendlyId(key)
    const label = readString(row.label) || undefined
    const title = readString(row.title) || undefined
    const derivedTitle = readString(row.derivedTitle) || undefined
    const updatedAtValue = readTimestamp(row.updatedAt)

    return {
      ...row,
      key,
      friendlyId,
      label,
      title,
      derivedTitle,
      updatedAt: updatedAtValue > 0 ? updatedAtValue : undefined,
    } as SessionAgentSource
  })
}

export function AgentStatusWidget({
  draggable = false,
  onRemove,
}: AgentStatusWidgetProps) {
  const sessionsQuery = useQuery({
    queryKey: ['dashboard', 'active-agent-sessions'],
    queryFn: fetchSessions,
    refetchInterval: 15_000,
  })

  const agents = useMemo(
    function buildAgents() {
      const rows = Array.isArray(sessionsQuery.data) ? sessionsQuery.data : []
      if (rows.length === 0) return []

      const now = Date.now()
      return [...rows]
        .sort(compareSessionRecency)
        .map(function mapAgent(session) {
          return toAgentRow(session, now)
        })
    },
    [sessionsQuery.data],
  )

  return (
    <DashboardGlassCard
      title="Active Agents"
      tier="primary"
      description=""
      icon={UserGroupIcon}
      titleAccessory={
        <span className="inline-flex items-center rounded-full border border-primary-200 bg-primary-100/70 px-2 py-0.5 text-[11px] font-medium text-primary-500 tabular-nums">
          {agents.length}
        </span>
      }
      draggable={draggable}
      onRemove={onRemove}
      className="h-full rounded-xl border-primary-200 p-3.5 md:p-4 shadow-sm [&_h2]:text-sm [&_h2]:font-semibold [&_h2]:normal-case [&_h2]:text-ink"
    >
      {sessionsQuery.isLoading && agents.length === 0 ? (
        <div className="flex h-32 items-center justify-center gap-3 rounded-lg border border-primary-200 bg-primary-100/45">
          <span
            className="size-4 animate-spin rounded-full border-2 border-primary-300 border-t-accent-600"
            role="status"
            aria-label="Loading"
          />
          <span className="text-sm text-primary-600">Loading sessions…</span>
        </div>
      ) : agents.length === 0 ? (
        <div className="flex h-32 flex-col items-center justify-center gap-1 rounded-lg border border-primary-200 bg-primary-100/45">
          <p className="text-sm font-semibold text-ink">No active sessions</p>
          <p className="text-xs text-primary-500">
            Active chat sessions and agents will appear here
          </p>
        </div>
      ) : (
        <div className="max-h-80 space-y-2 overflow-y-auto">
          {agents.map(function renderAgent(agent, index) {
            const model = formatModelShort(agent.model)
            return (
              <article
                key={agent.id}
                className={cn(
                  'flex items-center gap-2.5 rounded-lg border border-primary-200 px-3.5 py-2.5 text-sm',
                  index % 2 === 0 ? 'bg-primary-50/90' : 'bg-primary-100/55',
                )}
              >
                <span
                  className={cn(
                    'size-1.5 shrink-0 rounded-full',
                    agent.status === 'running'
                      ? 'bg-accent-500'
                      : 'bg-primary-300',
                  )}
                />
                <span className="min-w-0 flex-1 truncate text-sm font-semibold text-ink">
                  {agent.name}
                </span>
                {model ? (
                  <span className="shrink-0 rounded-full border border-accent-200 bg-accent-100/55 px-2 py-0.5 text-xs font-medium text-accent-700">
                    {model}
                  </span>
                ) : null}
                <span className="shrink-0 font-mono text-xs text-primary-400 tabular-nums">
                  {formatRelativeAge(agent.elapsedSeconds)}
                </span>
              </article>
            )
          })}
        </div>
      )}
    </DashboardGlassCard>
  )
}
