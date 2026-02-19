import { useEffect, useState } from 'react'
import { KillConfirmDialog } from './kill-confirm-dialog'
import { SteerModal } from './steer-modal'

export type AgentRegistryStatus = 'active' | 'idle' | 'available' | 'paused'

export type AgentRegistryCardData = {
  id: string
  name: string
  role: string
  category: string
  color: 'orange' | 'blue' | 'cyan' | 'purple' | 'violet'
  status: AgentRegistryStatus
  sessionKey?: string
  friendlyId?: string
  controlKey: string
}

type AgentRegistryCardProps = {
  agent: AgentRegistryCardData
  isSpawning?: boolean
  onChat: (agent: AgentRegistryCardData) => void | Promise<void>
  onSpawn: (agent: AgentRegistryCardData) => void | Promise<void>
  onHistory: (agent: AgentRegistryCardData) => void
  onPauseToggle: (
    agent: AgentRegistryCardData,
    nextPaused: boolean,
  ) => Promise<void>
  onKilled?: (agent: AgentRegistryCardData) => void
}

const STATUS_LABELS: Record<AgentRegistryStatus, string> = {
  active: 'Active',
  idle: 'Idle',
  available: 'Available',
  paused: 'Paused',
}

const STATUS_DOT_CLASS: Record<AgentRegistryStatus, string> = {
  active: 'bg-emerald-500',
  idle: 'bg-yellow-500',
  available: 'bg-neutral-400',
  paused: 'bg-red-500',
}

const CARD_GRADIENT_CLASS: Record<AgentRegistryCardData['color'], string> = {
  orange:
    'bg-gradient-to-br from-orange-500/20 via-orange-400/10 to-white/40 dark:to-neutral-900/30',
  blue: 'bg-gradient-to-br from-blue-500/20 via-blue-400/10 to-white/40 dark:to-neutral-900/30',
  cyan: 'bg-gradient-to-br from-cyan-500/20 via-cyan-400/10 to-white/40 dark:to-neutral-900/30',
  purple:
    'bg-gradient-to-br from-purple-500/20 via-purple-400/10 to-white/40 dark:to-neutral-900/30',
  violet:
    'bg-gradient-to-br from-violet-500/20 via-violet-400/10 to-white/40 dark:to-neutral-900/30',
}

export function AgentRegistryCard({
  agent,
  isSpawning = false,
  onChat,
  onSpawn,
  onHistory,
  onPauseToggle,
  onKilled,
}: AgentRegistryCardProps) {
  const [menuOpen, setMenuOpen] = useState(false)
  const [steerOpen, setSteerOpen] = useState(false)
  const [killOpen, setKillOpen] = useState(false)
  const [notice, setNotice] = useState('')
  const [pausePending, setPausePending] = useState(false)

  useEffect(() => {
    if (!notice) return
    const timer = window.setTimeout(() => {
      setNotice('')
    }, 2200)
    return () => {
      window.clearTimeout(timer)
    }
  }, [notice])

  useEffect(() => {
    setMenuOpen(false)
    setSteerOpen(false)
    setKillOpen(false)
    setPausePending(false)
    setNotice('')
  }, [agent.id, agent.sessionKey, agent.status])

  const hasSession = Boolean(agent.sessionKey)
  const isPaused = agent.status === 'paused'

  function showSpawnFirstNotice() {
    setNotice('Spawn agent first')
  }

  function handleSteerIntent() {
    if (!hasSession) {
      showSpawnFirstNotice()
      return
    }
    setSteerOpen(true)
  }

  function handleKillIntent() {
    if (!hasSession) {
      showSpawnFirstNotice()
      return
    }
    setKillOpen(true)
  }

  async function handlePauseToggle() {
    if (pausePending) return
    const nextPaused = !isPaused
    setPausePending(true)
    try {
      await onPauseToggle(agent, nextPaused)
      setMenuOpen(false)
    } finally {
      setPausePending(false)
    }
  }

  return (
    <article
      className={`relative overflow-hidden rounded-2xl p-4 shadow-sm border border-white/20 ${CARD_GRADIENT_CLASS[agent.color]}`}
    >
      <div className="bg-white/40 dark:bg-neutral-900/20 backdrop-blur-md rounded-xl p-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <span
                className={`h-2.5 w-2.5 rounded-full ${STATUS_DOT_CLASS[agent.status]}`}
              />
              <span className="text-xs font-medium text-neutral-600 dark:text-neutral-300">
                {STATUS_LABELS[agent.status]}
              </span>
            </div>
            <h3 className="mt-1 truncate text-sm font-semibold text-neutral-900 dark:text-neutral-50">
              {agent.name}
            </h3>
            <p className="truncate text-xs text-neutral-600 dark:text-neutral-300">
              {agent.role || agent.category}
            </p>
          </div>

          <div className="relative">
            <button
              type="button"
              onClick={() => setMenuOpen((open) => !open)}
              className="inline-flex h-8 w-8 items-center justify-center rounded-lg bg-white/60 text-neutral-700 shadow-sm border border-white/30 dark:bg-neutral-900/30 dark:text-neutral-100 dark:border-white/10"
              aria-label={`${agent.name} controls`}
              aria-expanded={menuOpen}
            >
              ...
            </button>

            {menuOpen ? (
              <>
                <button
                  type="button"
                  aria-label="Close controls"
                  className="fixed inset-0 z-40"
                  onClick={() => setMenuOpen(false)}
                />
                <div className="absolute right-3 top-10 z-50 w-44 rounded-xl bg-white/90 dark:bg-neutral-900/90 backdrop-blur border border-white/30 dark:border-white/10 shadow-lg p-1">
                  <button
                    type="button"
                    onClick={() => {
                      setMenuOpen(false)
                      handleSteerIntent()
                    }}
                    className="flex w-full items-center rounded-lg px-2.5 py-2 text-left text-xs font-medium text-neutral-700 hover:bg-neutral-100 dark:text-neutral-200 dark:hover:bg-neutral-800"
                  >
                    Steer
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      void handlePauseToggle()
                    }}
                    disabled={pausePending}
                    className="flex w-full items-center rounded-lg px-2.5 py-2 text-left text-xs font-medium text-neutral-700 hover:bg-neutral-100 disabled:opacity-60 dark:text-neutral-200 dark:hover:bg-neutral-800"
                  >
                    {pausePending
                      ? isPaused
                        ? 'Resuming...'
                        : 'Pausing...'
                      : isPaused
                        ? 'Resume'
                        : 'Pause'}
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setMenuOpen(false)
                      handleKillIntent()
                    }}
                    className="flex w-full items-center rounded-lg px-2.5 py-2 text-left text-xs font-medium text-red-600 hover:bg-red-50 dark:text-red-300 dark:hover:bg-red-950/40"
                  >
                    Kill
                  </button>
                </div>
              </>
            ) : null}
          </div>
        </div>

        {notice ? (
          <p className="mt-2 text-[11px] font-medium text-neutral-700 dark:text-neutral-200">
            {notice}
          </p>
        ) : null}

        <div className="grid grid-cols-4 gap-2 mt-3">
          <button
            type="button"
            onClick={() => {
              void onChat(agent)
            }}
            className="rounded-xl bg-white/60 dark:bg-neutral-900/30 backdrop-blur px-2 py-2 text-[11px] font-medium text-neutral-800 dark:text-neutral-100 shadow-sm border border-white/30 dark:border-white/10 active:scale-[0.97] transition"
          >
            Chat
          </button>
          <button
            type="button"
            onClick={handleSteerIntent}
            className="rounded-xl bg-white/60 dark:bg-neutral-900/30 backdrop-blur px-2 py-2 text-[11px] font-medium text-neutral-800 dark:text-neutral-100 shadow-sm border border-white/30 dark:border-white/10 active:scale-[0.97] transition"
          >
            Steer
          </button>
          <button
            type="button"
            onClick={() => onHistory(agent)}
            className="rounded-xl bg-white/60 dark:bg-neutral-900/30 backdrop-blur px-2 py-2 text-[11px] font-medium text-neutral-800 dark:text-neutral-100 shadow-sm border border-white/30 dark:border-white/10 active:scale-[0.97] transition"
          >
            History
          </button>
          <button
            type="button"
            onClick={() => {
              void onSpawn(agent)
            }}
            disabled={isSpawning}
            className="rounded-xl bg-white/60 dark:bg-neutral-900/30 backdrop-blur px-2 py-2 text-[11px] font-medium text-neutral-800 dark:text-neutral-100 shadow-sm border border-white/30 dark:border-white/10 active:scale-[0.97] transition disabled:opacity-60"
          >
            {isSpawning ? '...' : 'Spawn'}
          </button>
        </div>
      </div>

      <SteerModal
        open={steerOpen}
        onOpenChange={setSteerOpen}
        agentName={agent.name}
        sessionKey={agent.sessionKey}
      />

      <KillConfirmDialog
        open={killOpen}
        onOpenChange={setKillOpen}
        agentName={agent.name}
        sessionKey={agent.sessionKey}
        onKilled={() => onKilled?.(agent)}
      />
    </article>
  )
}
