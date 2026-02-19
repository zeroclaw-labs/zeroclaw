'use client'

import { memo, useEffect, useRef, useState } from 'react'
import { cn } from '@/lib/utils'

// Unicode braille: U+2800 + dot bits
// Dot layout (2×4 grid):
//   1 4
//   2 5
//   3 6
//   7 8
// Bit mapping: d1=0x01, d2=0x02, d3=0x04, d4=0x08, d5=0x10, d6=0x20, d7=0x40, d8=0x80

function braille(...dots: number[]): string {
  let bits = 0
  for (const d of dots) bits |= d
  return String.fromCharCode(0x2800 + bits)
}

// Dot bit constants
const D1 = 0x01,
  D2 = 0x02,
  D3 = 0x04,
  D4 = 0x08
const D5 = 0x10,
  D6 = 0x20,
  D7 = 0x40,
  D8 = 0x80

const PRESETS: Record<string, string[]> = {
  // Classic rotating braille spinner
  braille: '⠿⠧⠇⠏⠟⠻⠹⠸⠼⠾'.split(''),

  // Single dot orbiting all 8 positions
  orbit: [D1, D2, D3, D7, D8, D6, D5, D4].map((d) => braille(d)),

  // Expanding and contracting
  breathe: [
    braille(D3, D6), // center dots only
    braille(D2, D3, D5, D6), // mid + bottom
    braille(D1, D2, D3, D4, D5, D6), // top 6
    braille(D1, D2, D3, D4, D5, D6, D7, D8), // all 8
    braille(D1, D2, D3, D4, D5, D6), // shrink
    braille(D2, D3, D5, D6), // mid + bottom
    braille(D3, D6), // center only
    braille(0), // blank
  ],

  // Pulsing density
  pulse: [
    braille(D3, D6),
    braille(D2, D5),
    braille(D1, D2, D4, D5),
    braille(D1, D2, D3, D4, D5, D6),
    braille(D1, D2, D3, D4, D5, D6, D7, D8),
    braille(D1, D2, D3, D4, D5, D6),
    braille(D1, D2, D4, D5),
    braille(D2, D5),
  ],

  // Claw grabbing motion 🦀
  // Open pincer → closing → gripped → releasing
  claw: [
    // Open wide - two "arms" spread
    braille(D1, D4), // tips open
    braille(D1, D2, D4, D5), // arms extending down
    braille(D1, D2, D3, D4, D5, D6), // full arms open
    braille(D1, D2, D3, D7, D4, D5, D6, D8), // arms + base (fully open claw)
    // Closing inward
    braille(D2, D3, D7, D5, D6, D8), // tips retract, mid closes
    braille(D3, D7, D6, D8), // closing more
    braille(D7, D8), // gripped! just the base
    braille(D3, D7, D6, D8), // grip pulse
    braille(D7, D8), // gripped tight
    // Releasing
    braille(D3, D7, D6, D8), // opening
    braille(D2, D3, D7, D5, D6, D8), // wider
    braille(D1, D2, D3, D7, D4, D5, D6, D8), // fully open again
  ],

  // Snake pattern
  snake: [
    braille(D1),
    braille(D1, D2),
    braille(D2, D3),
    braille(D3, D7),
    braille(D7, D8),
    braille(D8, D6),
    braille(D6, D5),
    braille(D5, D4),
    braille(D4, D1),
  ],

  // Wave rows
  wave: [
    braille(D1, D4),
    braille(D1, D2, D4, D5),
    braille(D2, D3, D5, D6),
    braille(D3, D7, D6, D8),
    braille(D7, D8),
    braille(D3, D7, D6, D8),
    braille(D2, D3, D5, D6),
    braille(D1, D2, D4, D5),
  ],
}

type BrailleSpinnerPreset = keyof typeof PRESETS

type BrailleSpinnerProps = {
  /** Animation preset name */
  preset?: BrailleSpinnerPreset | string
  /** Font size (CSS value or number in px) */
  size?: string | number
  /** Text color (CSS value) */
  color?: string
  /** Milliseconds per frame */
  speed?: number
  /** Additional CSS classes */
  className?: string
  /** Accessible label */
  label?: string
}

function BrailleSpinnerComponent({
  preset = 'claw',
  size,
  color,
  speed = 100,
  className,
  label = 'Loading',
}: BrailleSpinnerProps) {
  const [frame, setFrame] = useState(0)
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null)

  const frames = PRESETS[preset] ?? PRESETS.claw!

  useEffect(() => {
    intervalRef.current = setInterval(() => {
      setFrame((prev) => (prev + 1) % frames.length)
    }, speed)

    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current)
    }
  }, [frames.length, speed])

  const style: React.CSSProperties = {}
  if (size) style.fontSize = typeof size === 'number' ? `${size}px` : size
  if (color) style.color = color

  return (
    <span
      role="status"
      aria-label={label}
      className={cn(
        'inline-block font-mono leading-none select-none',
        className,
      )}
      style={style}
    >
      {frames[frame % frames.length]}
    </span>
  )
}

const BrailleSpinner = memo(BrailleSpinnerComponent)
export { BrailleSpinner, PRESETS as BRAILLE_PRESETS }
export type { BrailleSpinnerPreset, BrailleSpinnerProps }

// Usage:
// <BrailleSpinner />                          — default claw animation
// <BrailleSpinner preset="braille" />          — classic rotating
// <BrailleSpinner preset="orbit" size={24} />  — orbiting dot, 24px
// <BrailleSpinner preset="claw" color="var(--color-primary-500)" speed={120} />
