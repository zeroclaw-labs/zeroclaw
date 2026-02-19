import {
  Activity01Icon,
  WifiDisconnected02Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useActivityEvents } from './use-activity-events'
import { ActivityEventRow } from './components/activity-event-row'
import type { UIEvent } from 'react'
import type { ActivityEvent } from '@/types/activity-event'
import { EmptyState } from '@/components/empty-state'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { cn } from '@/lib/utils'

type ActivityLevelFilter = 'all' | 'error' | 'warn' | 'info' | 'debug'

type ActivityLevelFilterOption = {
  id: ActivityLevelFilter
  label: string
}

type ActivitySourceOption = {
  label: string
  value: string
}

const ALL_SOURCE_FILTER_VALUE = 'all'

const ACTIVITY_LEVEL_FILTERS: Array<ActivityLevelFilterOption> = [
  { id: 'all', label: 'All' },
  { id: 'error', label: 'Errors' },
  { id: 'warn', label: 'Warnings' },
  { id: 'info', label: 'Info' },
  { id: 'debug', label: 'Debug' },
]

function normalizeSource(source: ActivityEvent['source']): string {
  if (!source) return ''
  return source.trim().toLowerCase()
}

function matchesLevelFilter(
  event: ActivityEvent,
  selectedLevel: ActivityLevelFilter,
): boolean {
  if (selectedLevel === 'all') return true
  return event.level === selectedLevel
}

function matchesSourceFilter(
  event: ActivityEvent,
  selectedSource: string,
): boolean {
  if (selectedSource === ALL_SOURCE_FILTER_VALUE) return true
  return normalizeSource(event.source) === selectedSource
}

function matchesSearchFilter(
  event: ActivityEvent,
  normalizedSearchText: string,
): boolean {
  if (!normalizedSearchText) return true

  const content = `${event.title}\n${event.detail || ''}`.toLowerCase()
  return content.includes(normalizedSearchText)
}

function formatEventCountLabel(
  filteredCount: number,
  totalCount: number,
  isFiltered: boolean,
): string {
  if (!isFiltered) return `${totalCount} events`
  return `${filteredCount} of ${totalCount} events`
}

export function ActivityScreen() {
  const [searchText, setSearchText] = useState('')
  const [selectedLevel, setSelectedLevel] = useState<ActivityLevelFilter>('all')
  const [selectedSource, setSelectedSource] = useState(ALL_SOURCE_FILTER_VALUE)
  const [isAutoScrollPinned, setIsAutoScrollPinned] = useState(true)
  const [canScrollToTop, setCanScrollToTop] = useState(false)
  const viewportRef = useRef<HTMLDivElement | null>(null)

  const { events, isConnected, isLoading } = useActivityEvents({
    initialCount: 100,
    maxEvents: 200,
  })

  const normalizedSearchText = useMemo(
    function memoNormalizedSearchText() {
      return searchText.trim().toLowerCase()
    },
    [searchText],
  )

  const sourceOptions = useMemo(
    function memoSourceOptions() {
      const sourceMap = new Map<string, string>()

      for (const event of events) {
        const normalizedSource = normalizeSource(event.source)
        if (!normalizedSource) continue
        if (sourceMap.has(normalizedSource)) continue

        sourceMap.set(
          normalizedSource,
          event.source?.trim() || normalizedSource,
        )
      }

      return Array.from(sourceMap.entries())
        .sort(function sortByLabel([leftLabel], [rightLabel]) {
          return leftLabel.localeCompare(rightLabel)
        })
        .map(function mapSourceOption([value, label]): ActivitySourceOption {
          return { label, value }
        })
    },
    [events],
  )

  const filteredEvents = useMemo(
    function memoFilteredEvents() {
      return events.filter(function keepMatchingEvent(event) {
        if (!matchesLevelFilter(event, selectedLevel)) return false
        if (!matchesSourceFilter(event, selectedSource)) return false
        return matchesSearchFilter(event, normalizedSearchText)
      })
    },
    [events, normalizedSearchText, selectedLevel, selectedSource],
  )

  const isFiltered = useMemo(
    function memoIsFiltered() {
      return (
        normalizedSearchText.length > 0 ||
        selectedLevel !== 'all' ||
        selectedSource !== ALL_SOURCE_FILTER_VALUE
      )
    },
    [normalizedSearchText, selectedLevel, selectedSource],
  )

  const eventCountLabel = useMemo(
    function memoEventCountLabel() {
      return formatEventCountLabel(
        filteredEvents.length,
        events.length,
        isFiltered,
      )
    },
    [events.length, filteredEvents.length, isFiltered],
  )

  const scrollToBottom = useCallback(function scrollToBottom() {
    const viewport = viewportRef.current
    if (!viewport) return
    viewport.scrollTop = viewport.scrollHeight
  }, [])

  function scrollToTop() {
    const viewport = viewportRef.current
    if (!viewport) return
    viewport.scrollTop = 0
  }

  function handleToggleAutoScroll() {
    setIsAutoScrollPinned(function togglePinnedState(currentValue) {
      const nextValue = !currentValue
      if (nextValue) {
        window.requestAnimationFrame(function scrollAfterPin() {
          scrollToBottom()
        })
      }
      return nextValue
    })
  }

  function handleViewportScroll(event: UIEvent<HTMLDivElement>) {
    setCanScrollToTop(event.currentTarget.scrollTop > 24)
  }

  useEffect(
    function keepViewportPinnedToBottom() {
      if (!isAutoScrollPinned) return
      scrollToBottom()
    },
    [
      filteredEvents.length,
      isAutoScrollPinned,
      normalizedSearchText,
      scrollToBottom,
      selectedLevel,
      selectedSource,
    ],
  )

  useEffect(
    function ensureSelectedSourceIsValid() {
      if (selectedSource === ALL_SOURCE_FILTER_VALUE) return

      const sourceStillExists = sourceOptions.some(function hasSource(option) {
        return option.value === selectedSource
      })

      if (!sourceStillExists) {
        setSelectedSource(ALL_SOURCE_FILTER_VALUE)
      }
    },
    [selectedSource, sourceOptions],
  )

  return (
    <main className="h-full overflow-y-auto bg-surface px-4 pt-6 pb-24 text-primary-900 md:px-6 md:pt-8 md:pb-0">
      <div className="mx-auto flex w-full max-w-4xl flex-col">
        <header className="mb-3 flex flex-wrap items-center gap-2.5 md:mb-4">
          <HugeiconsIcon icon={Activity01Icon} size={20} strokeWidth={1.5} />
          <h1 className="text-xl font-medium text-primary-900 text-balance md:text-3xl">
            Activity Log
          </h1>
          <span className="inline-flex items-center rounded-full border border-primary-200 bg-primary-100/80 px-2 py-0.5 text-xs text-primary-700 tabular-nums">
            {eventCountLabel}
          </span>
          <span
            className={cn(
              'ml-auto inline-flex size-2.5 rounded-full',
              isConnected ? 'bg-emerald-500 animate-pulse' : 'bg-red-500',
            )}
            title={isConnected ? 'Live' : 'Disconnected'}
          />
        </header>

        {isLoading && events.length === 0 ? (
          <p className="text-sm text-primary-600 text-pretty tabular-nums">
            Loading events…
          </p>
        ) : !isConnected && events.length === 0 ? (
          <EmptyState
            icon={WifiDisconnected02Icon}
            title="Stream disconnected"
            description="Check your Gateway connection and refresh"
          />
        ) : events.length === 0 ? (
          <EmptyState
            icon={Activity01Icon}
            title="No events recorded"
            description="Activity will appear here as you use the app"
          />
        ) : (
          <section className="overflow-hidden rounded-xl border border-primary-200 bg-primary-50/80">
            <div
              ref={viewportRef}
              onScroll={handleViewportScroll}
              className="max-h-[38rem] overflow-y-auto"
            >
              <div className="sticky top-0 z-10 border-b border-primary-200 bg-primary-50/95 p-3 backdrop-blur-sm">
                <div className="flex flex-wrap items-center gap-1.5">
                  {ACTIVITY_LEVEL_FILTERS.map(
                    function renderLevelFilter(filterOption) {
                      const selected = selectedLevel === filterOption.id

                      return (
                        <Button
                          key={filterOption.id}
                          size="sm"
                          variant={selected ? 'default' : 'outline'}
                          className="h-7 px-2 text-xs tabular-nums"
                          onClick={function onSelectLevelFilter() {
                            setSelectedLevel(filterOption.id)
                          }}
                          aria-pressed={selected}
                        >
                          {filterOption.label}
                        </Button>
                      )
                    },
                  )}
                </div>

                <div className="mt-2 flex flex-wrap items-center gap-2">
                  <Input
                    type="search"
                    size="sm"
                    value={searchText}
                    onChange={function onSearchTextChange(event) {
                      setSearchText(event.target.value)
                    }}
                    placeholder="Search by title or detail"
                    className="w-full min-w-52 flex-1 tabular-nums"
                    aria-label="Search activity events"
                  />

                  <label
                    htmlFor="activity-source-filter"
                    className="text-xs text-primary-700 tabular-nums"
                  >
                    <span className="sr-only">Source filter</span>
                    <select
                      id="activity-source-filter"
                      value={selectedSource}
                      onChange={function onSourceChange(event) {
                        setSelectedSource(event.target.value)
                      }}
                      className="h-7 rounded-lg border border-primary-200 bg-primary-50 px-2 text-xs text-primary-900 tabular-nums outline-none focus-visible:ring-2 focus-visible:ring-primary-400"
                      aria-label="Filter by source"
                    >
                      <option value={ALL_SOURCE_FILTER_VALUE}>
                        All sources
                      </option>
                      {sourceOptions.map(function renderSourceOption(option) {
                        return (
                          <option key={option.value} value={option.value}>
                            {option.label}
                          </option>
                        )
                      })}
                    </select>
                  </label>

                  <Button
                    size="sm"
                    variant={isAutoScrollPinned ? 'default' : 'outline'}
                    className="h-7 px-2 text-xs tabular-nums"
                    onClick={handleToggleAutoScroll}
                    aria-pressed={isAutoScrollPinned}
                  >
                    {isAutoScrollPinned ? 'Auto-scroll on' : 'Auto-scroll off'}
                  </Button>

                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 px-2 text-xs tabular-nums"
                    disabled={!canScrollToTop}
                    onClick={scrollToTop}
                  >
                    Scroll to top
                  </Button>
                </div>
              </div>

              <div className="space-y-2 p-3">
                {filteredEvents.length === 0 ? (
                  <p className="rounded-lg border border-primary-200 bg-primary-100/60 px-3 py-4 text-sm text-primary-600 text-pretty">
                    No events match the current filters.
                  </p>
                ) : (
                  filteredEvents.map(function renderEvent(event) {
                    return <ActivityEventRow key={event.id} event={event} />
                  })
                )}
              </div>
            </div>
          </section>
        )}
      </div>
    </main>
  )
}
