import type {
  CronJob,
  CronRun,
  CronRunStatus,
} from '@/components/cron-manager/cron-types'

type CronJobsResponse = {
  jobs?: Array<Record<string, unknown>>
}

type CronRunsResponse = {
  runs?: Array<Record<string, unknown>>
}

type ToggleCronPayload = {
  ok?: boolean
  enabled?: boolean
}

type RunCronPayload = {
  ok?: boolean
}

export type UpsertCronJobInput = {
  jobId?: string
  name: string
  schedule: string
  enabled: boolean
  description?: string
  payload?: unknown
  deliveryConfig?: unknown
}

type UpsertCronPayload = {
  ok?: boolean
  jobId?: string
}

function normalizeStatus(value: unknown): CronRunStatus {
  if (typeof value !== 'string') return 'unknown'
  const normalized = value.trim().toLowerCase()
  if (
    normalized.includes('success') ||
    normalized === 'ok' ||
    normalized === 'completed'
  ) {
    return 'success'
  }
  if (normalized.includes('error') || normalized.includes('fail')) {
    return 'error'
  }
  if (normalized.includes('run')) return 'running'
  if (normalized.includes('queue') || normalized.includes('pending'))
    return 'queued'
  return 'unknown'
}

function normalizeTimestamp(value: unknown): string | null {
  if (typeof value === 'string' && value.trim().length > 0) {
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return new Date(parsed).toISOString()
    return value.trim()
  }
  if (typeof value === 'number' && Number.isFinite(value)) {
    const milliseconds = value > 1_000_000_000_000 ? value : value * 1000
    return new Date(milliseconds).toISOString()
  }
  return null
}

function normalizeRun(row: Record<string, unknown>, index: number): CronRun {
  return {
    id:
      (typeof row.id === 'string' && row.id) ||
      (typeof row.runId === 'string' && row.runId) ||
      `run-${index}`,
    status: normalizeStatus(row.status ?? row.state ?? row.result),
    startedAt: normalizeTimestamp(
      row.startedAt ?? row.started_at ?? row.createdAt ?? row.timestamp,
    ),
    finishedAt: normalizeTimestamp(
      row.finishedAt ?? row.finished_at ?? row.completedAt,
    ),
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

function normalizeJob(row: Record<string, unknown>, index: number): CronJob {
  const lastRunRow = row.lastRun
  const lastRun =
    lastRunRow && typeof lastRunRow === 'object'
      ? normalizeRun(lastRunRow as Record<string, unknown>, index)
      : normalizeRun(
          {
            id: row.lastRunId,
            status: row.lastRunStatus,
            startedAt: row.lastRunAt,
            finishedAt: row.lastRunCompletedAt,
            durationMs: row.lastRunDurationMs,
            error: row.lastRunError,
          },
          index,
        )

  return {
    id:
      (typeof row.id === 'string' && row.id) ||
      (typeof row.jobId === 'string' && row.jobId) ||
      `job-${index}`,
    name:
      (typeof row.name === 'string' && row.name) ||
      (typeof row.title === 'string' && row.title) ||
      `Cron Job ${index + 1}`,
    schedule:
      (typeof row.schedule === 'string' && row.schedule) ||
      (typeof row.cron === 'string' && row.cron) ||
      '* * * * *',
    enabled: Boolean(row.enabled),
    payload: row.payload,
    deliveryConfig: row.deliveryConfig,
    status: typeof row.status === 'string' ? row.status : undefined,
    description:
      typeof row.description === 'string' ? row.description : undefined,
    lastRun,
  }
}

async function readError(response: Response): Promise<string> {
  try {
    const payload = (await response.json()) as Record<string, unknown>
    if (typeof payload.error === 'string') return payload.error
    if (typeof payload.message === 'string') return payload.message
    return JSON.stringify(payload)
  } catch {
    const text = await response.text().catch(() => '')
    return text || response.statusText || 'Request failed'
  }
}

export async function fetchCronJobs(): Promise<Array<CronJob>> {
  const response = await fetch('/api/cron')
  if (!response.ok) {
    throw new Error(await readError(response))
  }

  const payload = (await response.json()) as CronJobsResponse
  const rows = Array.isArray(payload.jobs) ? payload.jobs : []
  return rows.map(function mapJob(job, index) {
    return normalizeJob(job, index)
  })
}

export async function fetchCronRuns(jobId: string): Promise<Array<CronRun>> {
  const response = await fetch(
    `/api/cron/runs/${encodeURIComponent(jobId)}?limit=10`,
  )
  if (!response.ok) {
    throw new Error(await readError(response))
  }

  const payload = (await response.json()) as CronRunsResponse
  const rows = Array.isArray(payload.runs) ? payload.runs : []
  return rows.map(function mapRun(run, index) {
    return normalizeRun(run, index)
  })
}

export async function runCronJob(jobId: string): Promise<RunCronPayload> {
  const response = await fetch('/api/cron/run', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ jobId }),
  })

  if (!response.ok) {
    throw new Error(await readError(response))
  }

  return (await response.json()) as RunCronPayload
}

export async function toggleCronJob(payload: {
  jobId: string
  enabled: boolean
}): Promise<ToggleCronPayload> {
  const response = await fetch('/api/cron/toggle', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(payload),
  })

  if (!response.ok) {
    throw new Error(await readError(response))
  }

  return (await response.json()) as ToggleCronPayload
}

export async function upsertCronJob(
  payload: UpsertCronJobInput,
): Promise<UpsertCronPayload> {
  const response = await fetch('/api/cron/upsert', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify(payload),
  })

  if (!response.ok) {
    throw new Error(await readError(response))
  }

  return (await response.json()) as UpsertCronPayload
}

export async function deleteCronJob(jobId: string): Promise<{ ok?: boolean }> {
  const response = await fetch('/api/cron/delete', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ jobId }),
  })

  if (!response.ok) {
    throw new Error(await readError(response))
  }

  return (await response.json()) as { ok?: boolean }
}
