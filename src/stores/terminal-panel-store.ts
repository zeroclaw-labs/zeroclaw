import { create } from 'zustand'
import { persist } from 'zustand/middleware'

const DEFAULT_PANEL_HEIGHT = 280
const MIN_PANEL_HEIGHT = 100

export type TerminalTabStatus = 'active' | 'idle'

export type TerminalTab = {
  id: string
  title: string
  cwd: string
  sessionId: string | null
  status: TerminalTabStatus
}

type TerminalPanelState = {
  isPanelOpen: boolean
  panelHeight: number
  tabs: Array<TerminalTab>
  activeTabId: string
  terminalCounter: number
  setPanelOpen: (isOpen: boolean) => void
  togglePanel: () => void
  setPanelHeight: (height: number) => void
  createTab: (cwd?: string) => string
  closeTab: (tabId: string) => void
  closeAllTabs: () => void
  setActiveTab: (tabId: string) => void
  renameTab: (tabId: string, title: string) => void
  setTabSessionId: (tabId: string, sessionId: string | null) => void
  setTabStatus: (tabId: string, status: TerminalTabStatus) => void
}

function createDefaultTab(counter: number, cwd = '~'): TerminalTab {
  return {
    id: crypto.randomUUID(),
    title: `Terminal ${counter}`,
    cwd,
    sessionId: null,
    status: 'idle',
  }
}

export const useTerminalPanelStore = create<TerminalPanelState>()(
  persist(
    (set, get) => ({
      isPanelOpen: false,
      panelHeight: DEFAULT_PANEL_HEIGHT,
      tabs: [createDefaultTab(1)],
      activeTabId: '',
      terminalCounter: 1,
      setPanelOpen: function setPanelOpen(isOpen: boolean) {
        set({ isPanelOpen: isOpen })
      },
      togglePanel: function togglePanel() {
        set((state) => ({ isPanelOpen: !state.isPanelOpen }))
      },
      setPanelHeight: function setPanelHeight(height: number) {
        const clamped = Math.max(MIN_PANEL_HEIGHT, Math.round(height))
        set({ panelHeight: clamped })
      },
      createTab: function createTab(cwd = '~') {
        const { terminalCounter } = get()
        const nextCounter = terminalCounter + 1
        const tab = createDefaultTab(nextCounter, cwd)
        set((state) => ({
          tabs: [...state.tabs, tab],
          activeTabId: tab.id,
          terminalCounter: nextCounter,
          isPanelOpen: true,
        }))
        return tab.id
      },
      closeTab: function closeTab(tabId: string) {
        set((state) => {
          const nextTabs = state.tabs.filter((tab) => tab.id !== tabId)
          if (nextTabs.length === 0) {
            const fallbackTab = createDefaultTab(state.terminalCounter + 1)
            return {
              tabs: [fallbackTab],
              activeTabId: fallbackTab.id,
              terminalCounter: state.terminalCounter + 1,
              isPanelOpen: false,
            }
          }
          const activeTabId =
            state.activeTabId === tabId ? nextTabs[0].id : state.activeTabId
          return {
            tabs: nextTabs,
            activeTabId,
          }
        })
      },
      closeAllTabs: function closeAllTabs() {
        set((state) => {
          const fallbackTab = createDefaultTab(state.terminalCounter + 1)
          return {
            tabs: [fallbackTab],
            activeTabId: fallbackTab.id,
            terminalCounter: state.terminalCounter + 1,
            isPanelOpen: false,
          }
        })
      },
      setActiveTab: function setActiveTab(tabId: string) {
        set({ activeTabId: tabId })
      },
      renameTab: function renameTab(tabId: string, title: string) {
        const trimmed = title.trim()
        if (!trimmed) return
        set((state) => ({
          tabs: state.tabs.map((tab) =>
            tab.id === tabId ? { ...tab, title: trimmed } : tab,
          ),
        }))
      },
      setTabSessionId: function setTabSessionId(
        tabId: string,
        sessionId: string | null,
      ) {
        set((state) => ({
          tabs: state.tabs.map((tab) =>
            tab.id === tabId ? { ...tab, sessionId } : tab,
          ),
        }))
      },
      setTabStatus: function setTabStatus(
        tabId: string,
        status: TerminalTabStatus,
      ) {
        set((state) => ({
          tabs: state.tabs.map((tab) =>
            tab.id === tabId ? { ...tab, status } : tab,
          ),
        }))
      },
    }),
    {
      name: 'terminal-panel-state',
      partialize: function partialize(state) {
        return {
          isPanelOpen: state.isPanelOpen,
          panelHeight: state.panelHeight,
          tabs: state.tabs,
          activeTabId: state.activeTabId,
          terminalCounter: state.terminalCounter,
        }
      },
      onRehydrateStorage: function onRehydrateStorage() {
        return function onHydrated(state) {
          if (!state) return
          if (state.tabs.length === 0) {
            const fallback = createDefaultTab(state.terminalCounter + 1)
            state.tabs = [fallback]
            state.activeTabId = fallback.id
            state.terminalCounter += 1
            return
          }
          const activeExists = state.tabs.some(
            (tab) => tab.id === state.activeTabId,
          )
          if (!activeExists) {
            state.activeTabId = state.tabs[0].id
          }
        }
      },
    },
  ),
)

export { DEFAULT_PANEL_HEIGHT, MIN_PANEL_HEIGHT }
