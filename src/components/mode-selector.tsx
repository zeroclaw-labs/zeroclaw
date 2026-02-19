import { ArrowDown01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useEffect, useRef, useState, useCallback } from 'react'
import { cn } from '@/lib/utils'
import { useModes } from '@/hooks/use-modes'
import { SaveModeDialog } from './save-mode-dialog'
import { ManageModesModal } from './manage-modes-modal'
import { ApplyModeDialog } from './apply-mode-dialog'
import type { Mode } from '@/hooks/use-modes'

type ModeSelectorProps = {
  currentModel: string
  onModelSwitch: (modelId: string) => void
  disabled?: boolean
  availableModels: string[]
  isStreaming?: boolean
}

export function ModeSelector({
  currentModel,
  onModelSwitch,
  disabled = false,
  availableModels,
  isStreaming = false,
}: ModeSelectorProps) {
  const [isMenuOpen, setIsMenuOpen] = useState(false)
  const [showSaveDialog, setShowSaveDialog] = useState(false)
  const [showManageModal, setShowManageModal] = useState(false)
  const [modeToApply, setModeToApply] = useState<Mode | null>(null)
  const selectorRef = useRef<HTMLDivElement>(null)

  const {
    modes,
    appliedModeId,
    saveMode,
    applyMode,
    getAppliedMode,
    hasDrift,
  } = useModes()

  const appliedMode = getAppliedMode()
  const buttonLabel = appliedMode ? appliedMode.name : 'Mode'

  // Close menu on outside click (skip if dialog is open)
  useEffect(() => {
    if (!isMenuOpen) return
    function handleOutsideClick(event: MouseEvent) {
      if (!selectorRef.current) return
      if (selectorRef.current.contains(event.target as Node)) return
      // Don't close if clicking inside a dialog/modal overlay
      const target = event.target as HTMLElement
      if (target.closest('[role="dialog"]') || target.closest('.fixed')) return
      setIsMenuOpen(false)
    }

    document.addEventListener('mousedown', handleOutsideClick)
    return () => {
      document.removeEventListener('mousedown', handleOutsideClick)
    }
  }, [isMenuOpen])

  const handleApplyMode = useCallback(
    (mode: Mode) => {
      // Check if mode requires model switch
      const needsModelSwitch =
        mode.preferredModel && mode.preferredModel !== currentModel
      const modelAvailable =
        mode.preferredModel && availableModels.includes(mode.preferredModel)

      if (needsModelSwitch && !modelAvailable) {
        // Model unavailable - show warning but apply settings
        applyMode(mode)
        setIsMenuOpen(false)
        // Warning will be shown in UI via drift indicator
        return
      }

      if (needsModelSwitch && !isStreaming) {
        // Model available and not streaming - show confirmation
        setModeToApply(mode)
        setIsMenuOpen(false)
        return
      }

      // No model switch needed, or streaming (apply immediately)
      applyMode(mode)
      setIsMenuOpen(false)
    },
    [applyMode, currentModel, availableModels, isStreaming],
  )

  const handleConfirmApply = useCallback(
    (switchModel: boolean) => {
      if (!modeToApply) return
      applyMode(modeToApply)
      if (switchModel && modeToApply.preferredModel) {
        onModelSwitch(modeToApply.preferredModel)
      }
      setModeToApply(null)
    },
    [modeToApply, applyMode, onModelSwitch],
  )

  const handleSaveMode = useCallback(
    (name: string, includeModel: boolean) => {
      const result = saveMode(name, includeModel, currentModel)
      if (!('error' in result)) {
        setShowSaveDialog(false)
      }
      return result
    },
    [saveMode, currentModel],
  )

  const handleCloseSaveDialog = useCallback(() => {
    setShowSaveDialog(false)
  }, [])

  const showDrift = appliedMode && hasDrift(appliedMode.id)
  const modelUnavailable =
    appliedMode?.preferredModel &&
    !availableModels.includes(appliedMode.preferredModel)

  return (
    <>
      <div className="relative flex items-center gap-2" ref={selectorRef}>
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation()
            if (disabled) return
            setIsMenuOpen((prev) => !prev)
          }}
          className={cn(
            'inline-flex h-8 items-center gap-1 rounded-full border border-primary-200 bg-primary-50 px-3 text-xs font-medium text-primary-700 transition-colors hover:bg-primary-100',
            disabled && 'cursor-not-allowed opacity-50',
          )}
          aria-haspopup="menu"
          aria-expanded={!disabled && isMenuOpen}
          aria-disabled={disabled}
          aria-label="Mode selector"
          disabled={disabled}
        >
          <span className="max-w-[8rem] truncate">{buttonLabel}</span>
          {showDrift && (
            <span className="text-yellow-600" title="Settings changed">
              ⚠️
            </span>
          )}
          {modelUnavailable && (
            <span className="text-red-600" title="Model unavailable">
              ⚠️
            </span>
          )}
          <HugeiconsIcon icon={ArrowDown01Icon} size={20} strokeWidth={1.5} />
        </button>

        {!disabled && isMenuOpen && (
          <div className="absolute bottom-[calc(100%+0.5rem)] left-0 z-40 min-w-[14rem] max-w-[20rem] rounded-xl border border-primary-200 bg-surface shadow-lg">
            <div className="max-h-[20rem] overflow-y-auto p-1">
              {modes.length === 0 ? (
                <div className="p-4 text-center text-sm text-primary-500">
                  No modes saved
                </div>
              ) : (
                <>
                  {modes.map((mode) => {
                    const isApplied = appliedModeId === mode.id
                    const drift = isApplied && hasDrift(mode.id)
                    const unavailable =
                      mode.preferredModel &&
                      !availableModels.includes(mode.preferredModel)

                    return (
                      <button
                        key={mode.id}
                        type="button"
                        onClick={(event) => {
                          event.stopPropagation()
                          handleApplyMode(mode)
                        }}
                        className={cn(
                          'flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left text-sm text-primary-700 transition-colors hover:bg-primary-100',
                          isApplied && 'bg-primary-100 text-primary-900',
                        )}
                        role="menuitem"
                        aria-label={`Apply mode ${mode.name}`}
                      >
                        <span className="flex-1 truncate">{mode.name}</span>
                        {drift && (
                          <span
                            className="text-yellow-600 text-xs"
                            title="Settings changed"
                          >
                            ⚠️
                          </span>
                        )}
                        {unavailable && (
                          <span
                            className="text-red-600 text-xs"
                            title="Model unavailable"
                          >
                            ⚠️
                          </span>
                        )}
                        {isApplied && !drift && (
                          <span
                            className="text-primary-900"
                            aria-label="Currently active"
                          >
                            ✓
                          </span>
                        )}
                      </button>
                    )
                  })}
                </>
              )}

              <div className="my-1 border-t border-primary-200" />

              <button
                type="button"
                onClick={(event) => {
                  event.stopPropagation()
                  setIsMenuOpen(false)
                  setShowSaveDialog(true)
                }}
                className="flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left text-sm text-primary-700 transition-colors hover:bg-primary-100"
                role="menuitem"
                aria-label="Save current settings as new mode"
              >
                Save Current as New Mode...
              </button>

              {modes.length > 0 && (
                <button
                  type="button"
                  onClick={(event) => {
                    event.stopPropagation()
                    setIsMenuOpen(false)
                    setShowManageModal(true)
                  }}
                  className="flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left text-sm text-primary-700 transition-colors hover:bg-primary-100"
                  role="menuitem"
                  aria-label="Manage modes"
                >
                  Manage Modes...
                </button>
              )}
            </div>
          </div>
        )}
      </div>

      {showSaveDialog && (
        <SaveModeDialog
          currentModel={currentModel}
          onSave={handleSaveMode}
          onClose={handleCloseSaveDialog}
        />
      )}

      {showManageModal && (
        <ManageModesModal
          onClose={() => setShowManageModal(false)}
          availableModels={availableModels}
        />
      )}

      {modeToApply && (
        <ApplyModeDialog
          mode={modeToApply}
          onConfirm={handleConfirmApply}
          onClose={() => setModeToApply(null)}
        />
      )}
    </>
  )
}
