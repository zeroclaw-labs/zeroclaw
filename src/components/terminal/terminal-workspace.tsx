import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  Add01Icon,
  ArrowLeft01Icon,
  ArrowRight01Icon,
  Cancel01Icon,
  ComputerTerminal01Icon,
  SidebarLeft01Icon,
} from '@hugeicons/core-free-icons'
import type { FitAddon } from 'xterm-addon-fit'
import type * as FitAddonModule from 'xterm-addon-fit'
import type { Terminal } from 'xterm'
import type * as XtermModule from 'xterm'
import type * as WebLinksAddonModule from 'xterm-addon-web-links'
import type { DebugAnalysis } from '@/components/terminal/debug-panel'
import type { TerminalTab } from '@/stores/terminal-panel-store'
import { DebugPanel } from '@/components/terminal/debug-panel'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { useTerminalPanelStore } from '@/stores/terminal-panel-store'

// Dynamic imports to avoid SSR crash (xterm uses `self` which doesn't exist on server)
let xtermLoaded = false
let TerminalCtor: typeof XtermModule.Terminal
let FitAddonCtor: typeof FitAddonModule.FitAddon
let WebLinksAddonCtor: typeof WebLinksAddonModule.WebLinksAddon

async function ensureXterm() {
  if (xtermLoaded) return
  const [xtermMod, fitMod, linksMod] = await Promise.all([
    import('xterm'),
    import('xterm-addon-fit'),
    import('xterm-addon-web-links'),
  ])
  // Load CSS on client only
  await import('xterm/css/xterm.css')
  TerminalCtor = xtermMod.Terminal
  FitAddonCtor = fitMod.FitAddon
  WebLinksAddonCtor = linksMod.WebLinksAddon
  xtermLoaded = true
}

type ContextMenuState = {
  tabId: string
  x: number
  y: number
}

type TerminalWorkspaceProps = {
  mode: 'panel' | 'fullscreen'
  panelVisible?: boolean
  onMinimizePanel?: () => void
  onMaximizePanel?: () => void
  onClosePanel?: () => void
  onBack?: () => void
}

type TerminalSessionResponse = {
  sessionId?: string
}

const DEFAULT_TERMINAL_CWD = '~/.openclaw/workspace'
const TERMINAL_BG = '#0d0d0d'

function toDebugAnalysis(value: unknown): DebugAnalysis | null {
  if (!value || typeof value !== 'object') return null
  const entry = value as Record<string, unknown>
  const summary = typeof entry.summary === 'string' ? entry.summary.trim() : ''
  const rootCause =
    typeof entry.rootCause === 'string' ? entry.rootCause.trim() : ''
  const rawCommands = Array.isArray(entry.suggestedCommands)
    ? entry.suggestedCommands
    : []

  if (!summary || !rootCause) return null

  const suggestedCommands = rawCommands
    .map(function mapCommand(commandEntry) {
      if (!commandEntry || typeof commandEntry !== 'object') return null
      const command = commandEntry as Record<string, unknown>
      const commandText =
        typeof command.command === 'string' ? command.command.trim() : ''
      const descriptionText =
        typeof command.description === 'string'
          ? command.description.trim()
          : ''
      if (!commandText || !descriptionText) return null
      return { command: commandText, description: descriptionText }
    })
    .filter(function removeNulls(command): command is {
      command: string
      description: string
    } {
      return Boolean(command)
    })

  const docsLink =
    typeof entry.docsLink === 'string' && entry.docsLink.trim()
      ? entry.docsLink.trim()
      : undefined

  return {
    summary,
    rootCause,
    suggestedCommands,
    ...(docsLink ? { docsLink } : {}),
  }
}

export function TerminalWorkspace({
  mode,
  panelVisible = true,
  onMinimizePanel,
  onMaximizePanel,
  onClosePanel,
  onBack,
}: TerminalWorkspaceProps) {
  const tabs = useTerminalPanelStore((state) => state.tabs)
  const activeTabId = useTerminalPanelStore((state) => state.activeTabId)
  const createTab = useTerminalPanelStore((state) => state.createTab)
  const closeTab = useTerminalPanelStore((state) => state.closeTab)
  const closeAllTabs = useTerminalPanelStore((state) => state.closeAllTabs)
  const setActiveTab = useTerminalPanelStore((state) => state.setActiveTab)
  const renameTab = useTerminalPanelStore((state) => state.renameTab)
  const setTabSessionId = useTerminalPanelStore(
    (state) => state.setTabSessionId,
  )
  const setTabStatus = useTerminalPanelStore((state) => state.setTabStatus)

  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null)
  const [debugAnalysis, setDebugAnalysis] = useState<DebugAnalysis | null>(null)
  const [debugLoading, setDebugLoading] = useState(false)
  const [showDebugPanel, setShowDebugPanel] = useState(false)

  const containerMapRef = useRef(new Map<string, HTMLDivElement>())
  const terminalMapRef = useRef(new Map<string, Terminal>())
  const fitMapRef = useRef(new Map<string, FitAddon>())
  const readerMapRef = useRef(
    new Map<string, ReadableStreamDefaultReader<Uint8Array>>(),
  )
  const connectedRef = useRef(new Set<string>())

  const activeTab = useMemo(
    function activeTabMemo() {
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
      return tabs.find((tab) => tab.id === activeTabId) ?? tabs[0] ?? null
    },
    [activeTabId, tabs],
  )

  const sendInput = useCallback(async function sendInput(
    tabId: string,
    data: string,
  ) {
    // Look up session ID from store at call time (not stale closure)
    const currentTab = useTerminalPanelStore
      .getState()
      .tabs.find((t) => t.id === tabId)
    if (!currentTab?.sessionId) return
    await fetch('/api/terminal-input', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ sessionId: currentTab.sessionId, data }),
    }).catch(function ignore() {
      return undefined
    })
  }, [])

  const resizeSession = useCallback(async function resizeSession(
    tabId: string,
    terminal: Terminal,
  ) {
    const currentTab = useTerminalPanelStore
      .getState()
      .tabs.find((t) => t.id === tabId)
    if (!currentTab?.sessionId) return
    await fetch('/api/terminal-resize', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        sessionId: currentTab.sessionId,
        cols: terminal.cols,
        rows: terminal.rows,
      }),
    }).catch(function ignore() {
      return undefined
    })
  }, [])

  const captureRecentTerminalOutput = useCallback(
    function captureRecentTerminalOutput(tabId: string): string {
      const terminal = terminalMapRef.current.get(tabId)
      if (!terminal) return ''

      const buffer = terminal.buffer.active
      const startLine = Math.max(0, buffer.length - 100)
      const recentLines: Array<string> = []

      for (let index = startLine; index < buffer.length; index += 1) {
        const line = buffer.getLine(index)
        if (!line) continue
        recentLines.push(line.translateToString(true))
      }

      return recentLines.join('\n').trim()
    },
    [],
  )

  const handleAnalyzeDebug = useCallback(
    async function handleAnalyzeDebug() {
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
      if (!activeTab) return

      setShowDebugPanel(true)
      setDebugLoading(true)
      setDebugAnalysis(null)

      try {
        const terminalOutput = captureRecentTerminalOutput(activeTab.id)
        const response = await fetch('/api/debug-analyze', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ terminalOutput }),
        })

        const payload = (await response.json().catch(function fallback() {
          return null
        })) as unknown

        const analysis = toDebugAnalysis(payload)
        if (!analysis) {
          throw new Error('Invalid analysis response payload')
        }

        setDebugAnalysis(analysis)
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error)
        setDebugAnalysis({
          summary: 'Debug analysis failed.',
          rootCause: message,
          suggestedCommands: [],
        })
      } finally {
        setDebugLoading(false)
      }
    },
    [activeTab, captureRecentTerminalOutput],
  )

  const handleRunDebugCommand = useCallback(
    function handleRunDebugCommand(command: string) {
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
      if (!activeTab) return
      void sendInput(activeTab.id, `${command}\r`)
    },
    [activeTab, sendInput],
  )

  const handleCloseDebugPanel = useCallback(function handleCloseDebugPanel() {
    setShowDebugPanel(false)
  }, [])

  const focusActiveTerminal = useCallback(
    function focusActiveTerminal() {
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
      if (!activeTab) return
      const terminal = terminalMapRef.current.get(activeTab.id)
      terminal?.focus()
    },
    [activeTab],
  )

  const closeTabResources = useCallback(async function closeTabResources(
    tabId: string,
    sessionId: string | null,
  ) {
    const reader = readerMapRef.current.get(tabId)
    readerMapRef.current.delete(tabId)
    if (reader) {
      await reader.cancel().catch(function ignore() {
        return undefined
      })
    }
    const terminal = terminalMapRef.current.get(tabId)
    terminal?.dispose()
    terminalMapRef.current.delete(tabId)
    fitMapRef.current.delete(tabId)
    containerMapRef.current.delete(tabId)
    connectedRef.current.delete(tabId)

    if (sessionId) {
      await fetch('/api/terminal-close', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ sessionId }),
      }).catch(function ignore() {
        return undefined
      })
    }
  }, [])

  const handleCloseTab = useCallback(
    function handleCloseTab(tab: TerminalTab) {
      void closeTabResources(tab.id, tab.sessionId)
      closeTab(tab.id)
    },
    [closeTab, closeTabResources],
  )

  const handleClosePanel = useCallback(
    function handleClosePanel() {
      const currentTabs = useTerminalPanelStore.getState().tabs
      for (const tab of currentTabs) {
        void closeTabResources(tab.id, tab.sessionId)
      }
      closeAllTabs()
      setShowDebugPanel(false)
      if (onClosePanel) onClosePanel()
    },
    [closeAllTabs, closeTabResources, onClosePanel],
  )

  const connectTab = useCallback(
    async function connectTab(tab: TerminalTab) {
      if (connectedRef.current.has(tab.id)) return
      const terminal = terminalMapRef.current.get(tab.id)
      if (!terminal) return

      connectedRef.current.add(tab.id)
      setTabStatus(tab.id, 'active')

      const response = await fetch('/api/terminal-stream', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          cwd: DEFAULT_TERMINAL_CWD,
          // Let the server pick the shell from $SHELL
          cols: terminal.cols,
          rows: terminal.rows,
        }),
      }).catch(function handleError() {
        return null
      })

      if (!response || !response.ok || !response.body) {
        terminal.writeln('\r\n[terminal] failed to connect\r\n')
        connectedRef.current.delete(tab.id)
        setTabStatus(tab.id, 'idle')
        return
      }

      const reader = response.body.getReader()
      readerMapRef.current.set(tab.id, reader)
      const decoder = new TextDecoder()
      let buffer = ''

      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
      while (true) {
        const readState = await reader.read().catch(function onReadError() {
          return { done: true, value: undefined }
        })
        const value = readState.value
        if (readState.done) break
        if (!value) continue

        buffer += decoder.decode(value, { stream: true })
        const blocks = buffer.split('\n\n')
        buffer = blocks.pop() ?? ''

        for (const block of blocks) {
          if (!block.trim()) continue
          const lines = block.split('\n')
          let eventName = ''
          let eventData = ''
          for (const line of lines) {
            if (line.startsWith('event: ')) {
              eventName = line.slice(7).trim()
              continue
            }
            if (line.startsWith('data: ')) {
              eventData += line.slice(6)
              continue
            }
            if (line.startsWith('data:')) {
              eventData += line.slice(5)
            }
          }
          if (!eventName || eventName === 'ping') continue

          if (eventName === 'session' && eventData) {
            const payload = JSON.parse(eventData) as TerminalSessionResponse
            if (payload.sessionId) {
              setTabSessionId(tab.id, payload.sessionId)
              const nextTitle = tab.cwd === '~' ? tab.title : tab.cwd
              renameTab(tab.id, nextTitle)
            }
            continue
          }

          if (eventName === 'data' && eventData) {
            const payload = JSON.parse(eventData) as { data?: string }
            if (typeof payload.data === 'string') {
              terminal.write(payload.data)
            }
            continue
          }

          if (eventName === 'exit' && eventData) {
            const payload = JSON.parse(eventData) as {
              exitCode?: number
              signal?: number
            }
            terminal.writeln(
              `\r\n[process exited${payload.exitCode != null ? ` code=${payload.exitCode}` : ''}]\r\n`,
            )
            continue
          }

          if (eventName === 'error' && eventData) {
            terminal.writeln('\r\n[terminal] connection error\r\n')
          }
        }
      }

      const latestTab = useTerminalPanelStore
        .getState()
        .tabs.find((item) => item.id === tab.id)
      if (latestTab?.sessionId) {
        await fetch('/api/terminal-close', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ sessionId: latestTab.sessionId }),
        }).catch(function ignore() {
          return undefined
        })
      }
      setTabSessionId(tab.id, null)
      setTabStatus(tab.id, 'idle')
      connectedRef.current.delete(tab.id)
    },
    [renameTab, setTabSessionId, setTabStatus],
  )

  const ensureTerminalForTab = useCallback(
    function ensureTerminalForTab(tab: TerminalTab) {
      if (terminalMapRef.current.has(tab.id)) return
      const container = containerMapRef.current.get(tab.id)
      if (!container) return

      // Guard: xterm must be loaded first
      if (!xtermLoaded) {
        void ensureXterm().then(() => {
          // Re-trigger after load
          if (
            !terminalMapRef.current.has(tab.id) &&
            containerMapRef.current.has(tab.id)
          ) {
            ensureTerminalForTab(tab)
          }
        })
        return
      }

      const terminal = new TerminalCtor({
        cursorBlink: true,
        fontSize: 13,
        fontFamily: 'JetBrains Mono, Menlo, Monaco, Consolas, monospace',
        theme: {
          background: TERMINAL_BG,
          foreground: '#e6e6e6',
          cursor: '#ea580c',
          selectionBackground: '#2b2b2b',
        },
      })
      const fitAddon = new FitAddonCtor()
      const webLinks = new WebLinksAddonCtor()
      terminal.loadAddon(fitAddon)
      terminal.loadAddon(webLinks)
      terminal.open(container)
      fitAddon.fit()

      terminal.onData(function onData(data) {
        void sendInput(tab.id, data)
      })

      terminalMapRef.current.set(tab.id, terminal)
      fitMapRef.current.set(tab.id, fitAddon)
      void resizeSession(tab.id, terminal)
      void connectTab(tab)
    },
    [connectTab, resizeSession, sendInput],
  )

  const handleCreateTab = useCallback(
    function handleCreateTab() {
      const newTabId = createTab(DEFAULT_TERMINAL_CWD)
      window.setTimeout(function focusNewTab() {
        const tab = useTerminalPanelStore
          .getState()
          .tabs.find((item) => item.id === newTabId)
        if (!tab) return
        ensureTerminalForTab(tab)
        focusActiveTerminal()
      }, 0)
    },
    [createTab, ensureTerminalForTab, focusActiveTerminal],
  )

  useEffect(
    function closeContextMenuOnClick() {
      if (!contextMenu) return
      function handlePointerDown() {
        setContextMenu(null)
      }
      function handleEscape(event: KeyboardEvent) {
        if (event.key === 'Escape') {
          setContextMenu(null)
        }
      }
      window.addEventListener('pointerdown', handlePointerDown)
      window.addEventListener('keydown', handleEscape)
      return function cleanup() {
        window.removeEventListener('pointerdown', handlePointerDown)
        window.removeEventListener('keydown', handleEscape)
      }
    },
    [contextMenu],
  )

  useEffect(
    function ensureTabsInitialized() {
      if (tabs.length === 0) {
        createTab(DEFAULT_TERMINAL_CWD)
        return
      }
      if (!activeTabId) {
        setActiveTab(tabs[0].id)
      }
    },
    [activeTabId, createTab, setActiveTab, tabs],
  )

  useEffect(
    function initializeVisibleTabs() {
      for (const tab of tabs) {
        ensureTerminalForTab(tab)
      }
    },
    [ensureTerminalForTab, tabs],
  )

  useEffect(
    function focusOnVisible() {
      if (mode === 'panel' && !panelVisible) return
      focusActiveTerminal()
    },
    [focusActiveTerminal, mode, panelVisible, activeTabId],
  )

  useEffect(
    function fitOnResize() {
      function handleResize() {
        for (const fitAddon of fitMapRef.current.values()) {
          fitAddon.fit()
        }
        const snapshot = useTerminalPanelStore.getState().tabs
        for (const tab of snapshot) {
          const terminal = terminalMapRef.current.get(tab.id)
          if (!terminal) continue
          void resizeSession(tab.id, terminal)
        }
      }

      const timeout = window.setTimeout(handleResize, 50)
      window.addEventListener('resize', handleResize)

      return function cleanup() {
        window.clearTimeout(timeout)
        window.removeEventListener('resize', handleResize)
      }
    },
    [resizeSession],
  )

  useEffect(function disposeOnUnmount() {
    return function cleanup() {
      for (const reader of readerMapRef.current.values()) {
        void reader.cancel().catch(function ignore() {
          return undefined
        })
      }
      readerMapRef.current.clear()
      for (const terminal of terminalMapRef.current.values()) {
        terminal.dispose()
      }
      terminalMapRef.current.clear()
      fitMapRef.current.clear()
      containerMapRef.current.clear()
      connectedRef.current.clear()
    }
  }, [])

  return (
    <div className="relative flex h-full min-h-0 flex-col bg-primary-50">
      {mode === 'fullscreen' ? (
        <div className="flex h-11 items-center border-b border-primary-300 bg-primary-100 px-2 text-sm font-medium">
          <Button size="sm" variant="ghost" className="mr-2" onClick={onBack}>
            <HugeiconsIcon icon={ArrowLeft01Icon} size={20} strokeWidth={1.5} />
            Back
          </Button>
          <div className="text-balance text-sm font-medium">Terminal</div>
          <div className="ml-auto flex items-center gap-1">
            <Button size="sm" variant="ghost" onClick={handleCreateTab}>
              <HugeiconsIcon icon={Add01Icon} size={20} strokeWidth={1.5} />
              New Tab
            </Button>
          </div>
        </div>
      ) : null}

      <div className="flex h-8 items-center border-b border-primary-300 bg-primary-100 px-1">
        <div className="flex min-w-0 flex-1 items-center overflow-x-auto">
          {tabs.map(function renderTab(tab) {
            // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
            const isActive = tab.id === activeTab?.id
            return (
              <button
                key={tab.id}
                type="button"
                onClick={function onClick() {
                  setActiveTab(tab.id)
                  window.setTimeout(function focusCurrent() {
                    terminalMapRef.current.get(tab.id)?.focus()
                  }, 0)
                }}
                onContextMenu={function onContextMenu(event) {
                  event.preventDefault()
                  setContextMenu({
                    tabId: tab.id,
                    x: event.clientX,
                    y: event.clientY,
                  })
                }}
                className={cn(
                  'group relative flex h-8 max-w-[220px] items-center gap-2 px-3 text-xs text-primary-700 transition-colors',
                  isActive
                    ? 'bg-primary-50 text-primary-900'
                    : 'hover:bg-primary-200/70',
                )}
              >
                <span
                  className={cn(
                    'size-2 rounded-full',
                    isActive || tab.status === 'active'
                      ? 'bg-emerald-400'
                      : 'bg-primary-500',
                  )}
                />
                <HugeiconsIcon
                  icon={ComputerTerminal01Icon}
                  size={20}
                  strokeWidth={1.5}
                  className="shrink-0"
                />
                <span className="truncate text-left tabular-nums">
                  {tab.title}
                </span>
                {tabs.length > 1 ? (
                  <span
                    role="button"
                    tabIndex={0}
                    onClick={function onClose(event) {
                      event.stopPropagation()
                      handleCloseTab(tab)
                    }}
                    onKeyDown={function onCloseByKeyboard(event) {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault()
                        handleCloseTab(tab)
                      }
                    }}
                    className="hidden rounded p-0.5 text-primary-600 hover:bg-primary-300 hover:text-primary-900 group-hover:inline-flex"
                  >
                    <HugeiconsIcon
                      icon={Cancel01Icon}
                      size={20}
                      strokeWidth={1.5}
                    />
                  </span>
                ) : null}
                <span
                  className={cn(
                    'pointer-events-none absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-[#ea580c] transition-opacity',
                    isActive ? 'opacity-100' : 'opacity-0',
                  )}
                />
              </button>
            )
          })}
        </div>

        <div className="flex items-center gap-0.5">
          <Button
            size="sm"
            variant="ghost"
            onClick={handleAnalyzeDebug}
            disabled={debugLoading}
            aria-label="Analyze terminal output"
          >
            🔍 Debug
          </Button>
          <Button
            size="icon-sm"
            variant="ghost"
            onClick={handleCreateTab}
            aria-label="New terminal tab"
          >
            <HugeiconsIcon icon={Add01Icon} size={20} strokeWidth={1.5} />
          </Button>
          {mode === 'panel' ? (
            <>
              <Button
                size="icon-sm"
                variant="ghost"
                onClick={onMinimizePanel}
                aria-label="Minimize terminal panel"
              >
                <HugeiconsIcon
                  icon={SidebarLeft01Icon}
                  size={20}
                  strokeWidth={1.5}
                />
              </Button>
              <Button
                size="icon-sm"
                variant="ghost"
                onClick={onMaximizePanel}
                aria-label="Maximize terminal panel"
              >
                <HugeiconsIcon
                  icon={ArrowRight01Icon}
                  size={20}
                  strokeWidth={1.5}
                />
              </Button>
              <Button
                size="icon-sm"
                variant="ghost"
                onClick={handleClosePanel}
                aria-label="Close terminal panel"
              >
                <HugeiconsIcon
                  icon={Cancel01Icon}
                  size={20}
                  strokeWidth={1.5}
                />
              </Button>
            </>
          ) : null}
        </div>
      </div>

      <div
        className="relative flex-1 overflow-hidden bg-primary-50"
        style={{ backgroundColor: TERMINAL_BG }}
      >
        {tabs.map(function renderTerminal(tab) {
          // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
          const isActive = tab.id === activeTab?.id
          return (
            <div
              key={tab.id}
              className={cn('absolute inset-0', isActive ? 'block' : 'hidden')}
            >
              <div
                ref={function assignContainer(node) {
                  if (node) {
                    containerMapRef.current.set(tab.id, node)
                    ensureTerminalForTab(tab)
                    return
                  }
                  containerMapRef.current.delete(tab.id)
                }}
                className="h-full w-full bg-primary-50 font-mono text-primary-900"
                style={{ backgroundColor: TERMINAL_BG }}
              />
            </div>
          )
        })}
      </div>

      {showDebugPanel ? (
        <DebugPanel
          analysis={debugAnalysis}
          isLoading={debugLoading}
          onRunCommand={handleRunDebugCommand}
          onClose={handleCloseDebugPanel}
        />
      ) : null}

      {contextMenu ? (
        <div
          className="fixed z-50 min-w-36 rounded-md border border-primary-300 bg-primary-100 p-1 shadow-lg"
          style={{ top: contextMenu.y, left: contextMenu.x }}
          onClick={function stop(event) {
            event.stopPropagation()
          }}
        >
          <button
            type="button"
            className="flex w-full items-center rounded px-2 py-1.5 text-left text-xs text-primary-900 hover:bg-primary-200"
            onClick={function renameTabFromMenu() {
              const menuTab = tabs.find((tab) => tab.id === contextMenu.tabId)
              setContextMenu(null)
              if (!menuTab) return
              const nextName = window.prompt(
                'Rename terminal tab',
                menuTab.title,
              )
              if (!nextName) return
              renameTab(menuTab.id, nextName)
            }}
          >
            Rename
          </button>
          <button
            type="button"
            className="flex w-full items-center rounded px-2 py-1.5 text-left text-xs text-primary-900 hover:bg-primary-200"
            onClick={function closeTabFromMenu() {
              const menuTab = tabs.find((tab) => tab.id === contextMenu.tabId)
              setContextMenu(null)
              if (!menuTab) return
              handleCloseTab(menuTab)
            }}
          >
            Close
          </button>
        </div>
      ) : null}
    </div>
  )
}
