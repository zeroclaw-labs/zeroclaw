import { AnimatePresence, motion } from 'motion/react'
import type { AgentNodeStatus } from './agent-card'
import { cn } from '@/lib/utils'

export type SwarmConnectionLine = {
  id: string
  startY: number
  endY: number
  status: AgentNodeStatus
}

type SwarmConnectionOverlayProps = {
  lines: Array<SwarmConnectionLine>
  centerX: number
  className?: string
}

function getLineColor(status: AgentNodeStatus): string {
  if (status === 'thinking') return '#f97316' // accent-500
  if (status === 'complete') return '#10b981' // emerald-500
  if (status === 'failed') return '#ef4444' // red-500
  if (status === 'queued') return '#6366f1' // primary-500
  return '#10b981' // emerald-500
}

function getGlowFilter(status: AgentNodeStatus): string {
  if (status === 'thinking')
    return 'drop-shadow(0 0 4px rgba(249, 115, 22, 0.6))'
  if (status === 'complete')
    return 'drop-shadow(0 0 3px rgba(16, 185, 129, 0.5))'
  if (status === 'failed') return 'drop-shadow(0 0 3px rgba(239, 68, 68, 0.5))'
  return 'drop-shadow(0 0 4px rgba(16, 185, 129, 0.6))'
}

function shouldPulse(status: AgentNodeStatus): boolean {
  return status === 'running' || status === 'thinking'
}

export function SwarmConnectionOverlay({
  lines,
  centerX,
  className,
}: SwarmConnectionOverlayProps) {
  return (
    <svg
      aria-hidden
      className={cn('pointer-events-none absolute inset-0 z-10', className)}
      preserveAspectRatio="none"
    >
      <AnimatePresence initial={false}>
        {lines.map(function renderLine(line) {
          const color = getLineColor(line.status)
          const filter = getGlowFilter(line.status)
          const isPulsing = shouldPulse(line.status)

          return (
            <g key={line.id}>
              {/* Vertical connector line */}
              <motion.line
                x1={centerX}
                y1={line.startY}
                x2={centerX}
                y2={line.endY}
                stroke={color}
                strokeWidth={1.5}
                strokeLinecap="round"
                style={{ filter }}
                initial={{ pathLength: 0, opacity: 0 }}
                animate={{
                  pathLength: 1,
                  opacity: isPulsing ? [0.5, 0.9, 0.5] : 0.8,
                }}
                exit={{ opacity: 0 }}
                transition={{
                  pathLength: { duration: 0.25, ease: 'easeOut' },
                  opacity: isPulsing
                    ? { duration: 1.2, ease: 'easeInOut', repeat: Infinity }
                    : { duration: 0.3 },
                }}
              />

              {/* Top dot (at orchestrator) */}
              <motion.circle
                cx={centerX}
                cy={line.startY}
                r={3}
                fill={color}
                style={{ filter }}
                initial={{ scale: 0, opacity: 0 }}
                animate={{
                  scale: 1,
                  opacity: isPulsing ? [0.6, 1, 0.6] : 0.9,
                }}
                exit={{ scale: 0, opacity: 0 }}
                transition={{
                  scale: { duration: 0.2, ease: 'easeOut' },
                  opacity: isPulsing
                    ? { duration: 1.2, ease: 'easeInOut', repeat: Infinity }
                    : { duration: 0.3 },
                }}
              />

              {/* Bottom dot (at agent card) */}
              <motion.circle
                cx={centerX}
                cy={line.endY}
                r={3}
                fill={color}
                style={{ filter }}
                initial={{ scale: 0, opacity: 0 }}
                animate={{
                  scale: 1,
                  opacity: isPulsing ? [0.6, 1, 0.6] : 0.9,
                }}
                exit={{ scale: 0, opacity: 0 }}
                transition={{
                  scale: { duration: 0.2, ease: 'easeOut', delay: 0.1 },
                  opacity: isPulsing
                    ? { duration: 1.2, ease: 'easeInOut', repeat: Infinity }
                    : { duration: 0.3 },
                }}
              />
            </g>
          )
        })}
      </AnimatePresence>
    </svg>
  )
}
