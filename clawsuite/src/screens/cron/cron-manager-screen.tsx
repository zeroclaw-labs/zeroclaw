import { Clock01Icon, RefreshIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { motion } from 'motion/react'
import { useMemo, useState } from 'react'
import type {
  CronJob,
  CronJobUpsertInput,
  CronRun,
  CronSortKey,
  CronStatusFilter,
} from '@/components/cron-manager/cron-types'
import { CronJobForm } from '@/components/cron-manager/CronJobForm'
import { CronJobList } from '@/components/cron-manager/CronJobList'
import { Button } from '@/components/ui/button'
import {
  deleteCronJob,
  fetchCronJobs,
  fetchCronRuns,
  runCronJob,
  toggleCronJob,
  upsertCronJob,
} from '@/lib/cron-api'

const cronQueryKeys = {
  jobs: ['cron', 'jobs'] as const,
  runs: function runs(jobId: string) {
    return ['cron', 'runs', jobId] as const
  },
} as const

export function CronManagerScreen() {
  const queryClient = useQueryClient()
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null)
  const [searchText, setSearchText] = useState('')
  const [sortBy, setSortBy] = useState<CronSortKey>('lastRun')
  const [statusFilter, setStatusFilter] = useState<CronStatusFilter>('all')
  const [formMode, setFormMode] = useState<'create' | 'edit' | null>(null)
  const [editingJobId, setEditingJobId] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const [togglePendingJobId, setTogglePendingJobId] = useState<string | null>(
    null,
  )
  const [runPendingJobId, setRunPendingJobId] = useState<string | null>(null)
  const [deletePendingJobId, setDeletePendingJobId] = useState<string | null>(
    null,
  )

  const jobsQuery = useQuery({
    queryKey: cronQueryKeys.jobs,
    queryFn: fetchCronJobs,
    refetchInterval: 30_000,
  })

  const runsQuery = useQuery({
    queryKey: cronQueryKeys.runs(selectedJobId ?? 'none'),
    queryFn: async function queryCronRuns() {
      if (!selectedJobId) return []
      return fetchCronRuns(selectedJobId)
    },
    enabled: Boolean(selectedJobId),
  })

  const toggleMutation = useMutation({
    mutationFn: toggleCronJob,
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: cronQueryKeys.jobs })
    },
    onError: (error) => {
      setActionError(error instanceof Error ? error.message : String(error))
    },
    onSettled: () => {
      setTogglePendingJobId(null)
    },
  })

  const runMutation = useMutation({
    mutationFn: runCronJob,
    onSuccess: async (_, jobId) => {
      await queryClient.invalidateQueries({ queryKey: cronQueryKeys.jobs })
      await queryClient.invalidateQueries({
        queryKey: cronQueryKeys.runs(jobId),
      })
    },
    onError: (error) => {
      setActionError(error instanceof Error ? error.message : String(error))
    },
    onSettled: () => {
      setRunPendingJobId(null)
    },
  })

  const upsertMutation = useMutation({
    mutationFn: upsertCronJob,
  })

  const deleteMutation = useMutation({
    mutationFn: deleteCronJob,
    onSettled: () => {
      setDeletePendingJobId(null)
    },
  })

  const jobs = useMemo(
    function deriveJobs() {
      return Array.isArray(jobsQuery.data) ? jobsQuery.data : []
    },
    [jobsQuery.data],
  )
  const jobsErrorMessage =
    jobsQuery.error instanceof Error ? jobsQuery.error.message : null
  const runsErrorMessage =
    runsQuery.error instanceof Error ? runsQuery.error.message : null
  const editingJob = useMemo(
    function deriveEditingJob() {
      if (formMode !== 'edit' || !editingJobId) return null
      return jobs.find((job) => job.id === editingJobId) ?? null
    },
    [editingJobId, formMode, jobs],
  )

  const runsByJobId = useMemo<Record<string, Array<CronRun>>>(
    function deriveRunsByJobId() {
      if (!selectedJobId) return {}
      return {
        [selectedJobId]: Array.isArray(runsQuery.data) ? runsQuery.data : [],
      }
    },
    [runsQuery.data, selectedJobId],
  )

  function handleToggleEnabled(job: CronJob, enabled: boolean) {
    setActionError(null)
    setTogglePendingJobId(job.id)
    void toggleMutation.mutate({
      jobId: job.id,
      enabled,
    })
  }

  function handleRunNow(job: CronJob) {
    setActionError(null)
    setRunPendingJobId(job.id)
    void runMutation.mutate(job.id)
  }

  function handleToggleExpanded(jobId: string) {
    setSelectedJobId(function setNextExpanded(prev) {
      return prev === jobId ? null : jobId
    })
  }

  function closeForm() {
    setFormMode(null)
    setEditingJobId(null)
    setFormError(null)
  }

  function handleStartCreate() {
    if (formMode === 'create') {
      closeForm()
      return
    }
    setFormMode('create')
    setEditingJobId(null)
    setFormError(null)
  }

  function handleStartEdit(job: CronJob) {
    setFormMode('edit')
    setEditingJobId(job.id)
    setFormError(null)
    setActionError(null)
  }

  async function handleSubmitForm(payload: CronJobUpsertInput) {
    setActionError(null)
    setFormError(null)

    try {
      await upsertMutation.mutateAsync(payload)
      await queryClient.invalidateQueries({ queryKey: cronQueryKeys.jobs })
      if (payload.jobId) {
        await queryClient.invalidateQueries({
          queryKey: cronQueryKeys.runs(payload.jobId),
        })
      }
      closeForm()
    } catch (error) {
      setFormError(error instanceof Error ? error.message : String(error))
    }
  }

  async function handleDeleteJob(job: CronJob) {
    setActionError(null)
    const shouldDelete = window.confirm(
      `Delete cron job "${job.name}"? This cannot be undone.`,
    )
    if (!shouldDelete) return

    setDeletePendingJobId(job.id)
    try {
      await deleteMutation.mutateAsync(job.id)
      await queryClient.invalidateQueries({ queryKey: cronQueryKeys.jobs })
      await queryClient.invalidateQueries({
        queryKey: cronQueryKeys.runs(job.id),
      })

      if (selectedJobId === job.id) {
        setSelectedJobId(null)
      }
      if (editingJobId === job.id) {
        closeForm()
      }
    } catch (error) {
      setActionError(error instanceof Error ? error.message : String(error))
    }
  }

  return (
    <motion.main
      className="h-full overflow-y-auto bg-surface px-4 py-6 text-primary-900 md:px-6 md:py-8"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: 0.2, ease: 'easeOut' }}
    >
      <section className="mx-auto w-full max-w-[1600px]">
        <header className="mb-4 rounded-2xl border border-primary-200 bg-primary-50/85 p-4 backdrop-blur-xl md:p-5">
          <div className="inline-flex items-center gap-2 rounded-full border border-primary-200 bg-primary-100/60 px-3 py-1 text-xs text-primary-600 tabular-nums">
            <HugeiconsIcon icon={Clock01Icon} size={20} strokeWidth={1.5} />
            <span>Cron Manager</span>
          </div>
          <h1 className="mt-3 text-2xl font-medium text-ink text-balance md:text-3xl">
            Scheduled Task Control
          </h1>
          <p className="mt-1 max-w-3xl text-sm text-primary-600 text-pretty md:text-base">
            Monitor cron jobs, toggle schedules, trigger manual runs, and
            inspect execution history from one screen.
          </p>
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={function onClickRefresh() {
                void jobsQuery.refetch()
                if (selectedJobId) {
                  void runsQuery.refetch()
                }
              }}
              className="tabular-nums"
            >
              <HugeiconsIcon icon={RefreshIcon} size={20} strokeWidth={1.5} />
              Refresh
            </Button>
            <Button
              variant={formMode ? 'secondary' : 'outline'}
              size="sm"
              onClick={function onClickCreate() {
                handleStartCreate()
              }}
              className="tabular-nums"
            >
              {formMode === 'create' ? 'Hide Create Form' : 'Create Job'}
            </Button>
          </div>
        </header>

        {actionError ? (
          <section className="mb-4 rounded-2xl border border-accent-500/40 bg-accent-500/10 p-4 text-sm text-accent-500 text-pretty">
            {actionError}
          </section>
        ) : null}

        {formMode ? (
          <div className="mb-4">
            {formMode === 'edit' && !editingJob ? (
              <section className="rounded-2xl border border-accent-500/40 bg-accent-500/10 p-4 text-sm text-accent-500 text-pretty">
                The selected cron job is no longer available.
              </section>
            ) : (
              <CronJobForm
                key={`${formMode}-${editingJob?.id ?? 'create'}`}
                mode={formMode}
                initialJob={formMode === 'edit' ? editingJob : null}
                pending={upsertMutation.isPending}
                error={formError}
                onSubmit={handleSubmitForm}
                onClose={closeForm}
              />
            )}
          </div>
        ) : null}

        {jobsQuery.isLoading ? (
          <section className="rounded-2xl border border-primary-200 bg-primary-50/80 p-8 text-center text-sm text-primary-600 text-pretty">
            Loading cron jobs...
          </section>
        ) : jobsQuery.isError ? (
          <section className="rounded-2xl border border-accent-500/40 bg-accent-500/10 p-4 text-sm text-accent-500 text-pretty">
            {jobsErrorMessage ?? 'Failed to load cron jobs.'}
          </section>
        ) : (
          <CronJobList
            jobs={jobs}
            runsByJobId={runsByJobId}
            loadingRunsForJobId={runsQuery.isFetching ? selectedJobId : null}
            runHistoryError={runsQuery.isError ? runsErrorMessage : null}
            selectedJobId={selectedJobId}
            searchText={searchText}
            sortBy={sortBy}
            statusFilter={statusFilter}
            onSearchTextChange={setSearchText}
            onSortByChange={setSortBy}
            onStatusFilterChange={setStatusFilter}
            onToggleEnabled={handleToggleEnabled}
            onRunNow={handleRunNow}
            onEdit={handleStartEdit}
            onDelete={handleDeleteJob}
            onToggleExpanded={handleToggleExpanded}
            togglePendingJobId={togglePendingJobId}
            runPendingJobId={runPendingJobId}
            deletePendingJobId={deletePendingJobId}
          />
        )}
      </section>
    </motion.main>
  )
}
