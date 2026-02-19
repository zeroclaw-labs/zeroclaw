import { HugeiconsIcon } from '@hugeicons/react'
import { Search01Icon } from '@hugeicons/core-free-icons'
import { motion } from 'motion/react'
import { CronJobCard } from './CronJobCard'
import { CronJobDetail } from './CronJobDetail'
import { sortCronJobs } from './cron-utils'
import type {
  CronJob,
  CronRun,
  CronSortKey,
  CronStatusFilter,
} from './cron-types'
import { cn } from '@/lib/utils'

type CronJobListProps = {
  jobs: Array<CronJob>
  runsByJobId: Record<string, Array<CronRun>>
  loadingRunsForJobId: string | null
  runHistoryError: string | null
  selectedJobId: string | null
  searchText: string
  sortBy: CronSortKey
  statusFilter: CronStatusFilter
  onSearchTextChange: (value: string) => void
  onSortByChange: (value: CronSortKey) => void
  onStatusFilterChange: (value: CronStatusFilter) => void
  onToggleEnabled: (job: CronJob, enabled: boolean) => void
  onRunNow: (job: CronJob) => void
  onEdit: (job: CronJob) => void
  onDelete: (job: CronJob) => void
  onToggleExpanded: (jobId: string) => void
  togglePendingJobId: string | null
  runPendingJobId: string | null
  deletePendingJobId: string | null
}

function matchesSearch(job: CronJob, searchText: string): boolean {
  const needle = searchText.trim().toLowerCase()
  if (!needle) return true
  return (
    job.name.toLowerCase().includes(needle) ||
    job.schedule.toLowerCase().includes(needle) ||
    (job.description ?? '').toLowerCase().includes(needle)
  )
}

function matchesStatus(job: CronJob, filter: CronStatusFilter): boolean {
  if (filter === 'all') return true
  if (filter === 'enabled') return job.enabled
  return !job.enabled
}

export function CronJobList({
  jobs,
  runsByJobId,
  loadingRunsForJobId,
  runHistoryError,
  selectedJobId,
  searchText,
  sortBy,
  statusFilter,
  onSearchTextChange,
  onSortByChange,
  onStatusFilterChange,
  onToggleEnabled,
  onRunNow,
  onEdit,
  onDelete,
  onToggleExpanded,
  togglePendingJobId,
  runPendingJobId,
  deletePendingJobId,
}: CronJobListProps) {
  const filtered = sortCronJobs(
    jobs.filter(function filterJob(job) {
      return matchesSearch(job, searchText) && matchesStatus(job, statusFilter)
    }),
    sortBy,
  )

  return (
    <section className="space-y-3">
      <div className="rounded-2xl border border-primary-200 bg-primary-50/85 p-3 backdrop-blur-xl">
        <div className="grid grid-cols-1 gap-2 md:grid-cols-[1fr_auto_auto]">
          <label className="relative min-w-0">
            <HugeiconsIcon
              icon={Search01Icon}
              size={20}
              strokeWidth={1.5}
              className="pointer-events-none absolute top-1/2 left-2 -translate-y-1/2 text-primary-500"
            />
            <input
              type="text"
              value={searchText}
              onChange={function onChangeSearch(event) {
                onSearchTextChange(event.target.value)
              }}
              placeholder="Search jobs by name or schedule"
              className="h-9 w-full rounded-lg border border-primary-200 bg-primary-100/60 pr-3 pl-9 text-sm text-primary-900 outline-none transition-colors focus:border-primary-400"
            />
          </label>

          <select
            value={sortBy}
            onChange={function onChangeSort(event) {
              onSortByChange(event.target.value as CronSortKey)
            }}
            className="h-9 rounded-lg border border-primary-200 bg-primary-100/60 px-3 text-sm text-primary-900 outline-none focus:border-primary-400 tabular-nums"
          >
            <option value="name">Sort: Name</option>
            <option value="schedule">Sort: Schedule</option>
            <option value="lastRun">Sort: Last Run</option>
          </select>

          <div className="inline-flex rounded-lg border border-primary-200 bg-primary-100/60 p-1">
            {(['all', 'enabled', 'disabled'] as const).map(
              function mapFilter(filterValue) {
                return (
                  <button
                    key={filterValue}
                    type="button"
                    onClick={function onClickFilter() {
                      onStatusFilterChange(filterValue)
                    }}
                    className={cn(
                      'rounded-md px-2.5 py-1.5 text-xs tabular-nums transition-colors',
                      statusFilter === filterValue
                        ? 'bg-primary-900 text-primary-50'
                        : 'text-primary-700 hover:bg-primary-200',
                    )}
                  >
                    {filterValue === 'all'
                      ? 'All'
                      : filterValue === 'enabled'
                        ? 'Enabled'
                        : 'Disabled'}
                  </button>
                )
              },
            )}
          </div>
        </div>
      </div>

      {filtered.length === 0 ? (
        <div className="rounded-2xl border border-primary-200 bg-primary-50/80 p-8 text-center text-sm text-primary-600 text-pretty">
          No cron jobs matched your filters.
        </div>
      ) : (
        <motion.div
          layout
          className="grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3"
        >
          {filtered.map(function mapJob(job) {
            const isExpanded = selectedJobId === job.id
            return (
              <CronJobCard
                key={job.id}
                job={job}
                expanded={isExpanded}
                togglePending={togglePendingJobId === job.id}
                runPending={runPendingJobId === job.id}
                deletePending={deletePendingJobId === job.id}
                onToggleEnabled={onToggleEnabled}
                onRunNow={onRunNow}
                onEdit={onEdit}
                onDelete={onDelete}
                onToggleExpanded={onToggleExpanded}
              >
                <CronJobDetail
                  job={job}
                  runs={runsByJobId[job.id] ?? []}
                  loading={loadingRunsForJobId === job.id}
                  error={isExpanded ? runHistoryError : null}
                />
              </CronJobCard>
            )
          })}
        </motion.div>
      )}
    </section>
  )
}
