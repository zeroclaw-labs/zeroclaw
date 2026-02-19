import { memo, useCallback, useMemo, useState } from 'react'
import type { OrchestratorState } from '@/hooks/use-orchestrator-state'
import { useOrchestratorState } from '@/hooks/use-orchestrator-state'
import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

/* ── Avatar types ─────────────────────────────────────── */

export type AvatarStyle =
  | 'lobster'
  | 'claw-cat'
  | 'robot'
  | 'ghost'
  | 'fox'
  | 'owl'
  | 'octopus'
  | 'dragon'
  | 'panda'

const AVATAR_OPTIONS: Array<{ id: AvatarStyle; label: string; emoji: string }> =
  [
    { id: 'lobster', label: 'Lobster', emoji: '🦞' },
    { id: 'claw-cat', label: 'Cat', emoji: '🐱' },
    { id: 'robot', label: 'Robot', emoji: '🤖' },
    { id: 'fox', label: 'Fox', emoji: '🦊' },
    { id: 'owl', label: 'Owl', emoji: '🦉' },
    { id: 'ghost', label: 'Ghost', emoji: '👻' },
    { id: 'octopus', label: 'Octopus', emoji: '🐙' },
    { id: 'dragon', label: 'Dragon', emoji: '🐉' },
    { id: 'panda', label: 'Panda', emoji: '🐼' },
  ]

const STORAGE_KEY = 'clawsuite-orchestrator-avatar'

function getStoredAvatar(): AvatarStyle {
  try {
    const v = localStorage.getItem(STORAGE_KEY)
    if (v && AVATAR_OPTIONS.some((o) => o.id === v)) return v as AvatarStyle
  } catch {
    /* noop */
  }
  return 'lobster'
}

/* ── CSS keyframes ────────────────────────────────────── */

const STYLE_ID = 'oa-styles-v2'

function ensureStyles() {
  if (typeof document === 'undefined') return
  if (document.getElementById(STYLE_ID)) return
  const style = document.createElement('style')
  style.id = STYLE_ID
  style.textContent = `
    @keyframes oa-breathe { 0%,100% { transform:scale(1); } 50% { transform:scale(1.05); } }
    @keyframes oa-think-ring { 0% { stroke-dashoffset:0; } 100% { stroke-dashoffset:-60; } }
    @keyframes oa-dot1 { 0%,80%,100% { opacity:.15; } 40% { opacity:1; } }
    @keyframes oa-dot2 { 0%,80%,100% { opacity:.15; } 50% { opacity:1; } }
    @keyframes oa-dot3 { 0%,80%,100% { opacity:.15; } 60% { opacity:1; } }
    @keyframes oa-bob { 0%,100% { transform:translateY(0); } 50% { transform:translateY(-1.5px); } }
    @keyframes oa-ear-twitch { 0%,90%,100% { transform:rotate(0deg); } 93% { transform:rotate(-4deg); } 96% { transform:rotate(4deg); } }
    @keyframes oa-tail-wag { 0%,100% { transform:rotate(0deg); } 25% { transform:rotate(8deg); } 75% { transform:rotate(-8deg); } }
    @keyframes oa-type { 0%,100% { transform:translateY(0); } 50% { transform:translateY(-1px); } }
  `
  document.head.appendChild(style)
}

/* ── SVG Avatars ──────────────────────────────────────── */

const O = '#f97316'
const D = '#1a1a2e'
const L = '#fed7aa'
const DO = '#ea580c'

function stateAnim(state: OrchestratorState): string {
  if (state === 'idle') return 'oa-breathe 3s ease-in-out infinite'
  if (state === 'responding') return 'oa-bob 0.8s ease-in-out infinite'
  return 'none'
}

function LobsterSVG({
  state,
  size,
}: {
  state: OrchestratorState
  size: number
}) {
  ensureStyles()
  const ey = state === 'thinking' ? 8 : 9.5
  const clawAnim =
    state !== 'idle' ? 'oa-type 0.6s ease-in-out infinite' : 'none'
  const mouth =
    state === 'orchestrating'
      ? 'M14,14 Q16,16.5 18,14'
      : state === 'responding'
        ? 'M14.5,14 Q16,15.5 17.5,14'
        : state === 'thinking'
          ? 'M15,14.5 Q16,14.5 17,14.5'
          : 'M14.5,14 Q16,15 17.5,14'

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      {state === 'orchestrating' && (
        <circle
          cx="16"
          cy="16"
          r="15"
          fill="none"
          stroke="#dc2626"
          strokeWidth="1.5"
          opacity="0.4"
        >
          <animate
            attributeName="opacity"
            values="0.3;0.6;0.3"
            dur="1.5s"
            repeatCount="indefinite"
          />
        </circle>
      )}

      {/* Antennae */}
      <line
        x1="12"
        y1="6"
        x2="8"
        y2="1"
        stroke="#dc2626"
        strokeWidth="1"
        strokeLinecap="round"
      />
      <line
        x1="20"
        y1="6"
        x2="24"
        y2="1"
        stroke="#dc2626"
        strokeWidth="1"
        strokeLinecap="round"
      />
      <circle cx="8" cy="1" r="1.2" fill="#ef4444" />
      <circle cx="24" cy="1" r="1.2" fill="#ef4444" />

      {/* Eye stalks */}
      <line x1="12" y1="9" x2="10" y2="6" stroke="#dc2626" strokeWidth="1.5" />
      <line x1="20" y1="9" x2="22" y2="6" stroke="#dc2626" strokeWidth="1.5" />
      <circle cx="10" cy={ey - 3} r="2" fill="white" />
      <circle cx="22" cy={ey - 3} r="2" fill="white" />
      <circle cx="10" cy={ey - 3} r="1" fill={D} />
      <circle cx="22" cy={ey - 3} r="1" fill={D} />
      <circle cx="10.3" cy={ey - 3.4} r="0.35" fill="white" opacity="0.9" />
      <circle cx="22.3" cy={ey - 3.4} r="0.35" fill="white" opacity="0.9" />

      {/* Head/body */}
      <ellipse cx="16" cy="13" rx="7" ry="6" fill="#dc2626" />
      <ellipse cx="16" cy="13" rx="5" ry="4" fill="#ef4444" opacity="0.4" />

      {/* Tail segments */}
      <ellipse cx="16" cy="20" rx="5.5" ry="3" fill="#dc2626" />
      <ellipse cx="16" cy="24" rx="4.5" ry="2.5" fill="#b91c1c" />
      <ellipse cx="16" cy="27.5" rx="3.5" ry="2" fill="#991b1b" />
      {/* Tail fan */}
      <ellipse
        cx="13"
        cy="30"
        rx="2.5"
        ry="1.2"
        fill="#dc2626"
        transform="rotate(-15 13 30)"
      />
      <ellipse cx="16" cy="30.5" rx="2" ry="1" fill="#dc2626" />
      <ellipse
        cx="19"
        cy="30"
        rx="2.5"
        ry="1.2"
        fill="#dc2626"
        transform="rotate(15 19 30)"
      />

      {/* Claws — left */}
      <g style={{ transformOrigin: '5px 14px', animation: clawAnim }}>
        <path
          d="M9,13 Q6,11 4,13"
          fill="none"
          stroke="#dc2626"
          strokeWidth="2"
          strokeLinecap="round"
        />
        <ellipse
          cx="3.5"
          cy="11.5"
          rx="2.5"
          ry="2"
          fill="#dc2626"
          transform="rotate(-20 3.5 11.5)"
        />
        <path
          d="M2.5,10.5 Q3.5,9 4.5,10.5"
          fill="none"
          stroke="#b91c1c"
          strokeWidth="0.8"
        />
      </g>

      {/* Claws — right */}
      <g
        style={{
          transformOrigin: '27px 14px',
          animation: clawAnim.replace('0.6s', '0.65s'),
        }}
      >
        <path
          d="M23,13 Q26,11 28,13"
          fill="none"
          stroke="#dc2626"
          strokeWidth="2"
          strokeLinecap="round"
        />
        <ellipse
          cx="28.5"
          cy="11.5"
          rx="2.5"
          ry="2"
          fill="#dc2626"
          transform="rotate(20 28.5 11.5)"
        />
        <path
          d="M27.5,10.5 Q28.5,9 29.5,10.5"
          fill="none"
          stroke="#b91c1c"
          strokeWidth="0.8"
        />
      </g>

      {/* Mouth */}
      <path
        d={mouth}
        fill="none"
        stroke={D}
        strokeWidth="0.8"
        strokeLinecap="round"
      />

      {/* Segment lines on tail */}
      <line
        x1="12"
        y1="19"
        x2="20"
        y2="19"
        stroke="#b91c1c"
        strokeWidth="0.5"
        opacity="0.5"
      />
      <line
        x1="12.5"
        y1="22"
        x2="19.5"
        y2="22"
        stroke="#991b1b"
        strokeWidth="0.5"
        opacity="0.5"
      />
      <line
        x1="13.5"
        y1="25"
        x2="18.5"
        y2="25"
        stroke="#7f1d1d"
        strokeWidth="0.5"
        opacity="0.5"
      />

      {/* Responding dots */}
      {state === 'responding' && (
        <g>
          <circle
            cx="10"
            cy="30.5"
            r="1"
            fill="#ef4444"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="31"
            r="1"
            fill="#ef4444"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="22"
            cy="30.5"
            r="1"
            fill="#ef4444"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function ClawCatSVG({
  state,
  size,
}: {
  state: OrchestratorState
  size: number
}) {
  ensureStyles()
  const ey = state === 'thinking' ? 12.5 : state === 'reading' ? 15 : 13.5
  const eRy =
    state === 'responding' ? 0.8 : state === 'orchestrating' ? 1.8 : 1.3
  const mouth =
    state === 'orchestrating'
      ? 'M13,18 Q16,21 19,18'
      : state === 'responding'
        ? 'M14,17.5 Q16,19 18,17.5'
        : state === 'thinking'
          ? 'M14,18 Q16,18 18,18'
          : 'M14,17.5 Q16,18.5 18,17.5'

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      {state === 'orchestrating' && (
        <circle
          cx="16"
          cy="16"
          r="15"
          fill="none"
          stroke={O}
          strokeWidth="1.5"
          opacity="0.4"
        >
          <animate
            attributeName="opacity"
            values="0.3;0.6;0.3"
            dur="1.5s"
            repeatCount="indefinite"
          />
        </circle>
      )}
      {/* Tail */}
      <path
        d="M24,26 Q28,22 30,24"
        fill="none"
        stroke={O}
        strokeWidth="2"
        strokeLinecap="round"
        style={{
          transformOrigin: '24px 26px',
          animation:
            state !== 'idle' ? 'oa-tail-wag 0.6s ease-in-out infinite' : 'none',
        }}
      />
      <ellipse cx="16" cy="25" rx="7" ry="5" fill={O} />
      <circle cx="16" cy="14" r="9" fill={O} />
      <circle cx="16" cy="15" r="6.5" fill={L} opacity="0.15" />
      {/* Ears */}
      <g
        style={{
          transformOrigin: '9px 6px',
          animation:
            state === 'thinking' || state === 'reading'
              ? 'oa-ear-twitch 2.5s ease-in-out infinite'
              : 'none',
        }}
      >
        <polygon points="8,9 4,2 12,6" fill={O} stroke={DO} strokeWidth="0.5" />
        <polygon points="9,8 6,3 11,6.5" fill={L} opacity="0.25" />
      </g>
      <g
        style={{
          transformOrigin: '23px 6px',
          animation:
            state === 'thinking' || state === 'reading'
              ? 'oa-ear-twitch 2.5s ease-in-out infinite 0.15s'
              : 'none',
        }}
      >
        <polygon
          points="24,9 20,6 28,2"
          fill={O}
          stroke={DO}
          strokeWidth="0.5"
        />
        <polygon points="23,8 21,6.5 26,3" fill={L} opacity="0.25" />
      </g>
      {/* Eyes */}
      <ellipse cx="12.5" cy={ey} rx="1.4" ry={eRy} fill={D} />
      <ellipse cx="19.5" cy={ey} rx="1.4" ry={eRy} fill={D} />
      <circle cx="13" cy={ey - 0.4} r="0.45" fill="white" opacity="0.9" />
      <circle cx="20" cy={ey - 0.4} r="0.45" fill="white" opacity="0.9" />
      {/* Nose + whiskers */}
      <polygon points="16,16 15.2,17 16.8,17" fill={DO} />
      <line
        x1="8"
        y1="15.5"
        x2="12"
        y2="16"
        stroke={D}
        strokeWidth="0.3"
        opacity="0.35"
      />
      <line
        x1="8"
        y1="17"
        x2="12"
        y2="16.5"
        stroke={D}
        strokeWidth="0.3"
        opacity="0.35"
      />
      <line
        x1="24"
        y1="15.5"
        x2="20"
        y2="16"
        stroke={D}
        strokeWidth="0.3"
        opacity="0.35"
      />
      <line
        x1="24"
        y1="17"
        x2="20"
        y2="16.5"
        stroke={D}
        strokeWidth="0.3"
        opacity="0.35"
      />
      <path
        d={mouth}
        fill="none"
        stroke={D}
        strokeWidth="0.8"
        strokeLinecap="round"
      />
      {/* Claw marks */}
      <g opacity="0.5">
        <line
          x1="14"
          y1="22"
          x2="13"
          y2="25"
          stroke={DO}
          strokeWidth="0.8"
          strokeLinecap="round"
        />
        <line
          x1="16"
          y1="22"
          x2="16"
          y2="25.5"
          stroke={DO}
          strokeWidth="0.8"
          strokeLinecap="round"
        />
        <line
          x1="18"
          y1="22"
          x2="19"
          y2="25"
          stroke={DO}
          strokeWidth="0.8"
          strokeLinecap="round"
        />
      </g>
      {/* Responding dots */}
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill={O}
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill={O}
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill={O}
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function RobotSVG({ state, size }: { state: OrchestratorState; size: number }) {
  ensureStyles()
  const eyeH = state === 'responding' ? 1 : state === 'thinking' ? 2.5 : 2
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      <line x1="16" y1="2" x2="16" y2="6" stroke="#94a3b8" strokeWidth="1.5" />
      <circle cx="16" cy="2" r="1.5" fill={O} />
      <rect x="6" y="6" width="20" height="16" rx="4" fill="#334155" />
      <rect x="8" y="8" width="16" height="12" rx="2" fill="#1e293b" />
      <rect
        x="10"
        y="11"
        width="3.5"
        height={eyeH}
        rx="0.5"
        fill={state === 'orchestrating' ? O : '#22d3ee'}
      />
      <rect
        x="18.5"
        y="11"
        width="3.5"
        height={eyeH}
        rx="0.5"
        fill={state === 'orchestrating' ? O : '#22d3ee'}
      />
      {state !== 'thinking' && (
        <rect x="13" y="16" width="6" height="1" rx="0.5" fill="#94a3b8" />
      )}
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16.5"
          r="1.2"
          fill="none"
          stroke="#94a3b8"
          strokeWidth="0.8"
        />
      )}
      <rect x="8" y="23" width="16" height="7" rx="3" fill="#334155" />
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill="#22d3ee"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill="#22d3ee"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill="#22d3ee"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function FoxSVG({ state, size }: { state: OrchestratorState; size: number }) {
  ensureStyles()
  const ey = state === 'thinking' ? 12.5 : 14
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      <ellipse cx="16" cy="25" rx="6" ry="4.5" fill="#ea580c" />
      <path
        d="M24,26 Q27,22 29,25"
        fill="none"
        stroke="#ea580c"
        strokeWidth="2.5"
        strokeLinecap="round"
      />
      <circle cx="16" cy="14" r="9" fill="#ea580c" />
      <polygon points="7,10 4,1 12,7" fill="#ea580c" />
      <polygon points="25,10 28,1 20,7" fill="#ea580c" />
      <polygon points="8,8 5.5,2.5 11,7" fill="#fed7aa" opacity="0.4" />
      <polygon points="24,8 26.5,2.5 21,7" fill="#fed7aa" opacity="0.4" />
      <ellipse cx="16" cy="16" rx="5" ry="4" fill="#fed7aa" opacity="0.25" />
      <ellipse
        cx="12.5"
        cy={ey}
        rx="1.3"
        ry={state === 'responding' ? 0.7 : 1.3}
        fill={D}
      />
      <ellipse
        cx="19.5"
        cy={ey}
        rx="1.3"
        ry={state === 'responding' ? 0.7 : 1.3}
        fill={D}
      />
      <circle cx="13" cy={ey - 0.4} r="0.4" fill="white" opacity="0.9" />
      <circle cx="20" cy={ey - 0.4} r="0.4" fill="white" opacity="0.9" />
      <polygon points="16,16.5 15.3,17.3 16.7,17.3" fill="#1a1a2e" />
      <path
        d={
          state === 'orchestrating'
            ? 'M13,18 Q16,20.5 19,18'
            : 'M14,17.5 Q16,18.5 18,17.5'
        }
        fill="none"
        stroke={D}
        strokeWidth="0.8"
        strokeLinecap="round"
      />
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill="#ea580c"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill="#ea580c"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill="#ea580c"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function OwlSVG({ state, size }: { state: OrchestratorState; size: number }) {
  ensureStyles()
  const er = state === 'thinking' ? 3.5 : state === 'responding' ? 2 : 3
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      <ellipse cx="16" cy="24" rx="7" ry="6" fill="#78350f" />
      <circle cx="16" cy="13" r="10" fill="#92400e" />
      <polygon points="7,8 4,2 10,6" fill="#92400e" />
      <polygon points="25,8 28,2 22,6" fill="#92400e" />
      <circle cx="12" cy="12" r={er} fill="white" opacity="0.9" />
      <circle cx="20" cy="12" r={er} fill="white" opacity="0.9" />
      <circle cx="12" cy="12" r={er * 0.45} fill={D} />
      <circle cx="20" cy="12" r={er * 0.45} fill={D} />
      <polygon points="16,15 14.5,17 17.5,17" fill="#f59e0b" />
      <ellipse cx="16" cy="22" rx="4" ry="2.5" fill="#fed7aa" opacity="0.2" />
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill="#f59e0b"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill="#f59e0b"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill="#f59e0b"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function GhostSVG({ state, size }: { state: OrchestratorState; size: number }) {
  ensureStyles()
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      <path
        d="M8,28 L8,14 A8,8 0 0 1 24,14 L24,28 L21,25 L18,28 L16,25 L14,28 L11,25 Z"
        fill="#e2e8f0"
        opacity="0.9"
      />
      <circle
        cx="12"
        cy="14"
        r={state === 'orchestrating' ? 2.5 : 2}
        fill={D}
      />
      <circle
        cx="20"
        cy="14"
        r={state === 'orchestrating' ? 2.5 : 2}
        fill={D}
      />
      <circle cx="12.5" cy="13.5" r="0.6" fill="white" opacity="0.9" />
      <circle cx="20.5" cy="13.5" r="0.6" fill="white" opacity="0.9" />
      {state === 'thinking' && (
        <ellipse cx="16" cy="19" rx="2" ry="2.5" fill={D} opacity="0.6" />
      )}
      {state !== 'thinking' && (
        <path
          d={
            state === 'orchestrating'
              ? 'M13,18 Q16,21 19,18'
              : 'M14,18 Q16,19 18,18'
          }
          fill="none"
          stroke={D}
          strokeWidth="0.8"
          strokeLinecap="round"
        />
      )}
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill="#94a3b8"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill="#94a3b8"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill="#94a3b8"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function OctopusSVG({
  state,
  size,
}: {
  state: OrchestratorState
  size: number
}) {
  ensureStyles()
  const tentacleAnim =
    state !== 'idle' ? 'oa-type 0.5s ease-in-out infinite' : 'none'
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      <circle cx="16" cy="12" r="9" fill="#7c3aed" />
      {/* Tentacles */}
      <path
        d="M8,20 Q6,26 4,28"
        fill="none"
        stroke="#7c3aed"
        strokeWidth="2"
        strokeLinecap="round"
        style={{ transformOrigin: '8px 20px', animation: tentacleAnim }}
      />
      <path
        d="M11,22 Q10,27 8,30"
        fill="none"
        stroke="#7c3aed"
        strokeWidth="2"
        strokeLinecap="round"
        style={{
          transformOrigin: '11px 22px',
          animation: `${tentacleAnim.replace('0.5s', '0.6s')}`,
        }}
      />
      <path
        d="M16,23 Q16,28 16,31"
        fill="none"
        stroke="#7c3aed"
        strokeWidth="2"
        strokeLinecap="round"
      />
      <path
        d="M21,22 Q22,27 24,30"
        fill="none"
        stroke="#7c3aed"
        strokeWidth="2"
        strokeLinecap="round"
        style={{
          transformOrigin: '21px 22px',
          animation: `${tentacleAnim.replace('0.5s', '0.55s')}`,
        }}
      />
      <path
        d="M24,20 Q26,26 28,28"
        fill="none"
        stroke="#7c3aed"
        strokeWidth="2"
        strokeLinecap="round"
        style={{ transformOrigin: '24px 20px', animation: tentacleAnim }}
      />
      <ellipse
        cx="12"
        cy="11"
        rx="2"
        ry={state === 'responding' ? 1 : 2}
        fill="white"
      />
      <ellipse
        cx="20"
        cy="11"
        rx="2"
        ry={state === 'responding' ? 1 : 2}
        fill="white"
      />
      <circle cx="12" cy="11" r="1" fill={D} />
      <circle cx="20" cy="11" r="1" fill={D} />
      <path
        d={
          state === 'orchestrating'
            ? 'M13,16 Q16,19 19,16'
            : 'M14,16 Q16,17 18,16'
        }
        fill="none"
        stroke={D}
        strokeWidth="0.8"
        strokeLinecap="round"
      />
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill="#a78bfa"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill="#a78bfa"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill="#a78bfa"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function DragonSVG({
  state,
  size,
}: {
  state: OrchestratorState
  size: number
}) {
  ensureStyles()
  const ey = state === 'thinking' ? 11 : 12.5
  const mouth =
    state === 'orchestrating'
      ? 'M13,17 Q16,20 19,17'
      : state === 'responding'
        ? 'M14,17 Q16,18.5 18,17'
        : 'M14.5,17 Q16,18 17.5,17'
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      {state === 'orchestrating' && (
        <circle
          cx="16"
          cy="16"
          r="15"
          fill="none"
          stroke="#16a34a"
          strokeWidth="1.5"
          opacity="0.4"
        >
          <animate
            attributeName="opacity"
            values="0.3;0.6;0.3"
            dur="1.5s"
            repeatCount="indefinite"
          />
        </circle>
      )}
      {/* Horns */}
      <polygon points="9,7 6,1 12,5" fill="#15803d" />
      <polygon points="23,7 20,5 26,1" fill="#15803d" />
      {/* Head */}
      <ellipse cx="16" cy="12" rx="8" ry="7" fill="#16a34a" />
      <ellipse cx="16" cy="13" rx="5.5" ry="4" fill="#22c55e" opacity="0.3" />
      {/* Snout */}
      <ellipse cx="16" cy="15.5" rx="4" ry="2.5" fill="#15803d" />
      {/* Nostrils — smoke when thinking */}
      <circle cx="14" cy="15" r="0.8" fill="#0f172a" opacity="0.5" />
      <circle cx="18" cy="15" r="0.8" fill="#0f172a" opacity="0.5" />
      {state === 'thinking' && (
        <>
          <circle cx="12" cy="13" r="1" fill="#94a3b8" opacity="0.3">
            <animate
              attributeName="cy"
              values="13;10;7"
              dur="1.5s"
              repeatCount="indefinite"
            />
            <animate
              attributeName="opacity"
              values="0.4;0.1;0"
              dur="1.5s"
              repeatCount="indefinite"
            />
          </circle>
          <circle cx="20" cy="13" r="0.8" fill="#94a3b8" opacity="0.3">
            <animate
              attributeName="cy"
              values="13;9;6"
              dur="1.8s"
              repeatCount="indefinite"
            />
            <animate
              attributeName="opacity"
              values="0.4;0.1;0"
              dur="1.8s"
              repeatCount="indefinite"
            />
          </circle>
        </>
      )}
      {/* Eyes */}
      <ellipse
        cx="12"
        cy={ey}
        rx="1.5"
        ry={state === 'responding' ? 0.7 : 1.4}
        fill="#0f172a"
      />
      <ellipse
        cx="20"
        cy={ey}
        rx="1.5"
        ry={state === 'responding' ? 0.7 : 1.4}
        fill="#0f172a"
      />
      <circle cx="12.4" cy={ey - 0.4} r="0.5" fill="#fbbf24" opacity="0.9" />
      <circle cx="20.4" cy={ey - 0.4} r="0.5" fill="#fbbf24" opacity="0.9" />
      <path
        d={mouth}
        fill="none"
        stroke="#0f172a"
        strokeWidth="0.8"
        strokeLinecap="round"
      />
      {/* Spine ridges */}
      <polygon points="16,5 14.5,7 17.5,7" fill="#15803d" />
      <polygon points="16,3 15,5 17,5" fill="#166534" />
      {/* Body */}
      <ellipse cx="16" cy="23" rx="6" ry="5" fill="#16a34a" />
      <ellipse cx="16" cy="23" rx="4" ry="3.5" fill="#bbf7d0" opacity="0.15" />
      {/* Tail */}
      <path
        d="M22,25 Q26,22 28,25 Q29,27 27,28"
        fill="none"
        stroke="#16a34a"
        strokeWidth="2"
        strokeLinecap="round"
        style={{
          transformOrigin: '22px 25px',
          animation:
            state !== 'idle' ? 'oa-tail-wag 0.7s ease-in-out infinite' : 'none',
        }}
      />
      <polygon points="27,28 29,26 29,30" fill="#15803d" />
      {/* Wings (small) */}
      <path
        d="M8,18 Q4,14 6,10"
        fill="none"
        stroke="#16a34a"
        strokeWidth="1.5"
        opacity="0.6"
      />
      <path
        d="M24,18 Q28,14 26,10"
        fill="none"
        stroke="#16a34a"
        strokeWidth="1.5"
        opacity="0.6"
      />
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill="#16a34a"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill="#16a34a"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill="#16a34a"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

function PandaSVG({ state, size }: { state: OrchestratorState; size: number }) {
  ensureStyles()
  const ey = state === 'thinking' ? 12.5 : 14
  const mouth =
    state === 'orchestrating'
      ? 'M14,19 Q16,21.5 18,19'
      : state === 'responding'
        ? 'M14.5,19 Q16,20 17.5,19'
        : 'M14.5,19 Q16,19.5 17.5,19'
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      style={{ animation: stateAnim(state) }}
    >
      {state === 'thinking' && (
        <circle
          cx="16"
          cy="16"
          r="14.5"
          fill="none"
          stroke="#eab308"
          strokeWidth="1.5"
          strokeDasharray="6 4"
          opacity="0.6"
          style={{ animation: 'oa-think-ring 2s linear infinite' }}
        />
      )}
      {/* Ears */}
      <circle cx="8" cy="6" r="4" fill="#1a1a2e" />
      <circle cx="24" cy="6" r="4" fill="#1a1a2e" />
      <circle cx="8" cy="6" r="2" fill="#374151" opacity="0.4" />
      <circle cx="24" cy="6" r="2" fill="#374151" opacity="0.4" />
      {/* Head */}
      <circle cx="16" cy="14" r="10" fill="white" />
      {/* Eye patches */}
      <ellipse
        cx="11.5"
        cy={ey - 0.5}
        rx="3.5"
        ry="3"
        fill="#1a1a2e"
        transform="rotate(-10 11.5 13.5)"
      />
      <ellipse
        cx="20.5"
        cy={ey - 0.5}
        rx="3.5"
        ry="3"
        fill="#1a1a2e"
        transform="rotate(10 20.5 13.5)"
      />
      {/* Eyes */}
      <ellipse
        cx="11.5"
        cy={ey}
        rx="1.4"
        ry={state === 'responding' ? 0.7 : 1.4}
        fill="white"
      />
      <ellipse
        cx="20.5"
        cy={ey}
        rx="1.4"
        ry={state === 'responding' ? 0.7 : 1.4}
        fill="white"
      />
      <circle cx="11.5" cy={ey} r="0.7" fill="#1a1a2e" />
      <circle cx="20.5" cy={ey} r="0.7" fill="#1a1a2e" />
      <circle cx="11.8" cy={ey - 0.3} r="0.3" fill="white" opacity="0.9" />
      <circle cx="20.8" cy={ey - 0.3} r="0.3" fill="white" opacity="0.9" />
      {/* Nose */}
      <ellipse cx="16" cy="17.5" rx="2" ry="1.2" fill="#1a1a2e" />
      <path
        d={mouth}
        fill="none"
        stroke="#1a1a2e"
        strokeWidth="0.8"
        strokeLinecap="round"
      />
      {/* Body */}
      <ellipse cx="16" cy="26" rx="7" ry="5" fill="white" />
      <ellipse cx="16" cy="26" rx="5" ry="3.5" fill="#1a1a2e" opacity="0.08" />
      {/* Arms */}
      <ellipse
        cx="8"
        cy="24"
        rx="3"
        ry="2"
        fill="#1a1a2e"
        transform="rotate(-20 8 24)"
      />
      <ellipse
        cx="24"
        cy="24"
        rx="3"
        ry="2"
        fill="#1a1a2e"
        transform="rotate(20 24 24)"
      />
      {/* Blush */}
      <circle cx="9" cy="17" r="1.5" fill="#fca5a5" opacity="0.35" />
      <circle cx="23" cy="17" r="1.5" fill="#fca5a5" opacity="0.35" />
      {state === 'responding' && (
        <g>
          <circle
            cx="12"
            cy="30.5"
            r="1"
            fill="#374151"
            style={{ animation: 'oa-dot1 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="16"
            cy="30.5"
            r="1"
            fill="#374151"
            style={{ animation: 'oa-dot2 1.2s ease-in-out infinite' }}
          />
          <circle
            cx="20"
            cy="30.5"
            r="1"
            fill="#374151"
            style={{ animation: 'oa-dot3 1.2s ease-in-out infinite' }}
          />
        </g>
      )}
    </svg>
  )
}

const AVATAR_RENDERERS: Record<
  AvatarStyle,
  React.FC<{ state: OrchestratorState; size: number }>
> = {
  lobster: LobsterSVG,
  'claw-cat': ClawCatSVG,
  robot: RobotSVG,
  fox: FoxSVG,
  owl: OwlSVG,
  ghost: GhostSVG,
  octopus: OctopusSVG,
  dragon: DragonSVG,
  panda: PandaSVG,
}

/* ── Dot colour per state ─────────────────────────────── */

const DOT_COLORS: Record<OrchestratorState, string> = {
  idle: '#6b7280',
  reading: '#3b82f6',
  thinking: '#eab308',
  responding: '#22c55e',
  'tool-use': '#8b5cf6',
  orchestrating: '#f97316',
}

/* ── Avatar Picker Popover ────────────────────────────── */

function AvatarPicker({
  current,
  onSelect,
}: {
  current: AvatarStyle
  onSelect: (s: AvatarStyle) => void
}) {
  return (
    <div
      className="grid grid-cols-3 gap-2 rounded-2xl border border-primary-300/70 bg-primary-100/95 p-3 shadow-xl backdrop-blur-xl"
      style={{ minWidth: 200 }}
    >
      <p className="col-span-3 mb-1 text-[11px] font-medium text-primary-700">
        Choose Avatar
      </p>
      {AVATAR_OPTIONS.map((opt) => (
        <button
          key={opt.id}
          type="button"
          onClick={() => onSelect(opt.id)}
          className={cn(
            'flex flex-col items-center gap-1 rounded-xl p-2.5 transition-all',
            current === opt.id
              ? 'bg-accent-500/20 ring-2 ring-accent-500 scale-105'
              : 'hover:bg-primary-200/60 hover:scale-105',
          )}
        >
          <span className="text-2xl">{opt.emoji}</span>
          <span className="text-[10px] font-medium text-primary-700">
            {opt.label}
          </span>
        </button>
      ))}
    </div>
  )
}

/* ── Main export ──────────────────────────────────────── */

type OrchestratorAvatarProps = {
  size?: number
  /** When true, hides tooltip, edit pencil, and picker — just the avatar + state dot */
  compact?: boolean
}

function OrchestratorAvatarComponent({ size = 48, compact = false }: OrchestratorAvatarProps) {
  const { state, label } = useOrchestratorState()
  const [avatarStyle, setAvatarStyle] = useState<AvatarStyle>(getStoredAvatar)
  const [showPicker, setShowPicker] = useState(false)

  const Renderer = AVATAR_RENDERERS[avatarStyle]
  const dotColor = DOT_COLORS[state]

  const handleSelect = useCallback((s: AvatarStyle) => {
    setAvatarStyle(s)
    setShowPicker(false)
    try {
      localStorage.setItem(STORAGE_KEY, s)
    } catch {
      /* noop */
    }
  }, [])

  const tooltipText = useMemo(() => `⚡ Aurora — ${label}`, [label])

  if (compact) {
    return (
      <div
        className="relative flex items-center justify-center rounded-full"
        style={{ width: size + 4, height: size + 4 }}
      >
        <Renderer state={state} size={size} />
        <span
          className="absolute bottom-0 right-0 block rounded-full border-2 border-surface"
          style={{
            width: Math.max(6, size / 6),
            height: Math.max(6, size / 6),
            backgroundColor: dotColor,
            transition: 'background-color 300ms ease',
          }}
        />
      </div>
    )
  }

  return (
    <div className="relative flex flex-col items-center gap-1">
      <TooltipProvider>
        <TooltipRoot>
          <TooltipTrigger
            render={
              <div
                className="relative flex items-center justify-center rounded-full transition-all duration-300"
                style={{ width: size + 4, height: size + 4 }}
              >
                <Renderer state={state} size={size} />
                {/* State dot */}
                <span
                  className="absolute bottom-0 right-0 block rounded-full border-2 border-primary-50"
                  style={{
                    width: Math.max(8, size / 6),
                    height: Math.max(8, size / 6),
                    backgroundColor: dotColor,
                    transition: 'background-color 300ms ease',
                  }}
                />
              </div>
            }
          />
          <TooltipContent side="right" className="text-xs">
            {tooltipText}
          </TooltipContent>
        </TooltipRoot>
      </TooltipProvider>

      {/* Edit pencil overlay */}
      <button
        type="button"
        onClick={() => setShowPicker((v) => !v)}
        className="absolute -right-1 -top-1 rounded-full border border-primary-300/70 bg-primary-100/90 p-1 text-primary-500 shadow-sm transition-all hover:bg-primary-200 hover:text-primary-800 hover:scale-110"
        aria-label="Change avatar"
      >
        <svg width="10" height="10" viewBox="0 0 16 16" fill="currentColor">
          <path d="M12.146.854a.5.5 0 0 1 .708 0l2.292 2.292a.5.5 0 0 1 0 .708L5.854 13.146a.5.5 0 0 1-.233.131l-3.5 1a.5.5 0 0 1-.617-.617l1-3.5a.5.5 0 0 1 .131-.233L12.146.854zM11.5 2.5 13.5 4.5" />
        </svg>
      </button>

      {/* Picker popover — fixed so it can't be clipped by parent overflow */}
      {showPicker && (
        <>
          <div
            className="fixed inset-0 z-[100] bg-black/20 backdrop-blur-sm animate-in fade-in duration-150"
            onClick={() => setShowPicker(false)}
          />
          <div className="fixed left-1/2 top-1/2 z-[101] -translate-x-1/2 -translate-y-1/2 animate-in zoom-in-95 fade-in duration-200">
            <AvatarPicker current={avatarStyle} onSelect={handleSelect} />
          </div>
        </>
      )}
    </div>
  )
}

export const OrchestratorAvatar = memo(OrchestratorAvatarComponent)
