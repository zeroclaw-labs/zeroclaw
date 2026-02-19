import { useQuery } from '@tanstack/react-query'

export type CliAgentStatus = 'running' | 'finished'

export type CliAgent = {
  pid: number
  name: string
  task: string
  runtimeSeconds: number
  status: CliAgentStatus
}

type CliAgentsResponse = {
  agents?: Array<unknown>
}

function toRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object') return null
  if (Array.isArray(value)) return null
  return value as Record<string, unknown>
}

function isCliAgentStatus(value: unknown): value is CliAgentStatus {
  return value === 'running' || value === 'finished'
}

function toCliAgent(value: unknown): CliAgent | null {
  const record = toRecord(value)
  if (!record) return null

  const pid = record.pid
  const name = record.name
  const task = record.task
  const runtimeSeconds = record.runtimeSeconds
  const status = record.status

  if (typeof pid !== 'number' || !Number.isFinite(pid)) return null
  if (typeof name !== 'string' || !name.trim()) return null
  if (typeof task !== 'string') return null
  if (
    typeof runtimeSeconds !== 'number' ||
    !Number.isFinite(runtimeSeconds) ||
    runtimeSeconds < 0
  ) {
    return null
  }
  if (!isCliAgentStatus(status)) return null

  return {
    pid,
    name,
    task: task.trim() || 'No task description',
    runtimeSeconds: Math.floor(runtimeSeconds),
    status,
  }
}

export async function fetchCliAgents(): Promise<Array<CliAgent>> {
  try {
    const response = await fetch('/api/cli-agents')
    if (!response.ok) return []

    const payload = (await response.json()) as CliAgentsResponse
    if (!Array.isArray(payload.agents)) return []

    return payload.agents
      .map(function mapCliAgent(item) {
        return toCliAgent(item)
      })
      .filter(function isPresent(item): item is CliAgent {
        return item !== null
      })
  } catch {
    return []
  }
}

export function useCliAgents() {
  return useQuery({
    queryKey: ['sidebar', 'cli-agents'],
    queryFn: fetchCliAgents,
    refetchInterval: 5_000,
    retry: false,
  })
}
