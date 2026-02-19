import { useEffect, useMemo, useState } from 'react'
import { create } from 'zustand'
import { persist } from 'zustand/middleware'

export type ThemeMode = 'system' | 'light' | 'dark'
export type LoaderStyle =
  | 'dots'
  | 'braille-claw'
  | 'braille-orbit'
  | 'braille-breathe'
  | 'braille-pulse'
  | 'braille-wave'
  | 'lobster'
  | 'logo'
export const DEFAULT_CHAT_DISPLAY_NAME = 'User'

export type ChatSettings = {
  showToolMessages: boolean
  showReasoningBlocks: boolean
  theme: ThemeMode
  loaderStyle: LoaderStyle
  displayName: string
  avatarDataUrl: string | null
}

type ChatSettingsState = {
  settings: ChatSettings
  updateSettings: (updates: Partial<ChatSettings>) => void
}

function defaultChatSettings(): ChatSettings {
  return {
    showToolMessages: false,
    showReasoningBlocks: false,
    theme: 'light',
    loaderStyle: 'dots',
    displayName: DEFAULT_CHAT_DISPLAY_NAME,
    avatarDataUrl: null,
  }
}

function mergePersistedSettings(
  persistedState: unknown,
  currentState: ChatSettingsState,
): ChatSettingsState {
  if (
    !persistedState ||
    typeof persistedState !== 'object' ||
    !('settings' in persistedState)
  ) {
    return currentState
  }

  const state = persistedState as Partial<ChatSettingsState>
  return {
    ...currentState,
    ...state,
    settings: {
      ...currentState.settings,
      ...(state.settings || {}),
    },
  }
}

export const useChatSettingsStore = create<ChatSettingsState>()(
  persist(
    function createSettingsStore(set) {
      return {
        settings: defaultChatSettings(),
        updateSettings: function updateSettings(updates) {
          set(function applyUpdates(state) {
            return {
              settings: { ...state.settings, ...updates },
            }
          })
        },
      }
    },
    {
      name: 'chat-settings',
      merge: function merge(persistedState, currentState) {
        return mergePersistedSettings(persistedState, currentState)
      },
    },
  ),
)

export function getChatProfileDisplayName(displayName: string): string {
  const trimmed = displayName.trim()
  return trimmed.length > 0 ? trimmed : DEFAULT_CHAT_DISPLAY_NAME
}

export function selectChatProfileDisplayName(state: ChatSettingsState): string {
  return getChatProfileDisplayName(state.settings.displayName)
}

export function selectChatProfileAvatarDataUrl(
  state: ChatSettingsState,
): string | null {
  return state.settings.avatarDataUrl
}

export function useChatSettings() {
  const settings = useChatSettingsStore((state) => state.settings)
  const updateSettings = useChatSettingsStore((state) => state.updateSettings)

  return {
    settings,
    updateSettings,
  }
}

export function useResolvedTheme() {
  const theme = useChatSettingsStore((state) => state.settings.theme)
  const [systemIsDark, setSystemIsDark] = useState(false)

  useEffect(() => {
    if (typeof window === 'undefined') return
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    setSystemIsDark(media.matches)
    function handleChange(event: MediaQueryListEvent) {
      setSystemIsDark(event.matches)
    }
    media.addEventListener('change', handleChange)
    return () => media.removeEventListener('change', handleChange)
  }, [])

  return useMemo(() => {
    if (theme === 'dark') return 'dark'
    if (theme === 'light') return 'light'
    return systemIsDark ? 'dark' : 'light'
  }, [theme, systemIsDark])
}
