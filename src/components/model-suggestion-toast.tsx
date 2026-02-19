/**
 * Phase 4.1: Model Suggestion Toast
 *
 * Non-intrusive toast notification for model suggestions
 */
import { useEffect } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import { Cancel01Icon } from '@hugeicons/core-free-icons'
import { Button } from '@/components/ui/button'

type ModelSuggestionToastProps = {
  suggestedModel: string
  reason: string
  costImpact?: string
  onSwitch: () => void
  onDismiss: () => void
  onDismissForSession: () => void
  autoDismissMs?: number
}

export function ModelSuggestionToast({
  suggestedModel,
  reason,
  costImpact,
  onSwitch,
  onDismiss,
  onDismissForSession,
  autoDismissMs = 15000,
}: ModelSuggestionToastProps) {
  useEffect(() => {
    const timer = setTimeout(onDismiss, autoDismissMs)
    return () => clearTimeout(timer)
  }, [autoDismissMs, onDismiss])

  return (
    <div className="fixed bottom-6 right-6 z-50 animate-in slide-in-from-bottom-2">
      <div className="flex w-[380px] flex-col gap-3 rounded-xl border border-primary-200 bg-primary-50/95 p-4 shadow-lg backdrop-blur-xl">
        <div className="flex items-start justify-between gap-2">
          <div className="flex items-start gap-2">
            <span className="text-lg">💡</span>
            <div>
              <p className="text-sm font-medium text-primary-900">
                Try {getModelDisplayName(suggestedModel)}?
              </p>
              <p className="mt-0.5 text-xs text-primary-600">{reason}</p>
              {costImpact && (
                <p className="mt-1 text-xs font-medium text-emerald-700">
                  {costImpact}
                </p>
              )}
            </div>
          </div>
          <button
            type="button"
            onClick={onDismiss}
            className="shrink-0 rounded-md p-1 text-primary-500 hover:bg-primary-200 hover:text-primary-900"
            aria-label="Dismiss"
          >
            <HugeiconsIcon icon={Cancel01Icon} size={16} strokeWidth={1.5} />
          </button>
        </div>

        <div className="flex gap-2">
          <Button size="sm" onClick={onSwitch} className="flex-1">
            Switch
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={onDismissForSession}
            className="flex-1"
          >
            Not for this session
          </Button>
        </div>
      </div>
    </div>
  )
}

function getModelDisplayName(modelId: string): string {
  // Extract readable name from model ID
  const parts = modelId.split('/')
  const name = parts[parts.length - 1]

  // Capitalize and format
  return name
    .replace(/-/g, ' ')
    .split(' ')
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ')
}
