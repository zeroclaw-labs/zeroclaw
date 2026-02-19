import { Task01Icon } from '@hugeicons/core-free-icons'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { useMemo } from 'react'
import { DashboardGlassCard } from './dashboard-glass-card'
import { WidgetShell } from './widget-shell'
import type { CronJob } from '@/components/cron-manager/cron-types'
import type { TaskPriority, TaskStatus } from '@/stores/task-store'
import { fetchCronJobs } from '@/lib/cron-api'
import { cn } from '@/lib/utils'

type TasksWidgetProps = {
  draggable?: boolean
  onRemove?: () => void
}

type DashboardTask = {
  id: string
  title: string
  status: TaskStatus
  priority: TaskPriority
}

const PRIORITY_ORDER: Array<TaskPriority> = ['P0', 'P1', 'P2', 'P3']

function toTaskStatus(job: CronJob): TaskStatus {
  if (!job.enabled) return 'backlog'
  const status = job.lastRun?.status
  if (status === 'running' || status === 'queued') return 'in_progress'
  if (status === 'error') return 'review'
  if (status === 'success') return 'done'
  return 'backlog'
}

function toTaskPriority(job: CronJob): TaskPriority {
  const status = job.lastRun?.status
  if (status === 'error') return 'P0'
  if (status === 'running' || status === 'queued') return 'P1'
  if (!job.enabled) return 'P3'
  return 'P2'
}

function priorityBadgeClass(priority: TaskPriority): string {
  if (priority === 'P0') return 'bg-red-100/80 text-red-700'
  if (priority === 'P1') return 'bg-amber-100/80 text-amber-700'
  if (priority === 'P2') return 'bg-primary-200/65 text-primary-700'
  return 'bg-gray-100/80 text-gray-600'
}

function mobilePriorityBadgeClass(priority: TaskPriority): string {
  if (priority === 'P0') return 'bg-red-100/85 text-red-700 dark:bg-red-900/40 dark:text-red-300'
  if (priority === 'P1') return 'bg-amber-100/85 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300'
  if (priority === 'P2') return 'bg-blue-100/85 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300'
  return 'bg-neutral-100 text-neutral-600 dark:bg-neutral-800 dark:text-neutral-300'
}

function statusDotClass(status: TaskStatus): string {
  if (status === 'in_progress' || status === 'review') return 'bg-amber-500'
  if (status === 'done') return 'bg-emerald-500'
  return 'bg-gray-400'
}

function truncateTaskTitle(title: string): string {
  if (title.length <= 30) return title
  return `${title.slice(0, 29)}…`
}

function toDashboardTask(job: CronJob): DashboardTask {
  return {
    id: job.id,
    title: job.name,
    status: toTaskStatus(job),
    priority: toTaskPriority(job),
  }
}

function mobileStatusRank(status: TaskStatus): number {
  if (status === 'in_progress') return 0
  if (status === 'backlog') return 1
  if (status === 'review') return 2
  return 3
}

export function TasksWidget({ draggable = false, onRemove }: TasksWidgetProps) {
  const navigate = useNavigate()

  const cronJobsQuery = useQuery({
    queryKey: ['cron', 'jobs'],
    queryFn: fetchCronJobs,
    retry: false,
    refetchInterval: 30_000,
  })

  const tasks = useMemo(
    function buildTaskRows() {
      const jobs = Array.isArray(cronJobsQuery.data) ? cronJobsQuery.data : []
      return jobs.map(toDashboardTask)
    },
    [cronJobsQuery.data],
  )

  const sortedTasks = useMemo(
    function sortTasksByPriority() {
      return [...tasks].sort(function sortByPriority(left, right) {
        const leftOrder = PRIORITY_ORDER.indexOf(left.priority)
        const rightOrder = PRIORITY_ORDER.indexOf(right.priority)
        if (leftOrder !== rightOrder) return leftOrder - rightOrder
        return left.title.localeCompare(right.title)
      })
    },
    [tasks],
  )

  const mobilePreviewTasks = useMemo(
    function buildMobilePreviewTasks() {
      return [...tasks]
        .sort(function sortForMobile(left, right) {
          const statusDelta = mobileStatusRank(left.status) - mobileStatusRank(right.status)
          if (statusDelta !== 0) return statusDelta

          const leftPriority = PRIORITY_ORDER.indexOf(left.priority)
          const rightPriority = PRIORITY_ORDER.indexOf(right.priority)
          if (leftPriority !== rightPriority) return leftPriority - rightPriority

          return left.title.localeCompare(right.title)
        })
        .slice(0, 3)
    },
    [tasks],
  )

  const visibleTasks = sortedTasks.slice(0, 4)
  const remainingCount = Math.max(0, sortedTasks.length - visibleTasks.length)
  const activeCount = tasks.filter((task) => task.status !== 'done').length
  const backlogCount = tasks.filter((task) => task.status === 'backlog').length
  const inProgressCount = tasks.filter((task) => task.status === 'in_progress').length
  const doneCount = tasks.filter((task) => task.status === 'done').length
  const errorMessage =
    cronJobsQuery.error instanceof Error ? cronJobsQuery.error.message : null

  return (
    <>
      <WidgetShell
        size="medium"
        title="Tasks"
        icon={Task01Icon}
        action={
          <span className="inline-flex items-center rounded-full border border-primary-200 bg-primary-100/70 px-2 py-0.5 text-[10px] font-medium text-primary-500">
            Backlog {backlogCount} • In progress {inProgressCount} • Done {doneCount}
          </span>
        }
        className="h-full md:hidden"
      >
        {cronJobsQuery.isLoading && tasks.length === 0 ? (
          <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
            Loading tasks…
          </div>
        ) : cronJobsQuery.isError ? (
          <div className="rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-3 text-sm text-amber-700">
            {errorMessage ?? 'Unable to load tasks.'}
          </div>
        ) : tasks.length === 0 ? (
          <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
            No tasks yet
          </div>
        ) : (
          <div className="space-y-2.5">
            <div className="space-y-1.5">
              {mobilePreviewTasks.map(function renderMobileTask(task) {
                return (
                  <article
                    key={task.id}
                    className="flex items-center gap-2 rounded-xl border border-white/30 bg-white/55 px-3 py-2 dark:border-white/10 dark:bg-neutral-900/45"
                  >
                    <span
                      className={cn(
                        'shrink-0 rounded-full px-1.5 py-0.5 text-[10px] font-medium',
                        mobilePriorityBadgeClass(task.priority),
                      )}
                    >
                      {task.priority}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-sm font-medium text-neutral-800 dark:text-neutral-100">
                      {task.title}
                    </span>
                    <span
                      className={cn('size-2 shrink-0 rounded-full', statusDotClass(task.status))}
                    />
                  </article>
                )
              })}
            </div>
          </div>
        )}

        <div className="mt-2 flex justify-end">
          <button
            type="button"
            onClick={() => void navigate({ to: '/cron' })}
            className="inline-flex items-center gap-1 text-xs font-medium text-primary-500 transition-colors hover:text-accent-600"
          >
            View all ›
          </button>
        </div>
      </WidgetShell>

      <div className="hidden h-full md:block">
        <DashboardGlassCard
          title="Tasks"
          titleAccessory={
            <span className="inline-flex items-center rounded-full border border-primary-200 bg-primary-100/70 px-2 py-0.5 text-[11px] font-medium text-primary-500 tabular-nums">
              {activeCount}
            </span>
          }
          tier="tertiary"
          description=""
          icon={Task01Icon}
          draggable={draggable}
          onRemove={onRemove}
          className="h-full rounded-xl border-primary-200 p-3.5 md:p-4 shadow-sm [&_h2]:text-sm [&_h2]:font-semibold [&_h2]:normal-case [&_h2]:text-ink"
        >
          {cronJobsQuery.isLoading && tasks.length === 0 ? (
            <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
              Loading tasks…
            </div>
          ) : cronJobsQuery.isError ? (
            <div className="rounded-lg border border-amber-200 bg-amber-50/80 px-3 py-3 text-sm text-amber-700">
              {errorMessage ?? 'Unable to load tasks.'}
            </div>
          ) : tasks.length === 0 ? (
            <div className="rounded-lg border border-primary-200 bg-primary-100/45 px-3 py-3 text-sm text-primary-600">
              No tasks yet
            </div>
          ) : (
            <div className="space-y-1.5">
              {visibleTasks.map(function renderTask(task, index) {
                return (
                  <article
                    key={task.id}
                    className={cn(
                      'flex items-center gap-2 rounded-lg border border-primary-200 px-2.5 py-2',
                      index % 2 === 0 ? 'bg-primary-50/90' : 'bg-primary-100/60',
                    )}
                  >
                    <span
                      className={cn('size-2 shrink-0 rounded-full', statusDotClass(task.status))}
                    />
                    <span className="min-w-0 flex-1 truncate text-sm text-ink">
                      {truncateTaskTitle(task.title)}
                    </span>
                    <span
                      className={cn(
                        'shrink-0 rounded-full px-1.5 py-0.5 text-[10px] font-medium',
                        priorityBadgeClass(task.priority),
                      )}
                    >
                      {task.priority}
                    </span>
                  </article>
                )
              })}

              {remainingCount > 0 ? (
                <p className="px-1 text-xs text-primary-500">+{remainingCount} more</p>
              ) : null}
            </div>
          )}

          <div className="mt-2 flex justify-end">
            <button
              type="button"
              onClick={() => void navigate({ to: '/cron' })}
              className="inline-flex items-center gap-1 text-xs font-medium text-primary-500 transition-colors hover:text-accent-600"
            >
              View all →
            </button>
          </div>
        </DashboardGlassCard>
      </div>
    </>
  )
}
