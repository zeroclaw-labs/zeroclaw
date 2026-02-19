/**
 * AgentCharacter — Individual agent avatar for the isometric office view.
 * Displays persona emoji, name, role, and animated status indicators.
 */
import { motion } from 'motion/react'
import { cn } from '@/lib/utils'
import type { AgentPersona } from '@/lib/agent-personas'

type AgentStatus =
  | 'running'
  | 'thinking'
  | 'complete'
  | 'failed'
  | 'error'
  | 'idle'

type AgentCharacterProps = {
  persona: AgentPersona
  status: AgentStatus
  task?: string
  style?: React.CSSProperties
}

const statusGlow: Record<AgentStatus, string> = {
  thinking: 'shadow-[0_0_20px_rgba(251,191,36,0.5)]',
  running: 'shadow-[0_0_20px_rgba(96,165,250,0.5)]',
  complete: 'shadow-[0_0_20px_rgba(52,211,153,0.4)]',
  failed: 'shadow-[0_0_20px_rgba(248,113,113,0.4)]',
  error: 'shadow-[0_0_20px_rgba(239,68,68,0.5)]',
  idle: 'shadow-[0_0_10px_rgba(148,163,184,0.2)]',
}

const statusDot: Record<AgentStatus, string> = {
  thinking: 'bg-amber-400',
  running: 'bg-blue-400',
  complete: 'bg-emerald-400',
  failed: 'bg-red-400',
  error: 'bg-red-500',
  idle: 'bg-slate-400',
}

export function AgentCharacter({
  persona,
  status,
  task,
  style,
}: AgentCharacterProps) {
  const isActive = status === 'running' || status === 'thinking'

  return (
    <motion.div
      className="flex flex-col items-center gap-1"
      style={style}
      animate={
        isActive
          ? {
              y: [0, -6, 0],
            }
          : {}
      }
      transition={
        isActive
          ? {
              duration: 2.5,
              repeat: Infinity,
              ease: 'easeInOut',
            }
          : {}
      }
    >
      {/* Thought bubble for thinking agents */}
      {status === 'thinking' && (
        <motion.div
          className="mb-1 rounded-lg bg-slate-800/90 px-2 py-1 text-[9px] text-slate-300"
          initial={{ opacity: 0, scale: 0.8 }}
          animate={{ opacity: [0.6, 1, 0.6], scale: [0.95, 1, 0.95] }}
          transition={{ duration: 2, repeat: Infinity }}
        >
          💭 thinking...
        </motion.div>
      )}

      {/* Name label */}
      <span className={cn('text-[11px] font-bold', persona.color)}>
        {persona.name}
      </span>

      {/* Avatar circle */}
      <motion.div
        className={cn(
          'relative flex size-16 items-center justify-center rounded-full border-2 border-slate-600 bg-slate-800/90',
          statusGlow[status],
        )}
        whileHover={{ scale: 1.1 }}
      >
        {/* Emoji */}
        <span className="text-2xl">{persona.emoji}</span>

        {/* Status dot */}
        <div
          className={cn(
            'absolute -bottom-0.5 -right-0.5 size-3.5 rounded-full border-2 border-slate-900',
            statusDot[status],
            isActive && 'animate-pulse',
          )}
        />

        {/* Complete checkmark */}
        {status === 'complete' && (
          <div className="absolute -right-1 -top-1 flex size-5 items-center justify-center rounded-full bg-emerald-500 text-[10px] text-white">
            ✓
          </div>
        )}
      </motion.div>

      {/* Role label */}
      <span className="text-[9px] text-slate-400">{persona.role}</span>

      {/* Task snippet */}
      {task && (
        <span className="max-w-[100px] truncate text-[8px] text-slate-500">
          {task}
        </span>
      )}
    </motion.div>
  )
}
