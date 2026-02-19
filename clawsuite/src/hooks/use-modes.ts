import { useState, useCallback, useEffect } from 'react'
import { useSettings } from './use-settings'

export interface Mode {
  id: string
  name: string
  preferredModel?: string
  smartSuggestionsEnabled: boolean
  onlySuggestCheaper: boolean
  preferredBudgetModel?: string
  preferredPremiumModel?: string
}

const STORAGE_KEY = 'openclaw-modes'

function loadModes(): Mode[] {
  try {
    const stored = localStorage.getItem(STORAGE_KEY)
    return stored ? JSON.parse(stored) : []
  } catch {
    return []
  }
}

function saveModes(modes: Mode[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(modes))
  } catch {
    // Ignore storage write failures (private mode/quota)
  }
}

export function useModes() {
  const [modes, setModes] = useState<Mode[]>(loadModes)
  const [appliedModeId, setAppliedModeId] = useState<string | null>(null)
  const { settings, updateSettings } = useSettings()

  // Sync modes to localStorage whenever they change
  useEffect(() => {
    saveModes(modes)
  }, [modes])

  // Check for settings drift
  const checkDrift = useCallback(
    (mode: Mode): boolean => {
      return (
        mode.smartSuggestionsEnabled !==
          (settings.smartSuggestionsEnabled ?? false) ||
        mode.onlySuggestCheaper !== (settings.onlySuggestCheaper ?? false) ||
        (mode.preferredBudgetModel !== undefined &&
          mode.preferredBudgetModel !==
            (settings.preferredBudgetModel ?? '')) ||
        (mode.preferredPremiumModel !== undefined &&
          mode.preferredPremiumModel !== (settings.preferredPremiumModel ?? ''))
      )
    },
    [settings],
  )

  // Clear applied mode if settings drift
  useEffect(() => {
    if (appliedModeId) {
      const mode = modes.find((m) => m.id === appliedModeId)
      if (mode && checkDrift(mode)) {
        setAppliedModeId(null)
      }
    }
  }, [appliedModeId, modes, checkDrift])

  const saveMode = useCallback(
    (
      name: string,
      includeCurrentModel: boolean,
      currentModel?: string,
    ): Mode | { error: string } => {
      // Check for duplicate names
      if (modes.some((m) => m.name.toLowerCase() === name.toLowerCase())) {
        return { error: 'A mode with this name already exists' }
      }

      const newMode: Mode = {
        id: crypto.randomUUID(),
        name,
        preferredModel: includeCurrentModel ? currentModel : undefined,
        smartSuggestionsEnabled: settings.smartSuggestionsEnabled ?? false,
        onlySuggestCheaper: settings.onlySuggestCheaper ?? false,
        preferredBudgetModel: settings.preferredBudgetModel ?? '',
        preferredPremiumModel: settings.preferredPremiumModel ?? '',
      }

      setModes((prev) => [...prev, newMode])
      setAppliedModeId(newMode.id)
      return newMode
    },
    [modes, settings],
  )

  const renameMode = useCallback(
    (id: string, newName: string): { error?: string } => {
      // Check for duplicate names (excluding current mode)
      if (
        modes.some(
          (m) => m.id !== id && m.name.toLowerCase() === newName.toLowerCase(),
        )
      ) {
        return { error: 'A mode with this name already exists' }
      }

      setModes((prev) =>
        prev.map((m) => (m.id === id ? { ...m, name: newName } : m)),
      )
      return {}
    },
    [modes],
  )

  const deleteMode = useCallback((id: string): void => {
    setModes((prev) => prev.filter((m) => m.id !== id))
    setAppliedModeId((prev) => (prev === id ? null : prev))
  }, [])

  const applyMode = useCallback(
    (mode: Mode): void => {
      // Apply settings immediately
      updateSettings({
        smartSuggestionsEnabled: mode.smartSuggestionsEnabled,
        onlySuggestCheaper: mode.onlySuggestCheaper,
        ...(mode.preferredBudgetModel
          ? { preferredBudgetModel: mode.preferredBudgetModel }
          : {}),
        ...(mode.preferredPremiumModel
          ? { preferredPremiumModel: mode.preferredPremiumModel }
          : {}),
      })

      // Mark as applied
      setAppliedModeId(mode.id)
    },
    [settings],
  )

  const getAppliedMode = useCallback((): Mode | null => {
    if (!appliedModeId) return null
    return modes.find((m) => m.id === appliedModeId) || null
  }, [appliedModeId, modes])

  const hasDrift = useCallback(
    (modeId: string): boolean => {
      const mode = modes.find((m) => m.id === modeId)
      return mode ? checkDrift(mode) : false
    },
    [modes, checkDrift],
  )

  return {
    modes,
    appliedModeId,
    saveMode,
    renameMode,
    deleteMode,
    applyMode,
    getAppliedMode,
    hasDrift,
  }
}
