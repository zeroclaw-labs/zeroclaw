'use client'

import { useSyncExternalStore } from 'react'

type TitleSource = 'auto' | 'manual'

type SessionTitleStatus = 'idle' | 'generating' | 'ready' | 'error'

type PersistedTitle = {
  title?: string
  source?: TitleSource
  updatedAt?: number
}

type RuntimeState = {
  status?: SessionTitleStatus
  error?: string | null
}

export type SessionTitleInfo = {
  title?: string
  source?: TitleSource
  updatedAt?: number
  status: SessionTitleStatus
  error?: string | null
}

const STORAGE_KEY = 'clawsuite.sessionTitles.v1'

let persistedTitles: Record<string, PersistedTitle> = {}
const runtimeStates = new Map<string, RuntimeState>()
const listeners = new Set<() => void>()
let loaded = false

// Cached snapshot to prevent infinite re-renders
let cachedSnapshot: Record<string, SessionTitleInfo> | null = null

function ensureLoaded() {
  if (loaded || typeof window === 'undefined') return
  loaded = true
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (raw) {
      const parsed = JSON.parse(raw) as unknown
      if (parsed && typeof parsed === 'object') {
        persistedTitles = Object.fromEntries(
          Object.entries(parsed as Record<string, PersistedTitle>).map(
            ([key, value]) => {
              const normalized: PersistedTitle = {}
              // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
              if (value && typeof value === 'object') {
                if (
                  typeof value.title === 'string' &&
                  value.title.trim().length > 0
                ) {
                  normalized.title = value.title.trim()
                }
                if (value.source === 'auto' || value.source === 'manual') {
                  normalized.source = value.source
                }
                if (typeof value.updatedAt === 'number') {
                  normalized.updatedAt = value.updatedAt
                }
              }
              return [key, normalized]
            },
          ),
        )
        // Invalidate cache after loading from storage
        cachedSnapshot = null
      }
    }
  } catch {
    // ignore
  }
}

function persist() {
  if (typeof window === 'undefined') return
  try {
    const serializable = Object.fromEntries(
      Object.entries(persistedTitles).filter(([, value]) => {
        return Boolean(value.title) || Boolean(value.source)
      }),
    )
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(serializable))
  } catch {
    // ignore storage failures
  }
}

function notify() {
  // Invalidate cached snapshot when data changes
  cachedSnapshot = null
  for (const listener of listeners) listener()
}

function buildInfo(friendlyId: string): SessionTitleInfo {
  ensureLoaded()
  const persisted = persistedTitles[friendlyId] ?? {}
  const runtime = runtimeStates.get(friendlyId) ?? {}
  const title = persisted.title
  const source = persisted.source
  const status: SessionTitleStatus = runtime.status
    ? runtime.status
    : title
      ? 'ready'
      : 'idle'
  const error = runtime.error ?? null
  return {
    title,
    source,
    updatedAt: persisted.updatedAt,
    status,
    error,
  }
}

function getSnapshot(): Record<string, SessionTitleInfo> {
  ensureLoaded()
  // Return cached snapshot if available (prevents infinite re-renders)
  if (cachedSnapshot !== null) {
    return cachedSnapshot
  }
  const keys = new Set([
    ...Object.keys(persistedTitles),
    ...Array.from(runtimeStates.keys()),
  ])
  const result: Record<string, SessionTitleInfo> = {}
  for (const key of keys) {
    result[key] = buildInfo(key)
  }
  cachedSnapshot = result
  return result
}

function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

export function useSessionTitles() {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

export function useSessionTitleInfo(friendlyId: string): SessionTitleInfo {
  const map = useSessionTitles()
  // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
  return friendlyId && map[friendlyId]
    ? map[friendlyId]
    : { status: 'idle', error: null }
}

type SessionTitleUpdate = Partial<SessionTitleInfo>

export function updateSessionTitleState(
  friendlyId: string,
  patch: SessionTitleUpdate,
) {
  if (!friendlyId) return
  ensureLoaded()
  const prevPersisted = persistedTitles[friendlyId] ?? {}
  const prevRuntime = runtimeStates.get(friendlyId) ?? {}
  let nextPersisted: PersistedTitle = { ...prevPersisted }
  const nextRuntime: RuntimeState = { ...prevRuntime }

  if ('title' in patch) {
    const nextTitle = patch.title?.trim() ?? ''
    if (nextTitle.length > 0) {
      nextPersisted = {
        ...nextPersisted,
        title: nextTitle,
        source: patch.source ?? nextPersisted.source ?? 'auto',
        updatedAt: patch.updatedAt ?? nextPersisted.updatedAt ?? Date.now(),
      }
      nextRuntime.status = patch.status ?? 'ready'
      nextRuntime.error = null
    } else {
      nextPersisted = {}
      nextRuntime.status = patch.status ?? 'idle'
      nextRuntime.error = patch.error ?? null
    }
  }

  if ('source' in patch && patch.source) {
    nextPersisted = {
      ...nextPersisted,
      source: patch.source,
    }
  }

  if ('updatedAt' in patch && patch.updatedAt) {
    nextPersisted = {
      ...nextPersisted,
      updatedAt: patch.updatedAt,
    }
  }

  if ('status' in patch && !('title' in patch)) {
    nextRuntime.status = patch.status ?? nextRuntime.status
  }

  if ('error' in patch) {
    nextRuntime.error = patch.error ?? null
  }

  const hasPersistedData = Boolean(nextPersisted.title || nextPersisted.source)
  if (hasPersistedData) {
    persistedTitles = {
      ...persistedTitles,
      [friendlyId]: nextPersisted,
    }
  } else if (friendlyId in persistedTitles) {
    const { [friendlyId]: _removed, ...rest } = persistedTitles
    persistedTitles = rest
  }

  if (nextRuntime.status || nextRuntime.error) {
    runtimeStates.set(friendlyId, nextRuntime)
  } else {
    runtimeStates.delete(friendlyId)
  }

  persist()
  notify()
}

export function clearSessionTitleState(friendlyId: string) {
  if (!friendlyId) return
  ensureLoaded()
  if (friendlyId in persistedTitles) {
    const { [friendlyId]: _removed, ...rest } = persistedTitles
    persistedTitles = rest
  }
  runtimeStates.delete(friendlyId)
  persist()
  notify()
}
