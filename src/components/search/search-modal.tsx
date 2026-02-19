import { HugeiconsIcon } from '@hugeicons/react'
import {
  AiBrain01Icon,
  Chat01Icon,
  Clock01Icon,
  CommandIcon,
  File01Icon,
  FlashIcon,
  LanguageSkillIcon,
  ListViewIcon,
} from '@hugeicons/core-free-icons'
import { AnimatePresence, motion } from 'motion/react'
import { useNavigate } from '@tanstack/react-router'
import { useDeferredValue, useEffect, useMemo, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { SearchInput } from './search-input'
import { QuickActions } from './quick-actions'
import { SearchResults } from './search-results'
import type { QuickAction } from './quick-actions'
import type { SearchResultItemData } from './search-results'
import type { SearchScope } from '@/hooks/use-search-modal'
import {
  SEARCH_MODAL_EVENTS,
  emitSearchModalEvent,
  useSearchModal,
} from '@/hooks/use-search-modal'
import { filterResults, useSearchData } from '@/hooks/use-search-data'
import { cn } from '@/lib/utils'

const SCOPE_TABS: Array<{ value: SearchScope; label: string }> = [
  { value: 'all', label: 'All' },
  { value: 'chats', label: '💬 Chats' },
  { value: 'files', label: '📁 Files' },
  { value: 'agents', label: '🤖 Agents' },
  { value: 'skills', label: '🛠️ Skills' },
  { value: 'actions', label: '⚡ Actions' },
]

const RECENT_SEARCHES = [
  'streaming fixes',
  'session timeout',
  'agent memory',
  'usage alerts',
]

const RESULT_LIMITS = {
  chats: 24,
  files: 40,
  agents: 24,
  skills: 24,
  actions: 10,
  total: 80,
} as const

function includesQuery(value: string, query: string) {
  return value.toLowerCase().includes(query.toLowerCase())
}

function getFileBadge(ext: string) {
  return ext.toUpperCase()
}

export function SearchModal() {
  const navigate = useNavigate()
  const inputRef = useRef<HTMLInputElement | null>(null)

  const isOpen = useSearchModal((state) => state.isOpen)
  const query = useSearchModal((state) => state.query)
  const scope = useSearchModal((state) => state.scope)
  const closeModal = useSearchModal((state) => state.closeModal)
  const toggleModal = useSearchModal((state) => state.toggleModal)
  const setQuery = useSearchModal((state) => state.setQuery)
  const clearQuery = useSearchModal((state) => state.clearQuery)
  const setScope = useSearchModal((state) => state.setScope)

  const [debouncedQuery, setDebouncedQuery] = useState('')
  const [selectedIndex, setSelectedIndex] = useState(0)
  const deferredQuery = useDeferredValue(debouncedQuery)

  // Real data (Phase 3.2)
  const { sessions, files, skills, activity } = useSearchData(scope)
  const searchableFiles = useMemo(
    () => files.filter((entry) => entry.type === 'file'),
    [files],
  )

  const quickActions = useMemo<Array<QuickAction>>(
    () => [
      {
        id: 'qa-new-chat',
        emoji: '💬',
        label: 'New Chat',
        description: 'Start a new conversation session',
        onSelect: () => {
          closeModal()
          navigate({ to: '/new' })
        },
      },
      {
        id: 'qa-dashboard',
        emoji: '🏠',
        label: 'Dashboard',
        description: 'Open the dashboard workspace',
        onSelect: () => {
          closeModal()
          navigate({ to: '/dashboard' })
        },
      },
      {
        id: 'qa-skills',
        emoji: '🛠️',
        label: 'Skills',
        description: 'Manage installed and available skills',
        onSelect: () => {
          closeModal()
          navigate({ to: '/skills' })
        },
      },
      {
        id: 'qa-terminal',
        emoji: '🖥️',
        label: 'Terminal',
        description: 'Jump into terminal view',
        onSelect: () => {
          closeModal()
          navigate({ to: '/terminal' })
        },
      },
      {
        id: 'qa-logs',
        emoji: '📄',
        label: 'Logs',
        description: 'Open the real-time activity log viewer',
        onSelect: () => {
          closeModal()
          navigate({ to: '/logs' })
        },
      },
      {
        id: 'qa-cron',
        emoji: '⏰',
        label: 'Cron',
        description: 'Open cron job manager and run history',
        onSelect: () => {
          closeModal()
          navigate({ to: '/cron' })
        },
      },
      {
        id: 'qa-files',
        emoji: '📁',
        label: 'Files',
        description: 'Toggle the file explorer sidebar',
        onSelect: () => {
          closeModal()
          emitSearchModalEvent(SEARCH_MODAL_EVENTS.TOGGLE_FILE_EXPLORER)
        },
      },
      {
        id: 'qa-settings',
        emoji: '⚙️',
        label: 'Settings',
        description: 'Open the settings workspace',
        onSelect: () => {
          closeModal()
          navigate({ to: '/settings' })
        },
      },
      {
        id: 'qa-spawn-agent',
        emoji: '🤖',
        label: 'Spawn Agent',
        description: 'Placeholder action for agent orchestration',
        onSelect: () => {
          closeModal()
          window.alert('Spawn Agent is coming soon.')
        },
      },
      {
        id: 'qa-usage',
        emoji: '📊',
        label: 'Usage',
        description: 'Open usage meter details',
        onSelect: () => {
          closeModal()
          emitSearchModalEvent(SEARCH_MODAL_EVENTS.OPEN_USAGE)
        },
      },
    ],
    [closeModal, navigate],
  )

  useEffect(() => {
    const timeout = window.setTimeout(() => {
      setDebouncedQuery(query)
    }, 200)

    return () => {
      window.clearTimeout(timeout)
    }
  }, [query])

  useEffect(() => {
    if (!isOpen) return
    const frameId = window.requestAnimationFrame(function focusSearchInput() {
      inputRef.current?.focus()
      inputRef.current?.select()
    })
    return function cleanupFocusSearchInput() {
      window.cancelAnimationFrame(frameId)
    }
  }, [isOpen])

  const resultItems = useMemo<Array<SearchResultItemData>>(() => {
    const normalized = deferredQuery.trim()
    if (!normalized) return []

    // Real sessions data
    const chats = filterResults(
      sessions,
      normalized,
      ['friendlyId', 'key', 'title'],
      RESULT_LIMITS.chats,
    ).map<SearchResultItemData>((entry) => ({
      id: entry.id,
      scope: 'chats',
      icon: <HugeiconsIcon icon={Chat01Icon} size={20} strokeWidth={1.5} />,
      title: entry.title || entry.friendlyId,
      snippet: entry.preview || `Session: ${entry.key}`,
      meta: entry.updatedAt
        ? new Date(entry.updatedAt).toLocaleTimeString()
        : '',
      onSelect: () => {
        closeModal()
        navigate({
          to: '/chat/$sessionKey',
          params: { sessionKey: entry.key },
        })
      },
    }))

    // Real files data
    const fileResults = filterResults(
      searchableFiles,
      normalized,
      ['path', 'name'],
      RESULT_LIMITS.files,
    ).map<SearchResultItemData>((entry) => ({
      id: entry.id,
      scope: 'files',
      icon: <HugeiconsIcon icon={File01Icon} size={20} strokeWidth={1.5} />,
      title: entry.name,
      snippet: entry.path,
      meta: entry.type,
      badge: getFileBadge(entry.name.split('.').pop() || ''),
      onSelect: () => {
        closeModal()
        navigate({ to: '/files', search: { open: entry.path } })
      },
    }))

    // Real activity data
    const activityResults = filterResults(
      activity,
      normalized,
      ['title', 'detail', 'source'],
      RESULT_LIMITS.agents,
    ).map<SearchResultItemData>((entry) => ({
      id: entry.id,
      scope: 'agents',
      icon: <HugeiconsIcon icon={AiBrain01Icon} size={20} strokeWidth={1.5} />,
      title: entry.title,
      snippet: entry.detail || '',
      meta: new Date(entry.timestamp).toLocaleTimeString(),
      badge: entry.level,
      onSelect: () => {
        closeModal()
        navigate({ to: '/activity' })
      },
    }))

    // Real skills data (static)
    const skillResults = filterResults(
      skills,
      normalized,
      ['name', 'description'],
      RESULT_LIMITS.skills,
    ).map<SearchResultItemData>((entry) => ({
      id: entry.id,
      scope: 'skills',
      icon: (
        <HugeiconsIcon icon={LanguageSkillIcon} size={20} strokeWidth={1.5} />
      ),
      title: entry.name,
      snippet: entry.description,
      meta: entry.installed ? 'Installed' : 'Available',
      badge: entry.installed ? 'Installed' : 'Not Installed',
      onSelect: () => {
        closeModal()
        navigate({ to: '/skills' })
      },
    }))

    const actions: Array<SearchResultItemData> = []
    for (const entry of quickActions) {
      if (!includesQuery(`${entry.label} ${entry.description}`, normalized))
        continue
      actions.push({
        id: entry.id,
        scope: 'actions',
        icon: (
          <HugeiconsIcon
            icon={
              entry.id === 'qa-logs'
                ? ListViewIcon
                : entry.id === 'qa-cron'
                  ? Clock01Icon
                  : FlashIcon
            }
            size={20}
            strokeWidth={1.5}
          />
        ),
        title: entry.label,
        snippet: entry.description,
        meta: 'Action',
        onSelect: entry.onSelect,
      })
      if (actions.length >= RESULT_LIMITS.actions) break
    }

    if (scope === 'chats') return chats
    if (scope === 'files') return fileResults
    if (scope === 'agents') return activityResults
    if (scope === 'skills') return skillResults
    if (scope === 'actions') return actions

    return [
      ...chats,
      ...fileResults,
      ...activityResults,
      ...skillResults,
      ...actions,
    ].slice(0, RESULT_LIMITS.total)
  }, [
    activity,
    closeModal,
    deferredQuery,
    navigate,
    quickActions,
    scope,
    searchableFiles,
    sessions,
    skills,
  ])

  useEffect(() => {
    setSelectedIndex(0)
  }, [deferredQuery, scope])

  useEffect(() => {
    if (selectedIndex < resultItems.length) return
    setSelectedIndex(Math.max(0, resultItems.length - 1))
  }, [resultItems.length, selectedIndex])

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.isComposing) return

      const hasCommand = event.metaKey || event.ctrlKey
      if (hasCommand && event.key.toLowerCase() === 'k') {
        event.preventDefault()
        toggleModal()
        return
      }

      if (!isOpen) return

      if (event.key === 'Escape') {
        event.preventDefault()
        closeModal()
        return
      }

      if (event.key === 'Tab') {
        event.preventDefault()
        const direction = event.shiftKey ? -1 : 1
        const currentIndex = SCOPE_TABS.findIndex(
          (item) => item.value === scope,
        )
        const nextIndex =
          (currentIndex + direction + SCOPE_TABS.length) % SCOPE_TABS.length
        setScope(SCOPE_TABS[nextIndex].value)
        return
      }

      if (event.key === 'ArrowDown') {
        event.preventDefault()
        if (resultItems.length === 0) return
        setSelectedIndex((prev) => (prev + 1) % resultItems.length)
        return
      }

      if (event.key === 'ArrowUp') {
        event.preventDefault()
        if (resultItems.length === 0) return
        setSelectedIndex(
          (prev) => (prev - 1 + resultItems.length) % resultItems.length,
        )
        return
      }

      if (event.key === 'Enter') {
        if (resultItems.length === 0) return
        event.preventDefault()
        resultItems[selectedIndex]?.onSelect()
        closeModal()
        return
      }

      if (/^[1-9]$/.test(event.key)) {
        const index = Number(event.key) - 1
        const target = resultItems[index]
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
        if (!target) return
        event.preventDefault()
        setSelectedIndex(index)
        target.onSelect()
        closeModal()
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => {
      window.removeEventListener('keydown', handleKeyDown)
    }
  }, [
    closeModal,
    isOpen,
    resultItems,
    scope,
    selectedIndex,
    setScope,
    toggleModal,
  ])

  if (typeof document === 'undefined') return null

  return createPortal(
    <AnimatePresence>
      {isOpen ? (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.16, ease: 'easeOut' }}
          className="fixed inset-0 z-50 flex items-start justify-center bg-ink/45 px-4 pt-[9vh] backdrop-blur-md"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) {
              closeModal()
            }
          }}
        >
          <motion.div
            initial={{ opacity: 0, scale: 0.96, y: 12 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.98, y: 8 }}
            transition={{ duration: 0.18, ease: 'easeOut' }}
            className={cn(
              'w-[min(640px,92vw)] max-h-[480px] rounded-xl border border-primary-200 bg-primary-50/90 shadow-2xl',
              'overflow-hidden backdrop-blur-xl',
            )}
            onMouseDown={(event) => {
              event.stopPropagation()
            }}
          >
            <div className="p-3">
              <SearchInput
                value={query}
                onValueChange={setQuery}
                onClear={clearQuery}
                inputRef={inputRef}
              />

              <div className="mt-2 flex flex-wrap gap-1.5">
                {SCOPE_TABS.map((tab) => (
                  <button
                    key={tab.value}
                    type="button"
                    onClick={() => setScope(tab.value)}
                    className={cn(
                      'rounded-md border px-2.5 py-1 text-xs font-medium transition-colors',
                      scope === tab.value
                        ? 'border-accent-500/40 bg-accent-500/20 text-accent-400'
                        : 'border-primary-200 bg-primary-100/60 text-primary-500 hover:bg-primary-100',
                    )}
                  >
                    <span>{tab.label}</span>
                  </button>
                ))}
              </div>
            </div>

            <div className="max-h-[360px] overflow-y-auto border-t border-primary-200 p-3">
              {query.trim().length === 0 ? (
                <QuickActions
                  recentSearches={RECENT_SEARCHES}
                  actions={quickActions}
                  onSelectRecent={(value) => {
                    setQuery(value)
                  }}
                />
              ) : (
                <SearchResults
                  query={debouncedQuery.trim() || query.trim()}
                  results={resultItems}
                  selectedIndex={selectedIndex}
                  onHoverIndex={(index) => setSelectedIndex(index)}
                  onSelectIndex={(index) => {
                    const item = resultItems[index]
                    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
                    if (!item) return
                    item.onSelect()
                    closeModal()
                  }}
                />
              )}
            </div>

            <div className="flex items-center justify-between border-t border-primary-200 bg-primary-50/80 px-3 py-2 text-[11px] text-primary-500">
              <div className="flex items-center gap-1.5">
                <HugeiconsIcon icon={CommandIcon} size={20} strokeWidth={1.5} />
                <span>Arrow keys to navigate</span>
              </div>
              <div className="flex items-center gap-2 tabular-nums">
                <span>Tab scope</span>
                <span>1-9 jump</span>
                <span>↵ open</span>
                <span>Esc close</span>
              </div>
            </div>
          </motion.div>
        </motion.div>
      ) : null}
    </AnimatePresence>,
    document.body,
  )
}
