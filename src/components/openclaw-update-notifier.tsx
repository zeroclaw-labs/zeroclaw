'use client'

import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { AnimatePresence, motion } from 'motion/react'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  Cancel01Icon,
  Loading03Icon,
  Tick01Icon,
  ArrowUp02Icon,
  Settings02Icon,
} from '@hugeicons/core-free-icons'
import { cn } from '@/lib/utils'

type UpdatePhase = 'idle' | 'updating' | 'restarting' | 'done' | 'error'

const DISMISS_KEY = 'openclaw-update-dismissed-version'
const AUTO_UPDATE_KEY = 'openclaw-auto-update'
const CHECK_INTERVAL_MS = 30 * 60 * 1000

function shouldShowUpdateBanner(
  data:
    | {
        updateAvailable: boolean
        latestVersion: string
      }
    | null
    | undefined,
  phase: UpdatePhase,
  dismissed: string | null,
): boolean {
  return Boolean(data?.updateAvailable) &&
    phase !== 'done' &&
    dismissed !== data?.latestVersion
}

export function OpenClawUpdateNotifier() {
  const queryClient = useQueryClient()
  const [dismissed, setDismissed] = useState<string | null>(null)
  const [phase, setPhase] = useState<UpdatePhase>('idle')
  const [errorMsg, setErrorMsg] = useState('')
  const [progress, setProgress] = useState(0)
  const [autoUpdate, setAutoUpdate] = useState(false)

  useEffect(() => {
    if (typeof window !== 'undefined') {
      setDismissed(localStorage.getItem(DISMISS_KEY))
      setAutoUpdate(localStorage.getItem(AUTO_UPDATE_KEY) === 'true')
    }
  }, [])

  const { data } = useQuery({
    queryKey: ['openclaw-update-check'],
    queryFn: async () => {
      const res = await fetch('/api/openclaw-update')
      if (!res.ok) return null
      return res.json() as Promise<{
        ok: boolean
        currentVersion: string
        latestVersion: string
        updateAvailable: boolean
        installType?: 'git' | 'npm' | 'unknown'
      }>
    },
    refetchInterval: CHECK_INTERVAL_MS,
    staleTime: CHECK_INTERVAL_MS,
    retry: false,
  })

  // Auto-update when enabled (only for git installs)
  useEffect(() => {
    if (autoUpdate && data?.updateAvailable && data.installType !== 'npm' && phase === 'idle') {
      void handleUpdate()
    }
  }, [autoUpdate, data?.updateAvailable, data?.installType])

  const visible = shouldShowUpdateBanner(data, phase, dismissed)

  function handleDismiss() {
    if (data?.latestVersion) {
      localStorage.setItem(DISMISS_KEY, data.latestVersion)
      setDismissed(data.latestVersion)
    }
  }

  function toggleAutoUpdate() {
    const next = !autoUpdate
    setAutoUpdate(next)
    localStorage.setItem(AUTO_UPDATE_KEY, String(next))
  }

  async function handleUpdate() {
    setPhase('updating')
    setProgress(0)
    setErrorMsg('')

    const progressTimer = setInterval(() => {
      setProgress((p) => Math.min(p + 2, 90))
    }, 300)

    try {
      setProgress(10)
      await new Promise((r) => setTimeout(r, 400))

      const res = await fetch('/api/openclaw-update', { method: 'POST' })
      const result = (await res.json()) as {
        ok: boolean
        error?: string
        message?: string
      }

      clearInterval(progressTimer)

      if (result.ok) {
        setPhase('restarting')
        setProgress(80)

        // Wait for gateway to come back
        let attempts = 0
        while (attempts < 30) {
          attempts++
          await new Promise((r) => setTimeout(r, 2000))
          try {
            const ping = await fetch('/api/ping', {
              signal: AbortSignal.timeout(3000),
            })
            if (ping.ok) break
          } catch {
            // Gateway still restarting
          }
        }

        setPhase('done')
        setProgress(100)
        void queryClient.invalidateQueries({
          queryKey: ['openclaw-update-check'],
        })
        setTimeout(() => window.location.reload(), 1500)
      } else {
        setPhase('error')
        setErrorMsg(result.error || 'Update failed')
        setProgress(0)
      }
    } catch {
      clearInterval(progressTimer)
      // Connection drop during update usually means it's working
      setPhase('restarting')
      setProgress(85)
      setTimeout(() => {
        setPhase('done')
        setProgress(100)
        setTimeout(() => window.location.reload(), 1500)
      }, 5000)
    }
  }

  const isUpdating = phase === 'updating' || phase === 'restarting'

  return (
    <AnimatePresence>
      {visible && data && (
        <motion.div
          initial={{ opacity: 0, y: -50, scale: 0.95 }}
          animate={{ opacity: 1, y: 0, scale: 1 }}
          exit={{ opacity: 0, y: -50, scale: 0.95 }}
          transition={{ duration: 0.35, ease: [0.23, 1, 0.32, 1] }}
          className={cn(
            'fixed top-4 left-1/2 -translate-x-1/2 z-[9998]',
            'flex flex-col rounded-2xl overflow-hidden',
            'bg-primary-950 text-white',
            'shadow-2xl shadow-black/40',
            'border border-primary-800/60',
            'max-w-md w-[90vw]',
            'transition-all duration-300',
          )}
        >
          {/* Progress bar */}
          {isUpdating && (
            <motion.div
              className="h-0.5 bg-blue-500 origin-left"
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
                    : 'bg-blue-500/20',
              )}
            >
              {isUpdating ? (
                <HugeiconsIcon
                  icon={Loading03Icon}
                  size={18}
                  strokeWidth={2}
                  className="animate-spin text-blue-400"
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
                  icon={ArrowUp02Icon}
                  size={18}
                  strokeWidth={2}
                  className="text-blue-400"
                />
              )}
            </div>

            <div className="flex-1 min-w-0">
              <p className="text-sm font-semibold">
                {phase === 'done'
                  ? 'OpenClaw Updated!'
                  : phase === 'restarting'
                    ? 'Restarting gateway...'
                    : phase === 'updating'
                      ? 'Updating OpenClaw...'
                      : phase === 'error'
                        ? 'Update Failed'
                        : 'OpenClaw Update'}
              </p>
              <p className="text-xs text-primary-400 truncate">
                {phase === 'error'
                  ? errorMsg
                  : phase === 'done'
                    ? `Reloading with v${data.latestVersion}...`
                    : isUpdating
                      ? 'Please wait...'
                      : data.installType === 'npm'
                        ? `v${data.currentVersion} → v${data.latestVersion} · Run: npm i -g openclaw@latest`
                        : `v${data.currentVersion} → v${data.latestVersion}`}
              </p>
            </div>

            <div className="flex items-center gap-2 shrink-0">
              {(phase === 'idle' || phase === 'error') && data.installType !== 'npm' && (
                <button
                  type="button"
                  onClick={handleUpdate}
                  className={cn(
                    'rounded-lg px-4 py-1.5 text-xs font-semibold transition-all',
                    'bg-blue-500 hover:bg-blue-400 text-white',
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

          {/* Auto-update toggle */}
          {(phase === 'idle' || phase === 'error') && (
            <div className="border-t border-primary-800/60 px-5 py-2.5 flex items-center justify-between">
              <div className="flex items-center gap-2">
                <HugeiconsIcon
                  icon={Settings02Icon}
                  size={14}
                  strokeWidth={2}
                  className="text-primary-500"
                />
                <span className="text-xs text-primary-400">
                  Auto-update OpenClaw
                </span>
              </div>
              <button
                type="button"
                onClick={toggleAutoUpdate}
                className={cn(
                  'relative inline-flex h-5 w-9 shrink-0 cursor-pointer rounded-full transition-colors duration-200',
                  autoUpdate ? 'bg-blue-500' : 'bg-primary-700',
                )}
                role="switch"
                aria-checked={autoUpdate}
              >
                <span
                  className={cn(
                    'pointer-events-none inline-block size-4 rounded-full bg-white shadow-sm transform transition-transform duration-200',
                    autoUpdate ? 'translate-x-[17px]' : 'translate-x-0.5',
                    'mt-0.5',
                  )}
                />
              </button>
            </div>
          )}
        </motion.div>
      )}
    </AnimatePresence>
  )
}
