import { useState, useCallback, useEffect, useRef, memo } from 'react'
import { cn } from '@/lib/utils'
import type { Mode } from '@/hooks/use-modes'

type SaveModeDialogProps = {
  currentModel: string
  onSave: (name: string, includeModel: boolean) => Mode | { error: string }
  onClose: () => void
}

export const SaveModeDialog = memo(function SaveModeDialog({
  currentModel,
  onSave,
  onClose,
}: SaveModeDialogProps) {
  const [name, setName] = useState('')
  const [includeModel, setIncludeModel] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const dialogRef = useRef<HTMLDivElement>(null)

  // Focus input on mount
  useEffect(() => {
    inputRef.current?.focus()
  }, [])

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
          'button, input, [tabindex]:not([tabindex="-1"])',
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

  const handleSave = useCallback(() => {
    const trimmed = name.trim()
    if (!trimmed) {
      setError('Mode name is required')
      return
    }

    const result = onSave(trimmed, includeModel)
    if ('error' in result) {
      setError(result.error)
    } else {
      onClose()
    }
  }, [name, includeModel, onSave, onClose])

  const handleSubmit = useCallback(
    (event: React.FormEvent) => {
      event.preventDefault()
      handleSave()
    },
    [handleSave],
  )

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
        aria-labelledby="save-mode-title"
        aria-modal="true"
        className="fixed left-1/2 top-1/2 z-50 w-full max-w-md -translate-x-1/2 -translate-y-1/2 rounded-xl border border-primary-200 bg-surface p-6 shadow-xl"
      >
        <h2
          id="save-mode-title"
          className="mb-4 text-lg font-semibold text-primary-900"
        >
          Save Mode
        </h2>

        <form onSubmit={handleSubmit}>
          <div className="mb-4">
            <label
              htmlFor="mode-name"
              className="mb-2 block text-sm font-medium text-primary-700"
            >
              Mode Name
            </label>
            <input
              ref={inputRef}
              id="mode-name"
              type="text"
              value={name}
              onChange={(e) => {
                setName(e.target.value)
                setError(null)
              }}
              className={cn(
                'w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-sm text-primary-900 placeholder-primary-400 focus:border-primary-400 focus:outline-none focus:ring-2 focus:ring-primary-400',
                error &&
                  'border-red-500 focus:border-red-500 focus:ring-red-500',
              )}
              placeholder="e.g., Work Mode"
              maxLength={50}
              aria-invalid={!!error}
              aria-describedby={error ? 'mode-name-error' : undefined}
            />
            {error && (
              <p
                id="mode-name-error"
                className="mt-1 text-xs text-red-600"
                role="alert"
              >
                {error}
              </p>
            )}
          </div>

          <div className="mb-6">
            <label className="flex items-center gap-2 text-sm text-primary-700">
              <input
                type="checkbox"
                checked={includeModel}
                onChange={(e) => setIncludeModel(e.target.checked)}
                className="size-4 rounded border-primary-300 text-primary-600 focus:ring-2 focus:ring-primary-400 focus:ring-offset-0"
              />
              <span>Include current model ({currentModel || 'none'})</span>
            </label>
            <p className="ml-6 mt-1 text-xs text-primary-500">
              If unchecked, applying this mode will only update settings (not
              model).
            </p>
          </div>

          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              className="rounded-lg border border-primary-200 bg-surface px-4 py-2 text-sm font-medium text-primary-700 transition-colors hover:bg-primary-50 focus:outline-none focus:ring-2 focus:ring-primary-400"
            >
              Cancel
            </button>
            <button
              type="submit"
              className="rounded-lg bg-primary-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-primary-700 focus:outline-none focus:ring-2 focus:ring-primary-400"
            >
              Save Mode
            </button>
          </div>
        </form>
      </div>
    </>
  )
})
