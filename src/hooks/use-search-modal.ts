import { create } from 'zustand'

export const SEARCH_MODAL_EVENTS = {
  OPEN_SETTINGS: 'search-modal:open-settings',
  OPEN_USAGE: 'search-modal:open-usage',
  TOGGLE_FILE_EXPLORER: 'search-modal:toggle-file-explorer',
} as const

export type SearchScope =
  | 'all'
  | 'chats'
  | 'files'
  | 'agents'
  | 'skills'
  | 'actions'

type SearchModalState = {
  isOpen: boolean
  query: string
  scope: SearchScope
  openModal: () => void
  closeModal: () => void
  toggleModal: () => void
  setQuery: (value: string) => void
  clearQuery: () => void
  setScope: (value: SearchScope) => void
}

export const useSearchModal = create<SearchModalState>((set) => ({
  isOpen: false,
  query: '',
  scope: 'all',
  openModal: () => set({ isOpen: true }),
  closeModal: () => set({ isOpen: false }),
  toggleModal: () =>
    set((state) => ({
      isOpen: !state.isOpen,
    })),
  setQuery: (value) => set({ query: value }),
  clearQuery: () => set({ query: '' }),
  setScope: (value) => set({ scope: value }),
}))

export function emitSearchModalEvent(
  eventName: (typeof SEARCH_MODAL_EVENTS)[keyof typeof SEARCH_MODAL_EVENTS],
) {
  if (typeof window === 'undefined') return
  window.dispatchEvent(new CustomEvent(eventName))
}
