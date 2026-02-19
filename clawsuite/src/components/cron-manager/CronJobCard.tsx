import { HugeiconsIcon } from '@hugeicons/react'
import {
  ArrowDown01Icon,
  ArrowUp01Icon,
  Clock01Icon,
  PlayCircleIcon,
} from '@hugeicons/core-free-icons'
import { AnimatePresence, motion } from 'motion/react'
import {
  formatCronHuman,
  formatDateTime,
  statusBadgeClass,
  statusLabel,
} from './cron-utils'
import type { CronJob } from './cron-types'
import type * as React from 'react'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { cn } from '@/lib/utils'

type CronJobCardProps = {
  job: CronJob
  expanded: boolean
  togglePending: boolean
  runPending: boolean
  deletePending: boolean
  onToggleEnabled: (job: CronJob, enabled: boolean) => void
  onRunNow: (job: CronJob) => void
  onEdit: (job: CronJob) => void
  onDelete: (job: CronJob) => void
  onToggleExpanded: (jobId: string) => void
  children: React.ReactNode
}

export function CronJobCard({
  job,
  expanded,
  togglePending,
  runPending,
  deletePending,
  onToggleEnabled,
  onRunNow,
  onEdit,
  onDelete,
  onToggleExpanded,
  children,
}: CronJobCardProps) {
  return (
    <motion.article
      layout
      className="rounded-2xl border border-primary-200 bg-primary-50/80 p-4 shadow-sm backdrop-blur-xl"
    >
      <header className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="line-clamp-1 text-base font-medium text-ink text-balance">
            {job.name}
          </h3>
          <p className="mt-1 line-clamp-1 text-sm text-primary-600 text-pretty">
            {formatCronHuman(job.schedule)}
          </p>
          <p className="mt-1 truncate text-xs text-primary-600 tabular-nums">
            {job.schedule}
          </p>
        </div>

        <span
          className={cn(
            'inline-flex rounded-md border px-2 py-1 text-xs tabular-nums',
            job.enabled
              ? 'border-primary-300 bg-primary-100 text-primary-800'
              : 'border-primary-300 bg-primary-200/60 text-primary-700',
          )}
        >
          {job.enabled ? 'Enabled' : 'Disabled'}
        </span>
      </header>

      <div className="mt-3 flex items-center justify-between gap-3 rounded-xl border border-primary-200 bg-primary-100/45 p-2.5">
        <div className="min-w-0">
          <div className="flex items-center gap-1.5 text-xs text-primary-600 tabular-nums">
            <HugeiconsIcon icon={Clock01Icon} size={20} strokeWidth={1.5} />
            <span>Last Run</span>
          </div>
          <p className="mt-1 truncate text-sm text-primary-900 tabular-nums">
            {formatDateTime(job.lastRun?.startedAt)}
          </p>
          <span
            className={cn(
              'mt-1 inline-flex rounded-md border px-1.5 py-0.5 text-[11px] tabular-nums',
              statusBadgeClass(job.lastRun?.status ?? 'unknown'),
            )}
          >
            {statusLabel(job.lastRun?.status ?? 'unknown')}
          </span>
        </div>

        <div className="flex flex-wrap items-center justify-end gap-2">
          <Switch
            checked={job.enabled}
            disabled={togglePending}
            onCheckedChange={function onCheckedChange(nextValue) {
              onToggleEnabled(job, Boolean(nextValue))
            }}
            aria-label={`Toggle ${job.name}`}
          />
          <Button
            size="sm"
            variant="outline"
            disabled={runPending}
            onClick={function onClickRunNow() {
              onRunNow(job)
            }}
            className="tabular-nums"
          >
            <HugeiconsIcon icon={PlayCircleIcon} size={20} strokeWidth={1.5} />
            Run Now
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={deletePending}
            onClick={function onClickEdit() {
              onEdit(job)
            }}
            className="tabular-nums"
          >
            Edit
          </Button>
          <Button
            size="sm"
            variant="outline"
            disabled={deletePending}
            onClick={function onClickDelete() {
              onDelete(job)
            }}
            className="tabular-nums"
          >
            {deletePending ? 'Deleting...' : 'Delete'}
          </Button>
        </div>
      </div>

      <div className="mt-3">
        <Button
          size="sm"
          variant="ghost"
          onClick={function onClickExpand() {
            onToggleExpanded(job.id)
          }}
          className="w-full justify-between border border-primary-200 bg-primary-100/50 text-primary-800"
        >
          <span className="tabular-nums">
            {expanded ? 'Hide Details' : 'View Details & History'}
          </span>
          <HugeiconsIcon
            icon={expanded ? ArrowUp01Icon : ArrowDown01Icon}
            size={20}
            strokeWidth={1.5}
          />
        </Button>
      </div>

      <AnimatePresence initial={false}>
        {expanded ? children : null}
      </AnimatePresence>
    </motion.article>
  )
}
