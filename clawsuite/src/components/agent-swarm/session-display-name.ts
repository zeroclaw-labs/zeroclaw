import type { SwarmSession } from '@/stores/agent-swarm-store'

const RAW_SESSION_NAME_PATTERN = /^[0-9a-f-]{8,}$/i

function readText(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function pickCandidateName(session: SwarmSession): string {
  const row = session as Record<string, unknown>
  const candidates = [
    session.label,
    session.title,
    session.derivedTitle,
    row.name,
    row.displayName,
    row.sessionName,
    row.agentName,
  ]

  for (const candidate of candidates) {
    const text = readText(candidate)
    if (text) return text
  }

  return ''
}

function derivePrefix(session: SwarmSession): string {
  const kind = readText(session.kind).toLowerCase()
  if (kind.includes('subagent')) return 'Agent'
  if (kind.includes('swarm')) return 'Swarm'
  if (kind.includes('task') || kind.includes('cron')) return 'Task'
  return 'Session'
}

function deriveSuffix(session: SwarmSession): string {
  const row = session as Record<string, unknown>
  const rawId =
    readText(session.friendlyId) ||
    readText(session.key) ||
    readText(row.id) ||
    readText(row.sessionId)
  if (!rawId) return '0000'
  const compact = rawId.replace(/[^a-zA-Z0-9]/g, '')
  const source = compact.length > 0 ? compact : rawId
  return source.slice(-4).toUpperCase()
}

export function getSwarmSessionDisplayName(session: SwarmSession): string {
  const candidate = pickCandidateName(session)
  if (candidate && !RAW_SESSION_NAME_PATTERN.test(candidate)) {
    return candidate
  }
  return `${derivePrefix(session)} ${deriveSuffix(session)}`
}
