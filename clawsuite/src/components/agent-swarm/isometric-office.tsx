/**
 * IsometricOffice — Living virtual office for AI agent swarm.
 * Agents walk around, take breaks, chat, celebrate, and show expressions.
 * Inspired by Gather.town / @RoundtableSpace.
 */
import { motion, AnimatePresence } from 'motion/react'
import { useMemo } from 'react'
import { PERSONA_COLORS, PixelAvatar } from './pixel-avatar'
import { DESK_POSITIONS, LOCATIONS } from './agent-behaviors'
import {
  useAgentBehaviors,
  type AgentBehaviorView,
} from '@/hooks/use-agent-behaviors'
import { assignPersona } from '@/lib/agent-personas'
import type { SwarmSession } from '@/stores/agent-swarm-store'
import { cn } from '@/lib/utils'
import { getSwarmSessionDisplayName } from './session-display-name'

type IsometricOfficeProps = {
  sessions: Array<SwarmSession>
  className?: string
}

/* ── Floor ── */
function CheckeredFloor() {
  const tiles = []
  const cols = 20
  const rows = 14
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      const isDark = (r + c) % 2 === 0
      tiles.push(
        <rect
          key={`${r}-${c}`}
          x={c * 50}
          y={r * 50}
          width="50"
          height="50"
          fill={isDark ? '#1a1a2e' : '#16213e'}
        />,
      )
    }
  }
  return <>{tiles}</>
}

/* ── Static Furniture ── */
function Desk({ x, y }: { x: number; y: number }) {
  return (
    <div
      className="absolute flex flex-col items-center"
      style={{
        left: `${x}%`,
        top: `${y}%`,
        transform: 'translate(-50%, -50%)',
      }}
    >
      {/* Monitor */}
      <div className="h-8 w-10 rounded-t-sm border border-blue-500/40 bg-slate-800/60">
        <div className="m-0.5 h-5 rounded-sm bg-blue-600/30">
          {/* Screen content flicker */}
          <motion.div
            className="h-full w-full rounded-sm bg-blue-400/10"
            animate={{ opacity: [0.1, 0.3, 0.1] }}
            transition={{ duration: 3, repeat: Infinity, ease: 'easeInOut' }}
          />
        </div>
      </div>
      {/* Stand */}
      <div className="h-1.5 w-1 bg-slate-500" />
      {/* Desk surface */}
      <div className="h-4 w-14 rounded-sm bg-slate-600/40 shadow-md" />
    </div>
  )
}

function MeetingTable() {
  return (
    <div
      className="absolute"
      style={{
        left: `${LOCATIONS.meetingTable.x}%`,
        top: `${LOCATIONS.meetingTable.y}%`,
        transform: 'translate(-50%, -50%)',
      }}
    >
      <div className="relative">
        <div className="h-16 w-28 rounded-full bg-slate-600/50 shadow-lg" />
        {/* Chairs */}
        {[0, 60, 120, 180, 240, 300].map((angle) => {
          const rad = (angle * Math.PI) / 180
          return (
            <div
              key={angle}
              className="absolute size-3 rounded-full bg-slate-500/40"
              style={{
                left: `calc(50% + ${Math.cos(rad) * 42}px - 6px)`,
                top: `calc(50% + ${Math.sin(rad) * 24}px - 6px)`,
              }}
            />
          )
        })}
        <div className="absolute inset-0 flex items-center justify-center">
          <span className="text-[8px] text-slate-500/60">Meeting</span>
        </div>
      </div>
    </div>
  )
}

function WaterCooler() {
  return (
    <div
      className="absolute flex flex-col items-center"
      style={{
        left: `${LOCATIONS.waterCooler.x}%`,
        top: `${LOCATIONS.waterCooler.y}%`,
      }}
    >
      <div className="h-5 w-3 rounded-t-sm bg-sky-300/60" />
      <div className="h-8 w-4 rounded-b-sm bg-slate-400/40" />
      <span className="mt-0.5 text-[7px] text-slate-500">💧</span>
    </div>
  )
}

function CoffeeMachine() {
  return (
    <div
      className="absolute flex flex-col items-center"
      style={{
        left: `${LOCATIONS.coffeeMachine.x}%`,
        top: `${LOCATIONS.coffeeMachine.y}%`,
      }}
    >
      <div className="h-6 w-5 rounded-t-sm bg-amber-800/60 shadow-sm" />
      <div className="h-2 w-6 rounded-b-sm bg-amber-700/40" />
      <span className="mt-0.5 text-[7px] text-slate-500">☕</span>
    </div>
  )
}

function LunchArea() {
  return (
    <div
      className="absolute flex flex-col items-center"
      style={{
        left: `${LOCATIONS.lunchArea.x}%`,
        top: `${LOCATIONS.lunchArea.y}%`,
      }}
    >
      <div className="h-3 w-10 rounded-sm bg-slate-500/30" />
      <div className="flex gap-1 mt-0.5">
        <div className="size-2 rounded-full bg-red-400/30" />
        <div className="size-2 rounded-full bg-green-400/30" />
        <div className="size-2 rounded-full bg-yellow-400/30" />
      </div>
      <span className="mt-0.5 text-[7px] text-slate-500">🍕 Lunch</span>
    </div>
  )
}

function Plant({ x, y }: { x: number; y: number }) {
  return (
    <div className="absolute" style={{ left: `${x}%`, top: `${y}%` }}>
      <motion.div
        className="flex flex-col items-center"
        animate={{ rotate: [-1, 1, -1] }}
        transition={{ duration: 4, repeat: Infinity, ease: 'easeInOut' }}
      >
        <div className="size-6 rounded-full bg-emerald-600/60" />
        <div className="h-2 w-1.5 bg-amber-700/50" />
        <div className="h-3 w-5 rounded-sm bg-amber-600/40" />
      </motion.div>
    </div>
  )
}

function Clock() {
  const now = new Date()
  const hours = now.getHours().toString().padStart(2, '0')
  const minutes = now.getMinutes().toString().padStart(2, '0')
  return (
    <div className="absolute right-[8%] top-[10%] rounded bg-slate-800/70 px-2 py-1 border border-slate-600/30">
      <span className="font-mono text-[10px] text-green-400/80">
        {hours}:{minutes}
      </span>
    </div>
  )
}

/* ── Chat Connection Line ── */
function ChatLine({
  from,
  to,
}: {
  from: { x: number; y: number }
  to: { x: number; y: number }
}) {
  return (
    <svg
      className="pointer-events-none absolute inset-0 h-full w-full"
      style={{ zIndex: 5 }}
    >
      <line
        x1={`${from.x}%`}
        y1={`${from.y}%`}
        x2={`${to.x}%`}
        y2={`${to.y}%`}
        stroke="rgba(251, 191, 36, 0.2)"
        strokeWidth="1"
        strokeDasharray="4 4"
      />
    </svg>
  )
}

/* ── Animated Agent ── */
function AnimatedAgent({
  behavior,
  session,
}: {
  behavior: AgentBehaviorView
  session: SwarmSession
}) {
  const persona = assignPersona(
    behavior.sessionKey,
    session.task ?? session.initialMessage ?? session.label ?? '',
  )
  const displayName = getSwarmSessionDisplayName(session)
  const colors = PERSONA_COLORS[persona.name] ?? {
    body: '#6b7280',
    accent: '#9ca3af',
  }

  return (
    <motion.div
      className="absolute flex flex-col items-center"
      animate={{
        left: `${behavior.position.x}%`,
        top: `${behavior.position.y}%`,
      }}
      transition={{ duration: 0.9, ease: 'easeInOut' }}
      style={{
        transform: 'translate(-50%, -50%)',
        zIndex: Math.round(behavior.position.y) + 10,
      }}
    >
      {/* Activity emoji */}
      <motion.div
        className="mb-0.5 text-sm"
        animate={{ y: [0, -3, 0] }}
        transition={{ duration: 1.5, repeat: Infinity }}
      >
        {behavior.activityEmoji}
      </motion.div>

      {/* Chat bubble */}
      <AnimatePresence>
        {behavior.chatMessage && (
          <motion.div
            initial={{ opacity: 0, y: 4, scale: 0.8 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -4, scale: 0.8 }}
            className="mb-1 max-w-[100px] whitespace-nowrap rounded-lg bg-slate-800/90 px-2 py-1 text-[9px] text-slate-200 shadow-lg"
          >
            <div className="absolute -bottom-1 left-1/2 size-2 -translate-x-1/2 rotate-45 bg-slate-800/90" />
            {behavior.chatMessage}
          </motion.div>
        )}
      </AnimatePresence>

      {/* Pixel avatar */}
      <PixelAvatar
        color={colors.body}
        accentColor={colors.accent}
        size={44}
        status={session.swarmStatus}
        expression={behavior.expression}
        isWalking={behavior.isWalking}
        direction={behavior.direction}
      />

      {/* Name */}
      <span
        className={cn(
          'mt-0.5 max-w-[96px] truncate text-[10px] font-bold drop-shadow-md',
          persona.color,
        )}
      >
        {displayName}
      </span>

      {/* Role + status */}
      <div className="flex items-center gap-1">
        <div
          className={cn(
            'size-1.5 rounded-full',
            session.swarmStatus === 'running' && 'bg-blue-400 animate-pulse',
            session.swarmStatus === 'thinking' && 'bg-amber-400 animate-pulse',
            session.swarmStatus === 'complete' && 'bg-emerald-400',
            session.swarmStatus === 'failed' && 'bg-red-400',
            session.swarmStatus === 'idle' && 'bg-slate-400',
          )}
        />
        <span className="text-[8px] text-slate-500 drop-shadow-sm">
          {persona.role}
        </span>
      </div>
    </motion.div>
  )
}

/* ── Empty State ── */
function EmptyOffice() {
  return (
    <div className="flex h-full items-center justify-center">
      <div className="text-center">
        <motion.span
          className="text-4xl"
          animate={{ y: [0, -5, 0] }}
          transition={{ duration: 2, repeat: Infinity }}
        >
          🏢
        </motion.span>
        <p className="mt-2 text-sm text-slate-400">Virtual office is empty</p>
        <p className="text-xs text-slate-500">
          Spawn agents to see them work here
        </p>
      </div>
    </div>
  )
}

/* ── Ambient Particles ── */
function AmbientParticles() {
  const particles = useMemo(
    () =>
      Array.from({ length: 12 }).map((_, i) => ({
        id: i,
        x: Math.random() * 100,
        y: Math.random() * 100,
        delay: Math.random() * 5,
        duration: 3 + Math.random() * 4,
      })),
    [],
  )
  return (
    <>
      {particles.map((p) => (
        <motion.div
          key={p.id}
          className="absolute size-0.5 rounded-full bg-blue-400/20"
          style={{ left: `${p.x}%`, top: `${p.y}%` }}
          animate={{ opacity: [0, 0.6, 0], y: [0, -20, -40] }}
          transition={{
            duration: p.duration,
            delay: p.delay,
            repeat: Infinity,
          }}
        />
      ))}
    </>
  )
}

/* ── Main Component ── */
export function IsometricOffice({ sessions, className }: IsometricOfficeProps) {
  // Filter: show running/thinking always, completed/failed for 30s, skip idle old ones
  const activeSessions = useMemo(() => {
    return sessions
      .filter((s) => {
        if (s.swarmStatus === 'running' || s.swarmStatus === 'thinking')
          return true
        if (s.swarmStatus === 'complete' || s.swarmStatus === 'failed') {
          return s.staleness < 30_000 // Show for 30s after completion
        }
        return s.staleness < 10_000 // Idle: show briefly
      })
      .slice(0, 8) // Max 8 agents in office
  }, [sessions])

  const behaviors = useAgentBehaviors(activeSessions)

  // Find chatting pairs for connection lines
  const chatLines = useMemo(() => {
    const lines: Array<{
      from: { x: number; y: number }
      to: { x: number; y: number }
    }> = []
    for (const [, b] of behaviors) {
      if (b.activity === 'chatting' && b.chatTarget) {
        const target = behaviors.get(b.chatTarget)
        if (target) {
          lines.push({ from: b.position, to: target.position })
        }
      }
    }
    return lines
  }, [behaviors])

  return (
    <div
      className={cn(
        'relative h-full w-full overflow-hidden bg-[#0d1117]',
        className,
      )}
    >
      {/* Floor */}
      <svg
        className="absolute inset-0 h-full w-full opacity-80"
        preserveAspectRatio="none"
      >
        <CheckeredFloor />
      </svg>

      {/* Radial spotlight for atmosphere */}
      <div
        className="absolute inset-0 pointer-events-none"
        style={{
          background:
            'radial-gradient(ellipse at 50% 40%, rgba(59,130,246,0.05) 0%, transparent 70%)',
        }}
      />

      {/* Ambient particles */}
      <AmbientParticles />

      {/* Top wall */}
      <div className="absolute inset-x-0 top-0 h-[8%] border-b border-slate-600/30 bg-slate-700/40">
        <div className="flex h-full items-center justify-center gap-8 px-4">
          {Array.from({ length: 8 }).map((_, i) => (
            <div key={i} className="h-4 w-12 rounded-sm bg-slate-600/30" />
          ))}
        </div>
      </div>

      {/* Clock */}
      <Clock />

      {/* Desks */}
      {DESK_POSITIONS.map((pos, i) => (
        <Desk key={`desk-${i}`} x={pos.deskX} y={pos.deskY} />
      ))}

      {/* Meeting table */}
      <MeetingTable />

      {/* Decorations */}
      <WaterCooler />
      <CoffeeMachine />
      <LunchArea />
      <Plant x={3} y={20} />
      <Plant x={93} y={20} />
      <Plant x={93} y={75} />
      <Plant x={3} y={75} />

      {/* Chat connection lines */}
      {chatLines.map((line, i) => (
        <ChatLine key={`line-${i}`} from={line.from} to={line.to} />
      ))}

      {/* Animated agents */}
      <AnimatePresence mode="popLayout">
        {activeSessions.map((session) => {
          const key = session.key ?? session.friendlyId ?? ''
          const behavior = behaviors.get(key)
          if (!behavior) return null
          return (
            <AnimatedAgent key={key} behavior={behavior} session={session} />
          )
        })}
      </AnimatePresence>

      {/* Empty state */}
      {activeSessions.length === 0 && <EmptyOffice />}

      {/* Office info */}
      <div className="absolute bottom-3 left-3 rounded bg-slate-900/80 px-2 py-1 backdrop-blur">
        <span className="text-[9px] font-mono text-accent-400/60">
          🦞 ClawSuite Office
        </span>
      </div>

      <div className="absolute bottom-3 right-3 rounded bg-slate-900/80 px-2 py-1 backdrop-blur">
        <span className="text-[9px] text-slate-500">
          {behaviors.size} agents · {sessions.length} sessions
        </span>
      </div>

      {/* Whiteboard with live stats */}
      <div className="absolute left-[8%] top-[10%] rounded border border-slate-600/30 bg-slate-800/70 px-2 py-1">
        <div className="text-[8px] text-slate-400 font-mono">
          <div>📋 Tasks: {sessions.length}</div>
          <div>
            🏃 Active:{' '}
            {
              sessions.filter(
                (s) =>
                  s.swarmStatus === 'running' || s.swarmStatus === 'thinking',
              ).length
            }
          </div>
          <div>
            ✅ Done:{' '}
            {sessions.filter((s) => s.swarmStatus === 'complete').length}
          </div>
        </div>
      </div>
    </div>
  )
}
