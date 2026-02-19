'use client'

import { memo } from 'react'
import { DialogContent, DialogRoot } from '@/components/ui/dialog'
import { cn } from '@/lib/utils'

type ContextAlertModalProps = {
  open: boolean
  onClose: () => void
  threshold: number
  contextPercent: number
}

function ContextAlertModalComponent({
  open,
  onClose,
  threshold,
  contextPercent,
}: ContextAlertModalProps) {
  const isCritical = threshold >= 90
  const isDanger = threshold >= 75

  const barColor = isCritical
    ? 'bg-red-500'
    : isDanger
      ? 'bg-amber-500'
      : 'bg-amber-400'
  const iconBg = isCritical
    ? 'bg-red-100'
    : isDanger
      ? 'bg-amber-100'
      : 'bg-amber-100'
  const iconColor = isCritical
    ? 'text-red-600'
    : isDanger
      ? 'text-amber-600'
      : 'text-amber-600'

  return (
    <DialogRoot
      open={open}
      onOpenChange={(o) => {
        if (!o) onClose()
      }}
    >
      <DialogContent className="w-[min(440px,92vw)] p-0 overflow-hidden">
        {/* Colored top bar */}
        <div className={cn('h-1.5 w-full', barColor)} />

        <div className="px-6 pt-5 pb-6">
          {/* Icon + title */}
          <div className="flex items-start gap-3 mb-4">
            <div className={cn('rounded-full p-2 shrink-0', iconBg)}>
              <svg
                viewBox="0 0 24 24"
                className={cn('size-5', iconColor)}
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
                <line x1="12" y1="9" x2="12" y2="13" />
                <line x1="12" y1="17" x2="12.01" y2="17" />
              </svg>
            </div>
            <div>
              <h3 className="text-sm font-semibold text-primary-900">
                {isCritical
                  ? 'Context Window Almost Full'
                  : isDanger
                    ? 'Context Window Getting Full'
                    : 'Context Window Half Used'}
              </h3>
              <p className="text-xs text-primary-500 mt-0.5">
                {Math.round(contextPercent)}% of your model's context window is
                in use
              </p>
            </div>
          </div>

          {/* Progress bar */}
          <div className="w-full h-2.5 rounded-full bg-primary-100 overflow-hidden mb-4">
            <div
              className={cn(
                'h-full rounded-full transition-all duration-500',
                barColor,
              )}
              style={{ width: `${Math.min(contextPercent, 100)}%` }}
            />
          </div>

          {/* What this means */}
          <div className="bg-primary-50 rounded-lg p-3 mb-4">
            <p className="text-xs font-medium text-primary-800 mb-2">
              What does this mean?
            </p>
            <p className="text-xs text-primary-600 leading-relaxed">
              {isCritical
                ? "Your conversation history is nearly at the model's limit. Responses may become less accurate as the model loses access to earlier context. You should start a new chat soon."
                : isDanger
                  ? 'Your conversation is getting long. The model may start forgetting earlier messages. Consider starting a new chat for best results.'
                  : "You've used about half of the available context. This is normal for longer conversations — no action needed yet."}
            </p>
          </div>

          {/* Recommendations */}
          <div className="space-y-2 mb-5">
            <p className="text-xs font-medium text-primary-800">
              Recommendations
            </p>
            <div className="space-y-1.5">
              {isCritical && (
                <Recommendation
                  icon="🆕"
                  text="Start a new chat to reset context"
                  emphasis
                />
              )}
              <Recommendation
                icon="🗜️"
                text="Enable auto-compaction in Settings → Config to automatically manage context"
              />
              <Recommendation
                icon="📋"
                text="Summarize important details before starting a new chat"
              />
              {!isCritical && (
                <Recommendation
                  icon="💡"
                  text="Keep messages concise to use context efficiently"
                />
              )}
            </div>
          </div>

          {/* Actions */}
          <div className="flex justify-end gap-2">
            <button
              onClick={onClose}
              className="rounded-lg border border-primary-200 bg-surface px-4 py-2 text-xs font-medium text-primary-700 hover:bg-primary-50 transition-colors"
            >
              Got it
            </button>
            {isDanger && (
              <a
                href="/new"
                className="rounded-lg bg-primary-900 px-4 py-2 text-xs font-medium text-white hover:bg-primary-800 transition-colors"
              >
                New Chat
              </a>
            )}
          </div>
        </div>
      </DialogContent>
    </DialogRoot>
  )
}

function Recommendation({
  icon,
  text,
  emphasis,
}: {
  icon: string
  text: string
  emphasis?: boolean
}) {
  return (
    <div className="flex items-start gap-2">
      <span className="text-xs shrink-0 mt-px">{icon}</span>
      <span
        className={cn(
          'text-xs text-primary-600 leading-relaxed',
          emphasis && 'font-medium text-primary-800',
        )}
      >
        {text}
      </span>
    </div>
  )
}

export const ContextAlertModal = memo(ContextAlertModalComponent)
