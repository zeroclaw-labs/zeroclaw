import { useCallback, useState } from 'react'

import { readError } from '../utils'
import type { PathsPayload } from '../types'

export function useChatSettings() {
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [pathsLoading, setPathsLoading] = useState(false)
  const [pathsError, setPathsError] = useState<string | null>(null)
  const [paths, setPaths] = useState<PathsPayload | null>(null)

  const openSettings = useCallback(async () => {
    setSettingsOpen(true)
    setPathsError(null)

    if (pathsLoading || paths) return

    setPathsLoading(true)
    try {
      const res = await fetch('/api/paths')
      if (!res.ok) throw new Error(await readError(res))
      const data = (await res.json()) as {
        agentId?: string
        stateDir?: string
        sessionsDir?: string
        storePath?: string
      }
      setPaths({
        agentId: String(data.agentId ?? 'main'),
        stateDir: String(data.stateDir ?? ''),
        sessionsDir: String(data.sessionsDir ?? ''),
        storePath: String(data.storePath ?? ''),
      })
    } catch (err) {
      setPathsError(err instanceof Error ? err.message : String(err))
    } finally {
      setPathsLoading(false)
    }
  }, [paths, pathsLoading])

  const handleOpenSettings = useCallback(() => {
    void openSettings()
  }, [openSettings])

  const closeSettings = useCallback(() => {
    setSettingsOpen(false)
  }, [])

  const copySessionsDir = useCallback(() => {
    if (!paths?.sessionsDir) return
    try {
      void navigator.clipboard.writeText(paths.sessionsDir)
    } catch {
      // ignore
    }
  }, [paths])

  const copyStorePath = useCallback(() => {
    if (!paths?.storePath) return
    try {
      void navigator.clipboard.writeText(paths.storePath)
    } catch {
      // ignore
    }
  }, [paths])

  return {
    settingsOpen,
    setSettingsOpen,
    pathsLoading,
    pathsError,
    paths,
    handleOpenSettings,
    closeSettings,
    copySessionsDir,
    copyStorePath,
  }
}
