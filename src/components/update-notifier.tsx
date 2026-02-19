import { useEffect, useMemo, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { AnimatePresence, motion } from 'motion/react'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  Cancel01Icon,
  Tick01Icon,
  Loading03Icon,
  SparklesIcon,
} from '@hugeicons/core-free-icons'
import { cn } from '@/lib/utils'

type CommitEntry = {
  hash: string
  subject: string
  date: string
}

type UpdateCheckResult = {
  updateAvailable: boolean
  localVersion: string
  remoteVersion: string
  localCommit: string
  remoteCommit: string
  localDate: string
  remoteDate: string
  behindBy: number
  changelog: Array<CommitEntry>
}

type UpdatePhase =
  | 'idle'
  | 'pulling'
  | 'installing'
  | 'restarting'
  | 'done'
  | 'error'

const DISMISS_KEY = 'openclaw-update-dismissed'
const CHECK_INTERVAL_MS = 15 * 60 * 1000

const PHASE_LABELS: Record<UpdatePhase, string> = {
  idle: '',
  pulling: 'Pulling latest changes...',
  installing: 'Installing dependencies...',
  restarting: 'Restarting...',
  done: 'Update complete!',
  error: 'Update failed',
}

function commitTypeIcon(subject: string): string {
  const lower = subject.toLowerCase()
  if (lower.startsWith('feat')) return '✨'
  if (lower.startsWith('fix')) return '🐛'
  if (lower.startsWith('perf')) return '⚡'
  if (lower.startsWith('refactor')) return '♻️'
  if (lower.startsWith('docs')) return '📝'
  if (lower.startsWith('style')) return '💄'
  if (lower.startsWith('chore')) return '🔧'
  if (lower.startsWith('security') || lower.startsWith('sec')) return '🔒'
  return '📦'
}

function cleanSubject(subject: string): string {
  // Strip conventional commit prefix like "feat: " or "fix(scope): "
  return subject.replace(/^[a-z]+(\([^)]*\))?:\s*/i, '')
}

function relativeTime(dateStr: string): string {
  if (!dateStr) return ''
  const diff = Date.now() - new Date(dateStr).getTime()
  const mins = Math.floor(diff / 60_000)
  if (mins < 1) return 'just now'
  if (mins < 60) return `${mins}m ago`
  const hrs = Math.floor(mins / 60)
  if (hrs < 24) return `${hrs}h ago`
  const days = Math.floor(hrs / 24)
  return `${days}d ago`
}

export function UpdateNotifier() {
  const queryClient = useQueryClient()
  const [dismissed, setDismissed] = useState<string | null>(null)
  const [visible, setVisible] = useState(false)
  const [expanded, setExpanded] = useState(false)
  const [phase, setPhase] = useState<UpdatePhase>('idle')
  const [progress, setProgress] = useState(0)
  const [errorMsg, setErrorMsg] = useState('')

  useEffect(() => {
    setDismissed(localStorage.getItem(DISMISS_KEY))
  }, [])

  const { data } = useQuery<UpdateCheckResult>({
    queryKey: ['update-check'],
    queryFn: async () => {
      const res = await fetch('/api/update-check')
      if (!res.ok) throw new Error('update check failed')
      return res.json() as Promise<UpdateCheckResult>
    },
    refetchInterval: CHECK_INTERVAL_MS,
    staleTime: CHECK_INTERVAL_MS,
    retry: false,
  })

  useEffect(() => {
    if (!data?.updateAvailable) {
      setVisible(false)
      return
    }
    if (dismissed === data.remoteCommit) {
      setVisible(false)
      return
    }
    setVisible(true)
  }, [data, dismissed])

  // Categorize changelog
  const changelogSummary = useMemo(() => {
    if (!data?.changelog) return { features: 0, fixes: 0, other: 0 }
    let features = 0
    let fixes = 0
    let other = 0
    for (const c of data.changelog) {
      const lower = c.subject.toLowerCase()
      if (lower.startsWith('feat')) features++
      else if (lower.startsWith('fix')) fixes++
      else other++
    }
    return { features, fixes, other }
  }, [data?.changelog])

  function handleDismiss() {
    if (data?.remoteCommit) {
      localStorage.setItem(DISMISS_KEY, data.remoteCommit)
      setDismissed(data.remoteCommit)
    }
    setVisible(false)
    setExpanded(false)
    setPhase('idle')
  }

  async function handleInstall() {
    setPhase('pulling')
    setProgress(0)
    setErrorMsg('')

    // Simulate phased progress
    const progressTimer = setInterval(() => {
      setProgress((p) => Math.min(p + 2, 90))
    }, 300)

    try {
      // Phase 1: pulling
      setProgress(10)
      await new Promise((r) => setTimeout(r, 500))
      setPhase('installing')
      setProgress(30)

      const res = await fetch('/api/update-check', { method: 'POST' })
      const result = (await res.json()) as { ok: boolean; output: string }

      clearInterval(progressTimer)

      if (result.ok) {
        setPhase('restarting')
        setProgress(90)
        await new Promise((r) => setTimeout(r, 800))
        setPhase('done')
        setProgress(100)
        void queryClient.invalidateQueries({ queryKey: ['update-check'] })
        // Auto-reload after showing success
        setTimeout(() => window.location.reload(), 1500)
      } else {
        setPhase('error')
        setErrorMsg(result.output?.slice(0, 300) || 'Unknown error')
        setProgress(0)
      }
    } catch (err) {
      clearInterval(progressTimer)
      setPhase('error')
      setErrorMsg(err instanceof Error ? err.message : 'Update failed')
      setProgress(0)
    }
  }

  const isUpdating =
    phase === 'pulling' || phase === 'installing' || phase === 'restarting'

  return (
    <AnimatePresence>
      {visible && data && (
        <motion.div
          initial={{ opacity: 0, y: -50, scale: 0.95 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={{ opacity: 0, y: -50, scale: 0.95 }}
          transition={{ duration: 0.35, ease: [0.23, 1, 0.32, 1] }}
          className={cn(
            'fixed top-4 left-1/2 -translate-x-1/2 z-[9999]',
            'flex flex-col rounded-2xl overflow-hidden',
            'bg-primary-950 text-white',
            'shadow-2xl shadow-black/40',
            'border border-primary-800/60',
            expanded ? 'max-w-lg w-[95vw]' : 'max-w-md w-[90vw]',
            'transition-all duration-300',
          )}
        >
          {/* Progress bar */}
          {isUpdating && (
            <motion.div
              className="h-0.5 bg-accent-500 origin-left"
              initial={{ scaleX: 0 }}
              animate={{ scaleX: progress / 100 }}
              transition={{ duration: 0.3 }}
            />
          )}
          {phase === 'done' && <div className="h-0.5 bg-green-500" />}

          {/* Header */}
          <div className="flex items-center gap-3 px-5 py-3.5">
            <div
              className={cn(
                'flex items-center justify-center size-9 rounded-xl shrink-0',
                phase === 'done'
                  ? 'bg-green-500/20'
                  : phase === 'error'
                    ? 'bg-red-500/20'
                    : 'bg-accent-500/20',
              )}
            >
              {isUpdating ? (
                <HugeiconsIcon
                  icon={Loading03Icon}
                  size={18}
                  strokeWidth={2}
                  className="animate-spin text-accent-400"
                />
              ) : phase === 'done' ? (
                <HugeiconsIcon
                  icon={Tick01Icon}
                  size={18}
                  strokeWidth={2}
                  className="text-green-400"
                />
              ) : (
                <HugeiconsIcon
                  icon={SparklesIcon}
                  size={18}
                  strokeWidth={2}
                  className="text-accent-400"
                />
              )}
            </div>

            <div className="flex-1 min-w-0">
              <p className="text-sm font-semibold">
                {phase === 'idle' || phase === 'error'
                  ? 'ClawSuite Update'
                  : PHASE_LABELS[phase]}
              </p>
              <p className="text-xs text-primary-400 truncate">
                {phase === 'error'
                  ? errorMsg
                  : phase === 'done'
                    ? 'Reloading with latest version...'
                    : isUpdating
                      ? PHASE_LABELS[phase]
                      : `${data.behindBy} update${data.behindBy !== 1 ? 's' : ''} available · ${data.localVersion}`}
              </p>
            </div>

            <div className="flex items-center gap-2 shrink-0">
              {!expanded && phase === 'idle' && (
                <button
                  type="button"
                  onClick={() => setExpanded(true)}
                  className="text-xs text-primary-400 hover:text-primary-200 transition-colors"
                >
                  What&apos;s new?
                </button>
              )}
              {(phase === 'idle' || phase === 'error') && (
                <button
                  type="button"
                  onClick={handleInstall}
                  className={cn(
                    'rounded-lg px-4 py-1.5 text-xs font-semibold transition-all',
                    'bg-accent-500 hover:bg-accent-400 text-white',
                  )}
                >
                  {phase === 'error' ? 'Retry' : 'Install'}
                </button>
              )}
              {!isUpdating && phase !== 'done' && (
                <button
                  type="button"
                  onClick={handleDismiss}
                  className="rounded-full p-1 text-primary-500 hover:text-primary-300 transition-colors"
                  aria-label="Dismiss"
                >
                  <HugeiconsIcon
                    icon={Cancel01Icon}
                    size={14}
                    strokeWidth={2}
                  />
                </button>
              )}
            </div>
          </div>

          {/* Changelog */}
          <AnimatePresence>
            {expanded && (
              <motion.div
                initial={{ height: 0, opacity: 0 }}
                animate={{ height: 'auto', opacity: 1 }}
                exit={{ height: 0, opacity: 0 }}
                transition={{ duration: 0.25 }}
                className="overflow-hidden"
              >
                <div className="border-t border-primary-800/60">
                  {data.changelog.length > 0 ? (
                    <>
                      {/* Summary pills */}
                      <div className="flex items-center gap-2 px-5 pt-3 pb-2">
                        {changelogSummary.features > 0 && (
                          <span className="inline-flex items-center gap-1 rounded-full bg-accent-500/15 px-2.5 py-0.5 text-[11px] font-medium text-accent-400">
                            ✨ {changelogSummary.features} feature
                            {changelogSummary.features !== 1 ? 's' : ''}
                          </span>
                        )}
                        {changelogSummary.fixes > 0 && (
                          <span className="inline-flex items-center gap-1 rounded-full bg-blue-500/15 px-2.5 py-0.5 text-[11px] font-medium text-blue-400">
                            🐛 {changelogSummary.fixes} fix
                            {changelogSummary.fixes !== 1 ? 'es' : ''}
                          </span>
                        )}
                        {changelogSummary.other > 0 && (
                          <span className="inline-flex items-center gap-1 rounded-full bg-primary-700/40 px-2.5 py-0.5 text-[11px] font-medium text-primary-400">
                            📦 {changelogSummary.other} other
                          </span>
                        )}
                      </div>

                      {/* Commit list */}
                      <div className="max-h-48 overflow-y-auto px-5 pb-4 space-y-1">
                        {data.changelog.map((commit) => (
                          <div
                            key={commit.hash}
                            className="flex items-start gap-2.5 py-1.5 group"
                          >
                            <span className="text-sm leading-5 shrink-0">
                              {commitTypeIcon(commit.subject)}
                            </span>
                            <div className="flex-1 min-w-0">
                              <p className="text-xs text-primary-200 leading-5 truncate">
                                {cleanSubject(commit.subject)}
                              </p>
                            </div>
                            <div className="flex items-center gap-2 shrink-0">
                              <code className="text-[10px] text-primary-500 font-mono">
                                {commit.hash}
                              </code>
                              <span className="text-[10px] text-primary-600">
                                {relativeTime(commit.date)}
                              </span>
                            </div>
                          </div>
                        ))}
                      </div>
                    </>
                  ) : (
                    <div className="px-5 py-4 text-center">
                      <p className="text-xs text-primary-400">
                        {data.behindBy} new update
                        {data.behindBy !== 1 ? 's' : ''} available with bug
                        fixes and improvements.
                      </p>
                      <p className="text-[10px] text-primary-500 mt-1">
                        Click Install to update to the latest version.
                      </p>
                    </div>
                  )}
                </div>
              </motion.div>
            )}
          </AnimatePresence>
        </motion.div>
      )}
    </AnimatePresence>
  )
}
