import { useCallback, useSyncExternalStore } from 'react'

export type DashboardSettings = {
  /** ZIP code, city name, or empty for auto-detect via timezone */
  weatherLocation: string
  /** 12 or 24 hour clock */
  clockFormat: '12h' | '24h'
}

const STORAGE_KEY = 'openclaw-dashboard-settings'

const DEFAULT_SETTINGS: DashboardSettings = {
  weatherLocation: '',
  clockFormat: '12h',
}

let cached: DashboardSettings | null = null

function read(): DashboardSettings {
  if (cached) return cached
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<DashboardSettings>
      cached = { ...DEFAULT_SETTINGS, ...parsed }
      return cached
    }
  } catch {}
  cached = DEFAULT_SETTINGS
  return cached
}

function write(settings: DashboardSettings) {
  cached = settings
  localStorage.setItem(STORAGE_KEY, JSON.stringify(settings))
  // Notify subscribers
  for (const cb of listeners) cb()
}

const listeners = new Set<() => void>()

function subscribe(cb: () => void) {
  listeners.add(cb)
  return () => {
    listeners.delete(cb)
  }
}

function getSnapshot() {
  return read()
}

export function useDashboardSettings() {
  const settings = useSyncExternalStore(subscribe, getSnapshot, getSnapshot)

  const update = useCallback(function updateSettings(
    patch: Partial<DashboardSettings>,
  ) {
    write({ ...read(), ...patch })
  }, [])

  return { settings, update }
}
