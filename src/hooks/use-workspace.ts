/**
 * Phase 2.6: Workspace management hook
 * Handles auto-detection, persistence, and workspace selection
 */
import { useCallback, useEffect, useState } from 'react'

const STORAGE_KEY = 'clawsuite-workspace'

export type WorkspaceState = {
  /** Current workspace folder path (server-side) */
  path: string | null
  /** Display name (folder name only, no full path) */
  folderName: string | null
  /** Whether we're still detecting/loading */
  isLoading: boolean
  /** Error message if detection failed */
  error: string | null
  /** Source of the workspace (how it was resolved) */
  source: 'localStorage' | 'gateway' | 'default' | 'manual' | null
}

type WorkspaceInfo = {
  path: string
  folderName: string
  source: string
  isValid: boolean
}

async function fetchWorkspaceInfo(savedPath?: string): Promise<WorkspaceInfo> {
  const params = new URLSearchParams()
  if (savedPath) params.set('saved', savedPath)

  const response = await fetch(`/api/workspace?${params.toString()}`)
  if (!response.ok) throw new Error('Failed to detect workspace')

  return (await response.json()) as WorkspaceInfo
}

function getSavedWorkspace(): string | null {
  try {
    return localStorage.getItem(STORAGE_KEY)
  } catch {
    return null
  }
}

function saveWorkspace(workspacePath: string): void {
  try {
    localStorage.setItem(STORAGE_KEY, workspacePath)
  } catch {
    // localStorage not available
  }
}

function clearSavedWorkspace(): void {
  try {
    localStorage.removeItem(STORAGE_KEY)
  } catch {
    // localStorage not available
  }
}

export function useWorkspace() {
  const [state, setState] = useState<WorkspaceState>({
    path: null,
    folderName: null,
    isLoading: true,
    error: null,
    source: null,
  })

  const detect = useCallback(async () => {
    setState((prev) => ({ ...prev, isLoading: true, error: null }))

    try {
      const savedPath = getSavedWorkspace()
      const info = await fetchWorkspaceInfo(savedPath || undefined)

      if (info.isValid) {
        // Save the valid path for next time
        saveWorkspace(info.path)

        setState({
          path: info.path,
          folderName: info.folderName,
          isLoading: false,
          error: null,
          source: info.source as WorkspaceState['source'],
        })
      } else {
        // Saved path was invalid, clear it
        if (savedPath) clearSavedWorkspace()

        setState({
          path: null,
          folderName: null,
          isLoading: false,
          error: null,
          source: null,
        })
      }
    } catch (err) {
      setState({
        path: null,
        folderName: null,
        isLoading: false,
        error: err instanceof Error ? err.message : 'Detection failed',
        source: null,
      })
    }
  }, [])

  const setWorkspace = useCallback(
    (workspacePath: string, displayName: string) => {
      saveWorkspace(workspacePath)
      setState({
        path: workspacePath,
        folderName: displayName,
        isLoading: false,
        error: null,
        source: 'manual',
      })
    },
    [],
  )

  const clearWorkspace = useCallback(() => {
    clearSavedWorkspace()
    setState({
      path: null,
      folderName: null,
      isLoading: false,
      error: null,
      source: null,
    })
  }, [])

  useEffect(() => {
    void detect()
  }, [detect])

  return {
    ...state,
    detect,
    setWorkspace,
    clearWorkspace,
  }
}
