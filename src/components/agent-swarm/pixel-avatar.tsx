/**
 * PixelAvatar — SVG pixel art robot avatar with expressions, walking, and direction.
 * Each persona gets a unique color scheme. Animated based on status + behavior state.
 */
import { motion } from 'motion/react'
import { cn } from '@/lib/utils'
import type { Expression } from './agent-behaviors'

type PixelAvatarProps = {
  color: string
  accentColor: string
  size?: number
  status?: 'running' | 'thinking' | 'complete' | 'failed' | 'error' | 'idle'
  expression?: Expression
  isWalking?: boolean
  direction?: 'left' | 'right'
  className?: string
}

/** Color mappings per persona role */
export const PERSONA_COLORS: Record<string, { body: string; accent: string }> =
  {
    Roger: { body: '#3b82f6', accent: '#93c5fd' },
    Sally: { body: '#a855f7', accent: '#d8b4fe' },
    Bill: { body: '#f97316', accent: '#fdba74' },
    Ada: { body: '#10b981', accent: '#6ee7b7' },
    Max: { body: '#f59e0b', accent: '#fcd34d' },
    Luna: { body: '#06b6d4', accent: '#67e8f9' },
    Kai: { body: '#eab308', accent: '#fde047' },
    Nova: { body: '#ef4444', accent: '#fca5a5' },
  }

function Eyes({ expression = 'neutral' }: { expression?: Expression }) {
  switch (expression) {
    case 'happy':
      return (
        <>
          {/* ^ ^ happy eyes */}
          <path
            d="M5 4 L6 3 L7 4"
            stroke="white"
            strokeWidth="0.8"
            fill="none"
          />
          <path
            d="M9 4 L10 3 L11 4"
            stroke="white"
            strokeWidth="0.8"
            fill="none"
          />
          {/* Small smile */}
          <path
            d="M6.5 5.5 Q8 7 9.5 5.5"
            stroke="white"
            strokeWidth="0.6"
            fill="none"
          />
        </>
      )
    case 'focused':
      return (
        <>
          {/* — — narrow eyes */}
          <rect x="5" y="3.5" width="2" height="0.8" fill="white" />
          <rect x="9" y="3.5" width="2" height="0.8" fill="white" />
        </>
      )
    case 'confused':
      return (
        <>
          {/* One eye higher, ? above */}
          <rect x="5" y="3" width="2" height="2" fill="white" />
          <rect x="6" y="3" width="1" height="1" fill="#1e293b" />
          <rect x="9" y="4" width="2" height="2" fill="white" />
          <rect x="10" y="4" width="1" height="1" fill="#1e293b" />
          <text x="13" y="2" fontSize="3" fill="#fbbf24" fontWeight="bold">
            ?
          </text>
        </>
      )
    case 'tired':
      return (
        <>
          {/* Half-closed eyes */}
          <rect x="5" y="4" width="2" height="1" fill="white" opacity="0.7" />
          <rect x="9" y="4" width="2" height="1" fill="white" opacity="0.7" />
          {/* zZz */}
          <text x="12" y="2" fontSize="2.5" fill="#94a3b8" fontWeight="bold">
            z
          </text>
          <text x="13.5" y="0.5" fontSize="2" fill="#94a3b8" fontWeight="bold">
            z
          </text>
        </>
      )
    case 'excited':
      return (
        <>
          {/* ★ ★ star eyes */}
          <text x="4.5" y="5" fontSize="3" fill="#fde047">
            ★
          </text>
          <text x="8.5" y="5" fontSize="3" fill="#fde047">
            ★
          </text>
          {/* Open mouth O */}
          <circle cx="8" cy="6" r="0.8" fill="white" />
        </>
      )
    default:
      // neutral — standard dot eyes
      return (
        <>
          <rect x="5" y="3" width="2" height="2" fill="white" />
          <rect x="9" y="3" width="2" height="2" fill="white" />
          <rect x="6" y="3" width="1" height="1" fill="#1e293b" />
          <rect x="10" y="3" width="1" height="1" fill="#1e293b" />
        </>
      )
  }
}

export function PixelAvatar({
  color,
  accentColor,
  size = 32,
  status = 'idle',
  expression = 'neutral',
  isWalking = false,
  direction = 'right',
  className,
}: PixelAvatarProps) {
  const isActive = status === 'running' || status === 'thinking'
  const scaleX = direction === 'left' ? -1 : 1

  return (
    <motion.svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      className={cn('pixelated', className)}
      style={{ imageRendering: 'pixelated', transform: `scaleX(${scaleX})` }}
      animate={isActive && !isWalking ? { y: [0, -2, 0] } : {}}
      transition={
        isActive && !isWalking
          ? { duration: 0.8, repeat: Infinity, ease: 'easeInOut' }
          : {}
      }
    >
      {/* Head */}
      <rect x="4" y="1" width="8" height="6" rx="1" fill={color} />
      {/* Expression-based eyes */}
      <Eyes expression={expression} />
      {/* Antenna */}
      <rect x="7" y="0" width="2" height="1" fill={accentColor} />
      {/* Body */}
      <rect x="3" y="7" width="10" height="5" rx="1" fill={color} />
      {/* Chest detail */}
      <rect
        x="6"
        y="8"
        width="4"
        height="3"
        rx="0.5"
        fill={accentColor}
        opacity="0.6"
      />
      {/* Arms */}
      <motion.rect
        x="1"
        y="8"
        width="2"
        height="3"
        rx="0.5"
        fill={color}
        animate={isWalking ? { y: [8, 7, 8, 9, 8] } : { y: 8 }}
        transition={isWalking ? { duration: 0.4, repeat: Infinity } : {}}
      />
      <motion.rect
        x="13"
        y="8"
        width="2"
        height="3"
        rx="0.5"
        fill={color}
        animate={isWalking ? { y: [8, 9, 8, 7, 8] } : { y: 8 }}
        transition={isWalking ? { duration: 0.4, repeat: Infinity } : {}}
      />
      {/* Legs — alternate when walking */}
      <motion.rect
        x="5"
        width="2"
        height="3"
        rx="0.5"
        fill={color}
        animate={isWalking ? { y: [12, 11, 12, 13, 12] } : { y: 12 }}
        transition={isWalking ? { duration: 0.4, repeat: Infinity } : {}}
      />
      <motion.rect
        x="9"
        width="2"
        height="3"
        rx="0.5"
        fill={color}
        animate={isWalking ? { y: [12, 13, 12, 11, 12] } : { y: 12 }}
        transition={isWalking ? { duration: 0.4, repeat: Infinity } : {}}
      />
      {/* Feet */}
      <motion.rect
        x="4"
        width="3"
        height="2"
        rx="0.5"
        fill={accentColor}
        animate={isWalking ? { y: [14, 13, 14, 15, 14] } : { y: 14 }}
        transition={isWalking ? { duration: 0.4, repeat: Infinity } : {}}
      />
      <motion.rect
        x="9"
        width="3"
        height="2"
        rx="0.5"
        fill={accentColor}
        animate={isWalking ? { y: [14, 15, 14, 13, 14] } : { y: 14 }}
        transition={isWalking ? { duration: 0.4, repeat: Infinity } : {}}
      />

      {/* Status indicator glow */}
      {status === 'thinking' && (
        <motion.circle
          cx="14"
          cy="2"
          r="1.5"
          fill="#fbbf24"
          animate={{ opacity: [0.4, 1, 0.4] }}
          transition={{ duration: 1.2, repeat: Infinity }}
        />
      )}
      {status === 'complete' && (
        <circle cx="14" cy="2" r="1.5" fill="#34d399" />
      )}
      {status === 'failed' && <circle cx="14" cy="2" r="1.5" fill="#f87171" />}
    </motion.svg>
  )
}
