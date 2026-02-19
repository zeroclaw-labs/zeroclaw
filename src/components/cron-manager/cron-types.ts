export type CronRunStatus =
  | 'success'
  | 'error'
  | 'running'
  | 'queued'
  | 'unknown'

export type CronRun = {
  id: string
  status: CronRunStatus
  startedAt: string | null
  finishedAt: string | null
  durationMs?: number
  error?: string
  output?: unknown
}

export type CronJob = {
  id: string
  name: string
  schedule: string
  enabled: boolean
  payload?: unknown
  deliveryConfig?: unknown
  status?: string
  description?: string
  lastRun?: CronRun
}

export type CronJobUpsertInput = {
  jobId?: string
  name: string
  schedule: string
  enabled: boolean
  description?: string
  payload?: unknown
  deliveryConfig?: unknown
}

export type CronSortKey = 'name' | 'schedule' | 'lastRun'
export type CronStatusFilter = 'all' | 'enabled' | 'disabled'
