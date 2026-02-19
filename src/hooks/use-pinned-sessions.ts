import { create } from 'zustand'
import { persist } from 'zustand/middleware'

type PinnedSessionsState = {
  pinnedSessionKeys: Array<string>
  pinSession: (key: string) => void
  unpinSession: (key: string) => void
  togglePinnedSession: (key: string) => void
  isSessionPinned: (key: string) => boolean
}

export const usePinnedSessionsStore = create<PinnedSessionsState>()(
  persist(
    (set, get) => ({
      pinnedSessionKeys: [],
      pinSession: (key) =>
        set((state) => {
          if (state.pinnedSessionKeys.includes(key)) return state
          return { pinnedSessionKeys: [...state.pinnedSessionKeys, key] }
        }),
      unpinSession: (key) =>
        set((state) => ({
          pinnedSessionKeys: state.pinnedSessionKeys.filter(
            (pinnedKey) => pinnedKey !== key,
          ),
        })),
      togglePinnedSession: (key) => {
        if (get().isSessionPinned(key)) {
          get().unpinSession(key)
          return
        }
        get().pinSession(key)
      },
      isSessionPinned: (key) => get().pinnedSessionKeys.includes(key),
    }),
    { name: 'pinned-sessions' },
  ),
)

export function usePinnedSessions() {
  const pinnedSessionKeys = usePinnedSessionsStore((s) => s.pinnedSessionKeys)
  const togglePinnedSession = usePinnedSessionsStore(
    (s) => s.togglePinnedSession,
  )
  return { pinnedSessionKeys, togglePinnedSession }
}
