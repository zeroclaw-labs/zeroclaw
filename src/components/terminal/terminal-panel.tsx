import 'xterm/css/xterm.css'

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Terminal } from 'xterm'
import { FitAddon } from 'xterm-addon-fit'
import { WebLinksAddon } from 'xterm-addon-web-links'
import { SearchAddon } from 'xterm-addon-search'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  Add01Icon,
  Cancel01Icon,
  ComputerTerminal01Icon,
  Search01Icon,
} from '@hugeicons/core-free-icons'
import { GatewayControlPanel } from './terminal-panel-control'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

const PANEL_HEIGHT_KEY = 'terminal.panel.height'
const PANEL_OPEN_KEY = 'terminal.panel.open'
const TABS_KEY = 'terminal.tabs'
const ACTIVE_TAB_KEY = 'terminal.active'

const DEFAULT_HEIGHT = 360
const MIN_HEIGHT = 300
const MAX_HEIGHT = 480
const DEFAULT_CWD = '~/.openclaw/workspace'

type TerminalTabState = {
  id: string
  title: string
  sessionId?: string
  log?: string
}

type TerminalPanelProps = {
  isMobile?: boolean
}

export function TerminalPanel({ isMobile }: TerminalPanelProps) {
  const [isOpen, setIsOpen] = useState(() => {
    const stored = window.localStorage.getItem(PANEL_OPEN_KEY)
    return stored ? stored === 'true' : false
  })
  const [height, setHeight] = useState(() => {
    const stored = window.localStorage.getItem(PANEL_HEIGHT_KEY)
    const parsed = stored ? Number(stored) : DEFAULT_HEIGHT
    return Number.isFinite(parsed) ? parsed : DEFAULT_HEIGHT
  })
  const [tabs, setTabs] = useState<Array<TerminalTabState>>(() => {
    const stored = window.localStorage.getItem(TABS_KEY)
    if (!stored) {
      return [{ id: crypto.randomUUID(), title: 'Terminal 1' }]
    }
    try {
      const parsed = JSON.parse(stored) as Array<TerminalTabState>
      return parsed.length
        ? parsed
        : [{ id: crypto.randomUUID(), title: 'Terminal 1' }]
    } catch {
      return [{ id: crypto.randomUUID(), title: 'Terminal 1' }]
    }
  })
  const [activeTabId, setActiveTabId] = useState(() => {
    const stored = window.localStorage.getItem(ACTIVE_TAB_KEY)
    return stored || tabs[0]?.id
  })

  const resizingRef = useRef(false)
  const panelRef = useRef<HTMLDivElement | null>(null)
  const terminalMap = useRef(new Map<string, Terminal>())
  const fitMap = useRef(new Map<string, FitAddon>())
  const searchMap = useRef(new Map<string, SearchAddon>())
  const logBufferRef = useRef(new Map<string, string>())
  const logSaveTimers = useRef(new Map<string, number>())

  useEffect(() => {
    window.localStorage.setItem(PANEL_OPEN_KEY, String(isOpen))
  }, [isOpen])

  useEffect(() => {
    const clamped = Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, height))
    window.localStorage.setItem(PANEL_HEIGHT_KEY, String(clamped))
  }, [height])

  useEffect(() => {
    window.localStorage.setItem(TABS_KEY, JSON.stringify(tabs))
  }, [tabs])

  useEffect(() => {
    if (activeTabId) {
      window.localStorage.setItem(ACTIVE_TAB_KEY, activeTabId)
    }
  }, [activeTabId])

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.id === activeTabId) ?? tabs[0],
    [tabs, activeTabId],
  )

  const handleAddTab = useCallback(() => {
    const newTab: TerminalTabState = {
      id: crypto.randomUUID(),
      title: `Terminal ${tabs.length + 1}`,
    }
    setTabs((prev) => [...prev, newTab])
    setActiveTabId(newTab.id)
  }, [tabs.length])

  const handleCloseTab = useCallback(
    async (tabId: string) => {
      const tab = tabs.find((item) => item.id === tabId)
      if (tab?.sessionId) {
        await fetch('/api/terminal-close', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ sessionId: tab.sessionId }),
        }).catch(() => undefined)
      }
      setTabs((prev) => prev.filter((item) => item.id !== tabId))
      if (activeTabId === tabId) {
        const remaining = tabs.filter((item) => item.id !== tabId)
        setActiveTabId(remaining[0]?.id)
      }
      const term = terminalMap.current.get(tabId)
      term?.dispose()
      terminalMap.current.delete(tabId)
      fitMap.current.delete(tabId)
      searchMap.current.delete(tabId)
      logBufferRef.current.delete(tabId)
    },
    [activeTabId, tabs],
  )

  const handleToggleOpen = useCallback(() => {
    setIsOpen((prev) => !prev)
  }, [])

  const handleResizeStart = useCallback(
    (event: React.MouseEvent) => {
      if (!panelRef.current) return
      resizingRef.current = true
      const startY = event.clientY
      const startHeight = height

      const handleMove = (moveEvent: MouseEvent) => {
        if (!resizingRef.current) return
        const delta = startY - moveEvent.clientY
        const nextHeight = Math.min(
          MAX_HEIGHT,
          Math.max(MIN_HEIGHT, startHeight + delta),
        )
        setHeight(nextHeight)
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
        const fit = fitMap.current.get(activeTab?.id ?? '')
        fit?.fit()
      }

      const handleUp = () => {
        resizingRef.current = false
        window.removeEventListener('mousemove', handleMove)
        window.removeEventListener('mouseup', handleUp)
      }

      window.addEventListener('mousemove', handleMove)
      window.addEventListener('mouseup', handleUp)
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
    },
    [activeTab?.id, height],
  )

  const handleSendInput = useCallback(
    async (tabId: string, data: string) => {
      const tab = tabs.find((item) => item.id === tabId)
      if (!tab?.sessionId) return
      await fetch('/api/terminal-input', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ sessionId: tab.sessionId, data }),
      }).catch(() => undefined)
    },
    [tabs],
  )

  const initializeTerminal = useCallback(
    (tabId: string, container: HTMLDivElement | null) => {
      if (!container) return
      if (terminalMap.current.has(tabId)) return

      const terminal = new Terminal({
        theme: {
          background: '#0b0f1a',
        },
        cursorBlink: true,
        fontSize: 13,
        fontFamily: 'Menlo, Monaco, Consolas, "Courier New", monospace',
      })
      const fitAddon = new FitAddon()
      const webLinks = new WebLinksAddon()
      const searchAddon = new SearchAddon()

      terminal.loadAddon(fitAddon)
      terminal.loadAddon(webLinks)
      terminal.loadAddon(searchAddon)
      terminal.open(container)
      fitAddon.fit()

      const storedTab = tabs.find((tab) => tab.id === tabId)
      if (storedTab?.log) {
        terminal.write(storedTab.log)
      }

      terminal.onData((data) => {
        void handleSendInput(tabId, data)
      })

      terminalMap.current.set(tabId, terminal)
      fitMap.current.set(tabId, fitAddon)
      searchMap.current.set(tabId, searchAddon)
    },
    [handleSendInput, tabs],
  )

  const connectSession = useCallback(async (tabId: string) => {
    const terminal = terminalMap.current.get(tabId)
    if (!terminal) return
    const existing = tabs.find((tab) => tab.id === tabId)
    if (existing?.sessionId) return

    const response = await fetch('/api/terminal-stream', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        // Let the server pick the shell from $SHELL
        cwd: DEFAULT_CWD,
      }),
    })

    if (!response.ok || !response.body) {
      terminal.writeln('\r\n[terminal] failed to connect\r\n')
      return
    }

    const reader = response.body.getReader()
    const decoder = new TextDecoder()
    let buffer = ''
    let sessionId: string | undefined

    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })
      const events = buffer.split('\n\n')
      buffer = events.pop() ?? ''

      for (const eventBlock of events) {
        if (!eventBlock.trim()) continue
        const lines = eventBlock.split('\n')
        let currentEvent = ''
        let currentData = ''
        for (const line of lines) {
          if (line.startsWith('event: ')) {
            currentEvent = line.slice(7).trim()
          } else if (line.startsWith('data: ')) {
            currentData += line.slice(6)
          } else if (line.startsWith('data:')) {
            currentData += line.slice(5)
          }
        }
        if (!currentEvent || !currentData) continue
        try {
          const payload = JSON.parse(currentData)
          if (currentEvent === 'session') {
            sessionId = payload.sessionId
            setTabs((prev) =>
              prev.map((tab) =>
                tab.id === tabId ? { ...tab, sessionId } : tab,
              ),
            )
            continue
          }
          if (currentEvent === 'event') {
            const textChunk =
              payload?.payload?.data ??
              payload?.payload?.text ??
              payload?.payload?.chunk ??
              payload?.payload?.output
            if (typeof textChunk === 'string') {
              terminal.write(textChunk)
              const currentLog = logBufferRef.current.get(tabId) ?? ''
              const nextLog = `${currentLog}${textChunk}`
              logBufferRef.current.set(tabId, nextLog)
              const existingTimer = logSaveTimers.current.get(tabId)
              if (existingTimer) window.clearTimeout(existingTimer)
              const timer = window.setTimeout(() => {
                setTabs((prev) =>
                  prev.map((tab) =>
                    tab.id === tabId ? { ...tab, log: nextLog } : tab,
                  ),
                )
              }, 500)
              logSaveTimers.current.set(tabId, timer)
            }
          }
        } catch {
          // ignore
        }
      }
    }

    if (sessionId) {
      await fetch('/api/terminal-close', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ sessionId }),
      }).catch(() => undefined)
    }
  }, [])

  const handleSearch = useCallback((tabId: string, query: string) => {
    const addon = searchMap.current.get(tabId)
    if (!addon) return
    addon.findNext(query)
  }, [])

  useEffect(() => {
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
    if (!activeTab?.sessionId) return
    const term = terminalMap.current.get(activeTab.id)
    if (!term) return
    void fetch('/api/terminal-resize', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        sessionId: activeTab.sessionId,
        cols: term.cols,
        rows: term.rows,
      }),
    })
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
  }, [activeTab?.id, activeTab?.sessionId, height])

  if (isMobile) return null

  return (
    <div className="flex flex-col bg-surface border-t border-primary-200">
      <div className="flex items-center justify-between px-3 py-2">
        <div className="flex items-center gap-2 text-sm font-medium">
          <HugeiconsIcon
            icon={ComputerTerminal01Icon}
            size={18}
            strokeWidth={1.4}
          />
          Terminal
        </div>
        <Button
          size="sm"
          variant="ghost"
          onClick={handleToggleOpen}
          className="text-xs"
        >
          {isOpen ? 'Hide' : 'Show'}
        </Button>
      </div>

      {isOpen ? (
        <div
          ref={panelRef}
          className="relative border-t border-primary-200"
          style={{ height }}
        >
          <div
            className="absolute top-0 left-0 right-0 h-2 cursor-row-resize"
            onMouseDown={handleResizeStart}
          />

          <div className="flex h-full flex-col">
            <GatewayControlPanel />

            <div className="flex items-center gap-2 border-b border-primary-200 px-3 py-2">
              <div className="flex items-center gap-2 overflow-x-auto">
                {tabs.map((tab) => (
                  <button
                    key={tab.id}
                    className={cn(
                      'flex items-center gap-2 rounded-full border px-3 py-1 text-xs',
                      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
                      tab.id === activeTab?.id
                        ? 'border-primary-400 bg-primary-100 text-primary-900'
                        : 'border-primary-200 text-primary-700',
                    )}
                    onClick={() => setActiveTabId(tab.id)}
                  >
                    {tab.title}
                    {tabs.length > 1 ? (
                      <span
                        onClick={(event) => {
                          event.stopPropagation()
                          void handleCloseTab(tab.id)
                        }}
                        className="text-primary-500 hover:text-primary-900"
                      >
                        <HugeiconsIcon icon={Cancel01Icon} size={12} />
                      </span>
                    ) : null}
                  </button>
                ))}
              </div>
              <Button
                size="icon-sm"
                variant="ghost"
                onClick={handleAddTab}
                className="ml-auto"
              >
                <HugeiconsIcon icon={Add01Icon} size={14} strokeWidth={1.4} />
              </Button>
            </div>

            <div className="flex items-center gap-2 border-b border-primary-200 px-3 py-2">
              <div className="flex items-center gap-2 text-xs text-primary-500">
                <HugeiconsIcon icon={Search01Icon} size={14} />
                <input
                  className="rounded border border-primary-200 bg-transparent px-2 py-1 text-xs focus:outline-none"
                  placeholder="Search output"
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') {
                      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
                      handleSearch(
                        activeTab?.id ?? '',
                        event.currentTarget.value,
                      )
                    }
                  }}
                />
              </div>
              <div className="ml-auto text-xs text-primary-500">
                cwd: {DEFAULT_CWD}
              </div>
            </div>

            <div className="flex-1 overflow-hidden">
              {tabs.map((tab) => (
                <TerminalView
                  key={tab.id}
                  // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
                  isActive={tab.id === activeTab?.id}
                  onConnect={() => connectSession(tab.id)}
                  onInput={(data) => handleSendInput(tab.id, data)}
                  onReady={(container) => initializeTerminal(tab.id, container)}
                />
              ))}
            </div>
          </div>
        </div>
      ) : null}
    </div>
  )
}

type TerminalViewProps = {
  isActive: boolean
  onReady: (container: HTMLDivElement | null) => void
  onConnect: () => void
  onInput: (data: string) => void
}

function TerminalView({
  isActive,
  onReady,
  onConnect,
  onInput,
}: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    onReady(containerRef.current)
    onConnect()
  }, [onConnect, onReady])

  useEffect(() => {
    if (!containerRef.current) return
    const terminal = containerRef.current.querySelector('.xterm')
    if (!terminal) return
    terminal.classList.toggle('hidden', !isActive)
  }, [isActive])

  useEffect(() => {
    if (!containerRef.current) return
    const term = containerRef.current.querySelector('.xterm')
    if (!term) return
  }, [])

  return (
    <div
      ref={containerRef}
      className={cn(
        'h-full w-full bg-[#0b0f1a] text-primary-100',
        isActive ? 'block' : 'hidden',
      )}
      onKeyDown={(event) => {
        if (event.key === 'c' && (event.metaKey || event.ctrlKey)) {
          document.execCommand('copy')
        }
        if (event.key === 'v' && (event.metaKey || event.ctrlKey)) {
          navigator.clipboard.readText().then((text) => {
            if (text) onInput(text)
          })
        }
      }}
    />
  )
}
