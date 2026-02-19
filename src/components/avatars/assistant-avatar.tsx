import { memo } from 'react'
import { cn } from '@/lib/utils'

type AvatarProps = {
  size?: number
  className?: string
}

/**
 * Assistant avatar — matches the ClawSuite favicon/hero logo.
 * Orange gradient rounded square with dark claw brackets < | >
 */
function AssistantAvatarComponent({ size = 28, className }: AvatarProps) {
  return (
    <svg
      viewBox="0 0 100 100"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={cn('shrink-0', className)}
      style={{ width: size, height: size }}
    >
      <defs>
        <linearGradient id="ava-orange" x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" stopColor="#ea580c" />
          <stop offset="50%" stopColor="#f97316" />
          <stop offset="100%" stopColor="#fb923c" />
        </linearGradient>
      </defs>
      {/* Orange background */}
      <rect
        x="5"
        y="5"
        width="90"
        height="90"
        rx="20"
        fill="url(#ava-orange)"
      />
      {/* Left claw bracket */}
      <path
        d="M 40 35 L 30 50 L 40 65"
        stroke="#1e293b"
        strokeWidth="5"
        strokeLinecap="round"
        strokeLinejoin="round"
        fill="none"
      />
      {/* Right claw bracket */}
      <path
        d="M 60 35 L 70 50 L 60 65"
        stroke="#1e293b"
        strokeWidth="5"
        strokeLinecap="round"
        strokeLinejoin="round"
        fill="none"
      />
      {/* Center cursor bar */}
      <rect x="47" y="40" width="6" height="20" rx="3" fill="#1e293b" />
    </svg>
  )
}

export const AssistantAvatar = memo(AssistantAvatarComponent)
