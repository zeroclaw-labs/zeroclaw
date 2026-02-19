import { useEffect, useRef, useCallback } from 'react'
import type { Mode } from '@/hooks/use-modes'

type ApplyModeDialogProps = {
  mode: Mode
  onConfirm: (switchModel: boolean) => void
  onClose: () => void
}

export function ApplyModeDialog({
  mode,
  onConfirm,
  onClose,
}: ApplyModeDialogProps) {
  const dialogRef = useRef<HTMLDivElement>(null)

  // Focus trap
  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        onClose()
        return
      }

      if (event.key === 'Tab') {
        const focusable = dialog!.querySelectorAll<HTMLElement>(
          'button, [tabindex]:not([tabindex="-1"])',
        )
        const first = focusable[0]
        const last = focusable[focusable.length - 1]

        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault()
          last.focus()
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault()
          first.focus()
        }
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => {
      document.removeEventListener('keydown', handleKeyDown)
    }
  }, [onClose])

  const handleSwitchNow = useCallback(() => {
    onConfirm(true)
  }, [onConfirm])

  const handleSkip = useCallback(() => {
    onConfirm(false)
  }, [onConfirm])

  return (
    <>
      {/* Backdrop */}
      <div
        className="fixed inset-0 z-50 bg-black/50"
        onClick={onClose}
        aria-hidden="true"
      />

      {/* Dialog */}
      <div
        ref={dialogRef}
        role="dialog"
        aria-labelledby="apply-mode-title"
        aria-modal="true"
        className="fixed left-1/2 top-1/2 z-50 w-full max-w-md -translate-x-1/2 -translate-y-1/2 rounded-xl border border-primary-200 bg-surface p-6 shadow-xl"
      >
        <h2
          id="apply-mode-title"
          className="mb-2 text-lg font-semibold text-primary-900"
        >
          Switch Model?
        </h2>

        <p className="mb-6 text-sm text-primary-600">
          Mode "{mode.name}" uses{' '}
          <span className="font-medium">{mode.preferredModel}</span>. Would you
          like to switch to this model now?
        </p>

        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={handleSkip}
            className="rounded-lg border border-primary-200 bg-surface px-4 py-2 text-sm font-medium text-primary-700 transition-colors hover:bg-primary-50 focus:outline-none focus:ring-2 focus:ring-primary-400"
          >
            Skip
          </button>
          <button
            type="button"
            onClick={handleSwitchNow}
            className="rounded-lg bg-primary-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-primary-700 focus:outline-none focus:ring-2 focus:ring-primary-400"
          >
            Switch Now
          </button>
        </div>
      </div>
    </>
  )
}
