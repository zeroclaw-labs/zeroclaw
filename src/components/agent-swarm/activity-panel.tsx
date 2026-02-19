/**
 * ActivityPanel — Right sidebar showing live agent activity feed, roster, and stats.
 */
import { useMemo } from 'react'
import { motion, AnimatePresence } from 'motion/react'
import { assignPersona } from '@/lib/agent-personas'
import { PERSONA_COLORS } from './pixel-avatar'
import type { SwarmSession } from '@/stores/agent-swarm-store'
import { cn } from '@/lib/utils'
import { getSwarmSessionDisplayName } from './session-display-name'

type ActivityPanelProps = {
  sessions: Array<SwarmSession>
  className?: string
}

function formatAge(ms: number): string {
  const s = Math.floor(ms / 1000)
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m`
  return `${Math.floor(m / 60)}h`
}

function formatTokens(n?: number): string {
  if (!n) return '0'
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return String(n)
}

function formatCost(cost?: number): string {
  if (!cost) return '$0.00'
  return `$${cost.toFixed(4)}`
}

const statusIcon: Record<string, string> = {
  running: '🏃',
  thinking: '💭',
  complete: '✅',
  failed: '❌',
  idle: '💤',
}

const statusColor: Record<string, string> = {
  running: 'text-blue-400',
  thinking: 'text-amber-400',
  complete: 'text-emerald-400',
  failed: 'text-red-400',
  idle: 'text-slate-400',
}

const statusDotColor: Record<string, string> = {
  running: 'bg-blue-400',
  thinking: 'bg-amber-400',
  complete: 'bg-emerald-400',
  failed: 'bg-red-400',
  idle: 'bg-slate-400',
}

function AgentRosterItem({ session }: { session: SwarmSession }) {
  const key = session.key ?? session.friendlyId ?? ''
  const persona = assignPersona(
    key,
    session.task ?? session.initialMessage ?? session.label ?? '',
  )
  const displayName = getSwarmSessionDisplayName(session)
  const colors = PERSONA_COLORS[persona.name]
  const tokens =
    session.usage?.totalTokens ?? session.totalTokens ?? session.tokenCount ?? 0
  const isActive =
    session.swarmStatus === 'running' || session.swarmStatus === 'thinking'

  return (
    <motion.div
      layout
      initial={{ opacity: 0, x: 10 }}
      animate={{ opacity: 1, x: 0 }}
      exit={{ opacity: 0, x: -10 }}
      className="flex items-center gap-2.5 rounded-lg bg-slate-800/40 px-3 py-2 border border-slate-700/30"
    >
      {/* Color dot avatar */}
      <div
        className="flex size-8 shrink-0 items-center justify-center rounded-full text-sm"
        style={{
          backgroundColor: colors?.body ? `${colors.body}30` : '#6b728030',
        }}
      >
        {persona.emoji}
      </div>

      {/* Info */}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <span className="truncate text-xs font-semibold text-slate-200">
            {displayName}
          </span>
          <div
            className={cn(
              'size-1.5 rounded-full',
              statusDotColor[session.swarmStatus],
              isActive && 'animate-pulse',
            )}
          />
        </div>
        <p className="truncate text-[10px] text-slate-500">{persona.role}</p>
      </div>

      {/* Stats */}
      <div className="shrink-0 text-right">
        <div className="text-[10px] text-slate-400">{formatTokens(tokens)}</div>
        <div className="text-[10px] text-slate-500">
          {formatAge(session.staleness)}
        </div>
      </div>
    </motion.div>
  )
}

function ActivityFeedItem({
  session,
  index,
}: {
  session: SwarmSession
  index: number
}) {
  const displayName = getSwarmSessionDisplayName(session)
  const task =
    session.task ?? session.initialMessage ?? session.label ?? 'Working...'

  return (
    <motion.div
      initial={{ opacity: 0, y: -8 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ delay: index * 0.05 }}
      className="flex gap-2 py-1.5"
    >
      <span className="shrink-0 text-xs">
        {statusIcon[session.swarmStatus]}
      </span>
      <div className="min-w-0 flex-1">
        <span className="truncate text-[11px] font-semibold text-slate-200">
          {displayName}
        </span>
        <span className="text-[11px] text-slate-500"> — </span>
        <span className={cn('text-[11px]', statusColor[session.swarmStatus])}>
          {session.swarmStatus === 'running' ? 'working' : session.swarmStatus}
        </span>
        <p className="truncate text-[10px] text-slate-500 mt-0.5">{task}</p>
      </div>
      <span className="shrink-0 text-[9px] text-slate-600">
        {formatAge(session.staleness)}
      </span>
    </motion.div>
  )
}

export function ActivityPanel({ sessions, className }: ActivityPanelProps) {
  const totalTokens = useMemo(
    () =>
      sessions.reduce(
        (sum, s) => sum + (s.usage?.totalTokens ?? s.totalTokens ?? 0),
        0,
      ),
    [sessions],
  )
  const totalCost = useMemo(
    () => sessions.reduce((sum, s) => sum + (s.usage?.cost ?? s.cost ?? 0), 0),
    [sessions],
  )
  const active = sessions.filter(
    (s) => s.swarmStatus === 'running' || s.swarmStatus === 'thinking',
  )
  const completed = sessions.filter((s) => s.swarmStatus === 'complete')
  const failed = sessions.filter((s) => s.swarmStatus === 'failed')

  // Recent activity: sorted by staleness (most recent first)
  const recentActivity = useMemo(
    () => [...sessions].sort((a, b) => a.staleness - b.staleness).slice(0, 6),
    [sessions],
  )

  return (
    <div
      className={cn(
        'flex h-full flex-col gap-3 overflow-y-auto bg-[#0d1117] p-3 text-white',
        className,
      )}
    >
      {/* Stats Grid */}
      <div className="grid grid-cols-2 gap-2">
        <div className="rounded-lg border border-blue-500/20 bg-blue-500/10 px-3 py-2 text-center">
          <div className="text-lg font-bold text-blue-400">{active.length}</div>
          <div className="text-[10px] text-blue-300/70">Active</div>
        </div>
        <div className="rounded-lg border border-emerald-500/20 bg-emerald-500/10 px-3 py-2 text-center">
          <div className="text-lg font-bold text-emerald-400">
            {completed.length}
          </div>
          <div className="text-[10px] text-emerald-300/70">Done</div>
        </div>
        <div className="rounded-lg border border-accent-500/20 bg-accent-500/10 px-3 py-2 text-center">
          <div className="text-lg font-bold text-accent-400">
            {formatTokens(totalTokens)}
          </div>
          <div className="text-[10px] text-accent-300/70">Tokens</div>
        </div>
        <div className="rounded-lg border border-slate-500/20 bg-slate-500/10 px-3 py-2 text-center">
          <div className="text-lg font-bold text-slate-300">
            {formatCost(totalCost)}
          </div>
          <div className="text-[10px] text-slate-400">Cost</div>
        </div>
      </div>

      {/* Agent Roster */}
      <div>
        <h3 className="mb-2 flex items-center gap-1.5 text-xs font-semibold text-slate-300">
          <span>👥</span> Agent Roster
          <span className="ml-auto rounded-full bg-slate-700/50 px-1.5 text-[10px] text-slate-400">
            {sessions.length}
          </span>
        </h3>
        <div className="space-y-1.5">
          <AnimatePresence mode="popLayout">
            {sessions.slice(0, 8).map((session) => (
              <AgentRosterItem
                key={session.key ?? session.friendlyId}
                session={session}
              />
            ))}
          </AnimatePresence>
          {sessions.length === 0 && (
            <div className="rounded-lg border border-dashed border-slate-700 py-4 text-center text-[11px] text-slate-500">
              No agents spawned yet
            </div>
          )}
        </div>
      </div>

      {/* Activity Feed */}
      {recentActivity.length > 0 && (
        <div>
          <h3 className="mb-2 flex items-center gap-1.5 text-xs font-semibold text-slate-300">
            <span>📡</span> Live Activity
          </h3>
          <div className="space-y-0.5 divide-y divide-slate-800/50">
            {recentActivity.map((session, i) => (
              <ActivityFeedItem
                key={session.key ?? session.friendlyId}
                session={session}
                index={i}
              />
            ))}
          </div>
        </div>
      )}

      {/* Failed alerts */}
      {failed.length > 0 && (
        <div className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2">
          <div className="flex items-center gap-1.5 text-xs font-semibold text-red-400">
            ⚠️ {failed.length} Failed
          </div>
          {failed.map((s) => {
            const displayName = getSwarmSessionDisplayName(s)
            return (
              <div
                key={s.key ?? s.friendlyId}
                className="mt-1 text-[10px] text-red-300/70"
              >
                {displayName}: {s.task?.slice(0, 50) ?? 'Unknown task'}
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}
