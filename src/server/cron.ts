import { gatewayRpc } from './gateway'

export async function gatewayCronRpc<TPayload = unknown>(
  methods: Array<string>,
  params?: unknown,
): Promise<TPayload> {
  let lastError: unknown = null

  for (const method of methods) {
    try {
      return await gatewayRpc<TPayload>(method, params)
    } catch (error) {
      lastError = error
    }
  }

  if (lastError instanceof Error) {
    throw lastError
  }
  throw new Error('Cron gateway request failed')
}

export function normalizeCronBool(value: unknown, fallback = false): boolean {
  if (typeof value === 'boolean') return value
  if (typeof value === 'number') return value > 0
  if (typeof value === 'string') {
    const normalized = value.trim().toLowerCase()
    if (['true', '1', 'enabled', 'active', 'on'].includes(normalized))
      return true
    if (['false', '0', 'disabled', 'inactive', 'off'].includes(normalized)) {
      return false
    }
  }
  return fallback
}

function normalizeTimestamp(value: unknown): string | null {
  if (typeof value === 'number' && Number.isFinite(value)) {
    const milliseconds = value > 1_000_000_000_000 ? value : value * 1000
    return new Date(milliseconds).toISOString()
  }
  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return new Date(parsed).toISOString()
    return value.trim()
  }
  return null
}

function normalizeRunStatus(value: unknown): string {
  if (typeof value === 'string' && value.trim().length > 0) {
    return value.trim().toLowerCase()
  }
  return 'unknown'
}

function asRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object') return {}
  return value as Record<string, unknown>
}

function normalizeRun(run: unknown, index: number): Record<string, unknown> {
  const row = asRecord(run)
  const startedAt = normalizeTimestamp(
    row.startedAt ??
      row.started_at ??
      row.createdAt ??
      row.timestamp ??
      row.time,
  )
  const finishedAt = normalizeTimestamp(
    row.finishedAt ?? row.finished_at ?? row.completedAt ?? row.endedAt,
  )

  return {
    id:
      (typeof row.id === 'string' && row.id) ||
      (typeof row.runId === 'string' && row.runId) ||
      (typeof row.key === 'string' && row.key) ||
      `run-${index}`,
    status: normalizeRunStatus(row.status ?? row.state ?? row.result),
    startedAt,
    finishedAt,
    durationMs:
      typeof row.durationMs === 'number'
        ? row.durationMs
        : typeof row.duration === 'number'
          ? row.duration
          : undefined,
    error:
      typeof row.error === 'string'
        ? row.error
        : typeof row.message === 'string'
          ? row.message
          : undefined,
    output: row.output,
  }
}

export function normalizeCronRuns(
  payload: unknown,
): Array<Record<string, unknown>> {
  const root = asRecord(payload)
  const rows = Array.isArray(payload)
    ? payload
    : Array.isArray(root.runs)
      ? root.runs
      : Array.isArray(root.items)
        ? root.items
        : Array.isArray(root.history)
          ? root.history
          : []

  return rows.map(function mapRun(run, index) {
    return normalizeRun(run, index)
  })
}

export function normalizeCronJobs(
  payload: unknown,
): Array<Record<string, unknown>> {
  const root = asRecord(payload)
  const rows = Array.isArray(payload)
    ? payload
    : Array.isArray(root.jobs)
      ? root.jobs
      : Array.isArray(root.items)
        ? root.items
        : Array.isArray(root.entries)
          ? root.entries
          : Array.isArray(root.tasks)
            ? root.tasks
            : []

  return rows.map(function mapJob(job, index) {
    const row = asRecord(job)
    const lastRunRecord = asRecord(row.lastRun)
    const enabled = normalizeCronBool(
      row.enabled ?? row.isEnabled ?? row.active,
      normalizeRunStatus(row.status) !== 'disabled',
    )

    return {
      id:
        (typeof row.id === 'string' && row.id) ||
        (typeof row.jobId === 'string' && row.jobId) ||
        (typeof row.key === 'string' && row.key) ||
        `job-${index}`,
      name:
        (typeof row.name === 'string' && row.name) ||
        (typeof row.title === 'string' && row.title) ||
        `Cron Job ${index + 1}`,
      schedule: (() => {
        // Gateway returns schedule as object: { kind: "cron", expr: "0 13 * * *", tz: "..." }
        if (row.schedule && typeof row.schedule === 'object') {
          const sched = row.schedule as Record<string, unknown>
          return (typeof sched.expr === 'string' && sched.expr) ||
            (typeof sched.expression === 'string' && sched.expression) ||
            '* * * * *'
        }
        return (typeof row.schedule === 'string' && row.schedule) ||
          (typeof row.cron === 'string' && row.cron) ||
          (typeof row.expression === 'string' && row.expression) ||
          '* * * * *'
      })(),
      enabled,
      payload: row.payload ?? row.data ?? row.body ?? null,
      deliveryConfig:
        row.deliveryConfig ??
        row.delivery ??
        row.config ??
        row.transport ??
        null,
      status: (() => {
        // Gateway puts run state in `state` object
        const stateObj = asRecord(row.state)
        if (stateObj.lastStatus) return normalizeRunStatus(stateObj.lastStatus)
        return normalizeRunStatus(row.status)
      })(),
      description:
        typeof row.description === 'string' ? row.description : undefined,
      lastRun: (() => {
        // Gateway returns state: { lastRunAtMs, lastStatus, lastDurationMs, nextRunAtMs }
        const stateObj = asRecord(row.state)
        if (stateObj.lastRunAtMs || stateObj.lastStatus) {
          return {
            id: null,
            status: normalizeRunStatus(stateObj.lastStatus),
            startedAt: normalizeTimestamp(stateObj.lastRunAtMs),
            finishedAt: null,
            durationMs: typeof stateObj.lastDurationMs === 'number' ? stateObj.lastDurationMs : null,
          }
        }
        return Object.keys(lastRunRecord).length > 0
          ? normalizeRun(lastRunRecord, 0)
          : {
              id: null,
              status: normalizeRunStatus(
                row.lastRunStatus ?? row.lastStatus ?? row.result,
              ),
              startedAt: normalizeTimestamp(
                row.lastRunAt ?? row.lastRunTime ?? row.lastExecutedAt,
              ),
              finishedAt: normalizeTimestamp(
                row.lastRunCompletedAt ?? row.lastCompletedAt,
              ),
              durationMs:
                typeof row.lastRunDurationMs === 'number'
                  ? row.lastRunDurationMs
                  : undefined,
              error:
                typeof row.lastRunError === 'string'
                  ? row.lastRunError
                  : undefined,
            }
      })(),
    }
  })
}
