import { motion } from 'motion/react'
import { cn } from '@/lib/utils'

export type AgentProgressStatus =
  | 'running'
  | 'thinking'
  | 'complete'
  | 'failed'
  | 'queued'

type AgentProgressProps = {
  value: number
  status: AgentProgressStatus
  size?: number
  strokeWidth?: number
  className?: string
}

function getProgressStrokeClassName(status: AgentProgressStatus): string {
  if (status === 'failed') return 'text-red-400'
  if (status === 'thinking') return 'text-accent-400'
  if (status === 'complete') return 'text-emerald-400'
  if (status === 'queued') return 'text-primary-500'
  return 'text-emerald-400'
}

export function AgentProgress({
  value,
  status,
  size = 96,
  strokeWidth = 6,
  className,
}: AgentProgressProps) {
  const clamped = Math.max(0, Math.min(100, value))
  const radius = (size - strokeWidth) / 2
  const circumference = 2 * Math.PI * radius

  return (
    <svg
      width={size}
      height={size}
      viewBox={`0 0 ${size} ${size}`}
      className={cn('pointer-events-none', className)}
      aria-hidden
    >
      <circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        strokeWidth={strokeWidth}
        className="fill-none stroke-primary-300/70"
      />
      <motion.circle
        cx={size / 2}
        cy={size / 2}
        r={radius}
        strokeWidth={strokeWidth}
        strokeLinecap="round"
        strokeDasharray={circumference}
        initial={false}
        animate={{
          strokeDashoffset: circumference - (clamped / 100) * circumference,
        }}
        transition={{ duration: 0.45, ease: 'easeOut' }}
        className={cn(
          'origin-center -rotate-90 fill-none stroke-current',
          getProgressStrokeClassName(status),
        )}
        style={{ transformOrigin: '50% 50%', transform: 'rotate(-90deg)' }}
      />
    </svg>
  )
}
