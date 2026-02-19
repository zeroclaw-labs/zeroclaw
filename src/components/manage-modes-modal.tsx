import { useState, useCallback, useEffect, useRef } from 'react'
import { cn } from '@/lib/utils'
import { useModes } from '@/hooks/use-modes'
import { RenameDialog } from './rename-mode-dialog'
import type { Mode } from '@/hooks/use-modes'

type ManageModesModalProps = {
  onClose: () => void
  availableModels: string[]
}

export function ManageModesModal({
  onClose,
  availableModels,
}: ManageModesModalProps) {
  const { modes, deleteMode } = useModes()
  const [modeToRename, setModeToRename] = useState<Mode | null>(null)
  const [modeToDelete, setModeToDelete] = useState<Mode | null>(null)
  const modalRef = useRef<HTMLDivElement>(null)

  // Focus trap
  useEffect(() => {
    const modal = modalRef.current
    if (!modal) return

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        onClose()
        return
      }

      if (event.key === 'Tab') {
        const focusable = modal!.querySelectorAll<HTMLElement>(
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

  const handleDelete = useCallback(
    (mode: Mode) => {
      deleteMode(mode.id)
      setModeToDelete(null)
    },
    [deleteMode],
  )

  if (modes.length === 0) {
    return (
      <>
        <div
          className="fixed inset-0 z-50 bg-black/50"
          onClick={onClose}
          aria-hidden="true"
        />
        <div
          ref={modalRef}
          role="dialog"
          aria-labelledby="manage-modes-title"
          aria-modal="true"
          className="fixed left-1/2 top-1/2 z-50 w-full max-w-2xl -translate-x-1/2 -translate-y-1/2 rounded-xl border border-primary-200 bg-surface p-6 shadow-xl"
        >
          <h2
            id="manage-modes-title"
            className="mb-4 text-lg font-semibold text-primary-900"
          >
            Manage Modes
          </h2>
          <p className="mb-6 text-sm text-primary-500">No modes saved.</p>
          <div className="flex justify-end">
            <button
              type="button"
              onClick={onClose}
              className="rounded-lg bg-primary-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-primary-700 focus:outline-none focus:ring-2 focus:ring-primary-400"
            >
              Close
            </button>
          </div>
        </div>
      </>
    )
  }

  return (
    <>
      {/* Backdrop */}
      <div
        className="fixed inset-0 z-50 bg-black/50"
        onClick={onClose}
        aria-hidden="true"
      />

      {/* Modal */}
      <div
        ref={modalRef}
        role="dialog"
        aria-labelledby="manage-modes-title"
        aria-modal="true"
        className="fixed left-1/2 top-1/2 z-50 w-full max-w-2xl -translate-x-1/2 -translate-y-1/2 rounded-xl border border-primary-200 bg-surface p-6 shadow-xl"
      >
        <h2
          id="manage-modes-title"
          className="mb-4 text-lg font-semibold text-primary-900"
        >
          Manage Modes
        </h2>

        <div className="mb-6 max-h-[24rem] space-y-3 overflow-y-auto">
          {modes.map((mode) => {
            const modelUnavailable =
              mode.preferredModel &&
              !availableModels.includes(mode.preferredModel)

            return (
              <div
                key={mode.id}
                className="rounded-lg border border-primary-200 bg-primary-50 p-4"
              >
                <div className="mb-2 flex items-start justify-between">
                  <h3 className="font-medium text-primary-900">
                    {mode.name}
                    {modelUnavailable && (
                      <span
                        className="ml-2 text-xs text-red-600"
                        title="Model unavailable"
                      >
                        ⚠️ Model unavailable
                      </span>
                    )}
                  </h3>
                  <div className="flex gap-2">
                    <button
                      type="button"
                      onClick={() => setModeToRename(mode)}
                      className="rounded-lg border border-primary-200 bg-surface px-3 py-1 text-xs font-medium text-primary-700 transition-colors hover:bg-primary-100 focus:outline-none focus:ring-2 focus:ring-primary-400"
                      aria-label={`Rename ${mode.name}`}
                    >
                      Rename
                    </button>
                    <button
                      type="button"
                      onClick={() => setModeToDelete(mode)}
                      className="rounded-lg border border-red-200 bg-surface px-3 py-1 text-xs font-medium text-red-700 transition-colors hover:bg-red-50 focus:outline-none focus:ring-2 focus:ring-red-400"
                      aria-label={`Delete ${mode.name}`}
                    >
                      Delete
                    </button>
                  </div>
                </div>

                <div className="space-y-1 text-xs text-primary-600">
                  {mode.preferredModel && (
                    <div>
                      <span className="font-medium">Model:</span>{' '}
                      <span className={cn(modelUnavailable && 'text-red-600')}>
                        {mode.preferredModel}
                      </span>
                    </div>
                  )}
                  <div>
                    <span className="font-medium">Smart Suggestions:</span>{' '}
                    {mode.smartSuggestionsEnabled ? 'On' : 'Off'}
                  </div>
                  <div>
                    <span className="font-medium">Only Suggest Cheaper:</span>{' '}
                    {mode.onlySuggestCheaper ? 'On' : 'Off'}
                  </div>
                  {mode.preferredBudgetModel && (
                    <div>
                      <span className="font-medium">Budget Model:</span>{' '}
                      {mode.preferredBudgetModel}
                    </div>
                  )}
                  {mode.preferredPremiumModel && (
                    <div>
                      <span className="font-medium">Premium Model:</span>{' '}
                      {mode.preferredPremiumModel}
                    </div>
                  )}
                </div>
              </div>
            )
          })}
        </div>

        <div className="flex justify-end">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg bg-primary-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-primary-700 focus:outline-none focus:ring-2 focus:ring-primary-400"
          >
            Close
          </button>
        </div>
      </div>

      {/* Rename Dialog */}
      {modeToRename && (
        <RenameDialog
          mode={modeToRename}
          onClose={() => setModeToRename(null)}
        />
      )}

      {/* Delete Confirmation */}
      {modeToDelete && (
        <>
          <div
            className="fixed inset-0 z-[60] bg-black/50"
            onClick={() => setModeToDelete(null)}
            aria-hidden="true"
          />
          <div
            role="dialog"
            aria-labelledby="delete-mode-title"
            aria-modal="true"
            className="fixed left-1/2 top-1/2 z-[60] w-full max-w-md -translate-x-1/2 -translate-y-1/2 rounded-xl border border-primary-200 bg-surface p-6 shadow-xl"
          >
            <h2
              id="delete-mode-title"
              className="mb-2 text-lg font-semibold text-primary-900"
            >
              Delete Mode
            </h2>
            <p className="mb-6 text-sm text-primary-600">
              Are you sure you want to delete "{modeToDelete.name}"? This action
              cannot be undone.
            </p>
            <div className="flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setModeToDelete(null)}
                className="rounded-lg border border-primary-200 bg-surface px-4 py-2 text-sm font-medium text-primary-700 transition-colors hover:bg-primary-50 focus:outline-none focus:ring-2 focus:ring-primary-400"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => handleDelete(modeToDelete)}
                className="rounded-lg bg-red-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-red-700 focus:outline-none focus:ring-2 focus:ring-red-400"
              >
                Delete
              </button>
            </div>
          </div>
        </>
      )}
    </>
  )
}
