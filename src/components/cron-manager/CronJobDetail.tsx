import { HugeiconsIcon } from '@hugeicons/react'
import { Clock01Icon } from '@hugeicons/core-free-icons'
import { motion } from 'motion/react'
import {
  formatCronHuman,
  formatDateTime,
  formatDuration,
  statusBadgeClass,
  statusLabel,
} from './cron-utils'
import type { CronJob, CronRun } from './cron-types'
import { cn } from '@/lib/utils'

type CronJobDetailProps = {
  job: CronJob
  runs: Array<CronRun>
  loading: boolean
  error: string | null
}

function stringifyBlock(value: unknown): string {
  if (value == null) return 'None'
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

export function CronJobDetail({
  job,
  runs,
  loading,
  error,
}: CronJobDetailProps) {
  return (
    <motion.section
      initial={{ opacity: 0, height: 0 }}
      animate={{ opacity: 1, height: 'auto' }}
      exit={{ opacity: 0, height: 0 }}
      transition={{ duration: 0.2, ease: 'easeOut' }}
      className="mt-3 overflow-hidden rounded-xl border border-primary-200 bg-primary-100/45"
    >
      <div className="grid grid-cols-1 gap-3 border-b border-primary-200 p-3 md:grid-cols-3">
        <div>
          <p className="text-xs text-primary-600 tabular-nums">Schedule</p>
          <p className="mt-1 text-sm font-medium text-primary-900 text-pretty">
            {formatCronHuman(job.schedule)}
          </p>
          <p className="mt-1 truncate text-xs text-primary-600 tabular-nums">
            {job.schedule}
          </p>
        </div>
        <div>
          <p className="text-xs text-primary-600 tabular-nums">Last Run</p>
          <p className="mt-1 text-sm text-primary-900 tabular-nums">
            {formatDateTime(job.lastRun?.startedAt)}
          </p>
          <p className="mt-1 text-xs text-primary-600 tabular-nums">
            Duration: {formatDuration(job.lastRun?.durationMs)}
          </p>
        </div>
        <div>
          <p className="text-xs text-primary-600 tabular-nums">
            Current Status
          </p>
          <span
            className={cn(
              'mt-1 inline-flex rounded-md border px-2 py-1 text-xs tabular-nums',
              statusBadgeClass(job.lastRun?.status ?? 'unknown'),
            )}
          >
            {statusLabel(job.lastRun?.status ?? 'unknown')}
          </span>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-3 border-b border-primary-200 p-3 lg:grid-cols-2">
        <div className="min-w-0">
          <h4 className="text-xs text-primary-600 tabular-nums">Payload</h4>
          <pre className="mt-1 max-h-32 overflow-auto rounded-md border border-primary-200 bg-primary-50/80 p-2 text-xs text-primary-800 tabular-nums">
            {stringifyBlock(job.payload)}
          </pre>
        </div>
        <div className="min-w-0">
          <h4 className="text-xs text-primary-600 tabular-nums">
            Delivery Config
          </h4>
          <pre className="mt-1 max-h-32 overflow-auto rounded-md border border-primary-200 bg-primary-50/80 p-2 text-xs text-primary-800 tabular-nums">
            {stringifyBlock(job.deliveryConfig)}
          </pre>
        </div>
      </div>

      <div className="p-3">
        <div className="mb-2 flex items-center gap-2 text-xs text-primary-600 tabular-nums">
          <HugeiconsIcon icon={Clock01Icon} size={20} strokeWidth={1.5} />
          <span>Run History (last 10)</span>
        </div>

        {loading ? (
          <p className="rounded-md border border-primary-200 bg-primary-50/80 p-3 text-sm text-primary-700 text-pretty">
            Loading run history...
          </p>
        ) : error ? (
          <p className="rounded-md border border-accent-500/40 bg-accent-500/10 p-3 text-sm text-accent-500 text-pretty">
            {error}
          </p>
        ) : runs.length === 0 ? (
          <p className="rounded-md border border-primary-200 bg-primary-50/80 p-3 text-sm text-primary-700 text-pretty">
            No runs recorded yet.
          </p>
        ) : (
          <div className="space-y-2">
            {runs.map(function mapRun(run) {
              return (
                <article
                  key={run.id}
                  className="grid grid-cols-[1fr_auto_auto] gap-2 rounded-md border border-primary-200 bg-primary-50/80 px-3 py-2"
                >
                  <div className="min-w-0">
                    <p className="truncate text-xs text-primary-600 tabular-nums">
                      {run.id}
                    </p>
                    <p className="text-xs text-primary-700 tabular-nums">
                      {formatDateTime(run.startedAt)}
                    </p>
                    {run.error ? (
                      <p className="mt-1 line-clamp-1 text-xs text-accent-500 text-pretty">
                        {run.error}
                      </p>
                    ) : null}
                  </div>
                  <span
                    className={cn(
                      'inline-flex h-fit rounded-md border px-2 py-1 text-xs tabular-nums',
                      statusBadgeClass(run.status),
                    )}
                  >
                    {statusLabel(run.status)}
                  </span>
                  <span className="text-xs text-primary-700 tabular-nums">
                    {formatDuration(run.durationMs)}
                  </span>
                </article>
              )
            })}
          </div>
        )}
      </div>
    </motion.section>
  )
}
