import { useEffect, useMemo, useState } from 'react'
import type { ActivityEvent } from '@/types/activity-event'

type UseActivityEventsOptions = {
  initialCount: number
  maxEvents: number
}

type RecentEventsResponse = {
  events?: Array<unknown>
  connected?: unknown
}

const EVENT_TYPES: Array<ActivityEvent['type']> = [
  'gateway',
  'model',
  'usage',
  'cron',
  'tool',
  'error',
  'session',
]

const EVENT_LEVELS: Array<ActivityEvent['level']> = [
  'debug',
  'info',
  'warn',
  'error',
]

function toRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object') return null
  if (Array.isArray(value)) return null
  return value as Record<string, unknown>
}

function isEventType(value: unknown): value is ActivityEvent['type'] {
  return EVENT_TYPES.includes(value as ActivityEvent['type'])
}

function isEventLevel(value: unknown): value is ActivityEvent['level'] {
  return EVENT_LEVELS.includes(value as ActivityEvent['level'])
}

function normalizeActivityEvent(value: unknown): ActivityEvent | null {
  const record = toRecord(value)
  if (!record) return null

  const id = typeof record.id === 'string' ? record.id : ''
  const timestamp =
    typeof record.timestamp === 'number' && Number.isFinite(record.timestamp)
      ? record.timestamp
      : null
  const type = record.type
  const title = typeof record.title === 'string' ? record.title : ''
  const detail = typeof record.detail === 'string' ? record.detail : undefined
  const source = typeof record.source === 'string' ? record.source : undefined
  const level = record.level

  if (
    !id ||
    !timestamp ||
    !isEventType(type) ||
    !title ||
    !isEventLevel(level)
  ) {
    return null
  }

  return {
    id,
    timestamp,
    type,
    title,
    detail,
    level,
    source,
  }
}

function normalizeEvents(items: Array<unknown>): Array<ActivityEvent> {
  const normalized: Array<ActivityEvent> = []

  for (const item of items) {
    const event = normalizeActivityEvent(item)
    if (event) normalized.push(event)
  }

  return normalized.sort(function sortByTimestamp(a, b) {
    return a.timestamp - b.timestamp
  })
}

function mergeEvents(
  current: Array<ActivityEvent>,
  incoming: Array<ActivityEvent>,
  maxEvents: number,
): Array<ActivityEvent> {
  const byId = new Map<string, ActivityEvent>()

  for (const item of current) {
    byId.set(item.id, item)
  }

  for (const item of incoming) {
    byId.set(item.id, item)
  }

  const merged = Array.from(byId.values()).sort(function sortByTimestamp(a, b) {
    return a.timestamp - b.timestamp
  })

  if (merged.length <= maxEvents) return merged
  return merged.slice(merged.length - maxEvents)
}

function clampCount(value: number, fallback: number): number {
  if (!Number.isFinite(value)) return fallback
  return Math.max(1, Math.floor(value))
}

function parseSsePayload(payload: string): ActivityEvent | null {
  try {
    const parsed = JSON.parse(payload) as unknown
    return normalizeActivityEvent(parsed)
  } catch {
    return null
  }
}

export function useActivityEvents(options: UseActivityEventsOptions) {
  const [events, setEvents] = useState<Array<ActivityEvent>>([])
  const [isConnected, setIsConnected] = useState(false)
  const [isLoading, setIsLoading] = useState(true)

  const initialCount = useMemo(
    function getInitialCount() {
      return clampCount(options.initialCount, 20)
    },
    [options.initialCount],
  )

  const maxEvents = useMemo(
    function getMaxEvents() {
      return clampCount(options.maxEvents, 100)
    },
    [options.maxEvents],
  )

  useEffect(
    function subscribeToActivityStream() {
      let active = true
      const abortController = new AbortController()
      const eventSource = new EventSource('/api/events')

      function appendIncoming(incoming: Array<ActivityEvent>) {
        setEvents(function setMerged(current) {
          return mergeEvents(current, incoming, maxEvents)
        })
      }

      eventSource.onopen = function onOpen() {
        if (!active) return
        setIsConnected(true)
      }

      eventSource.onerror = function onError() {
        if (!active) return
        setIsConnected(false)
      }

      eventSource.addEventListener('activity', function onActivity(event) {
        if (!active) return
        if (!(event instanceof MessageEvent)) return

        const parsed = parseSsePayload(event.data)
        if (!parsed) return

        // Update connection status based on gateway connect/disconnect events
        if (parsed.type === 'gateway') {
          if (parsed.title === 'Gateway connected') {
            setIsConnected(true)
          } else if (parsed.title === 'Gateway disconnected') {
            setIsConnected(false)
          }
        }

        appendIncoming([parsed])
      })

      eventSource.addEventListener('ready', function onReady(event) {
        if (!active) return
        if (!(event instanceof MessageEvent)) return

        try {
          const payload = JSON.parse(event.data) as Record<string, unknown>
          if (typeof payload.connected === 'boolean') {
            // Only update to disconnected if EventSource itself is still open
            // (onopen already set connected=true if the SSE stream is healthy)
            if (payload.connected) {
              setIsConnected(true)
            }
            // Don't set false here — the SSE ready event fires before the gateway
            // WS fully connects, so it would override the onopen=true with false
          }
        } catch {
          // ignore malformed payloads
        }
      })

      async function loadRecentEvents() {
        try {
          const response = await fetch(
            `/api/events/recent?count=${initialCount}`,
            {
              signal: abortController.signal,
            },
          )

          if (!response.ok) {
            throw new Error('Unable to load activity events')
          }

          const payload = (await response.json()) as RecentEventsResponse
          if (!active) return

          const recentItems = Array.isArray(payload.events)
            ? payload.events
            : []
          appendIncoming(normalizeEvents(recentItems))

          if (typeof payload.connected === 'boolean') {
            setIsConnected(payload.connected)
          }
        } catch {
          if (!active) return
          setIsConnected(false)
        } finally {
          if (active) {
            setIsLoading(false)
          }
        }
      }

      void loadRecentEvents()

      return function cleanup() {
        active = false
        abortController.abort()
        eventSource.close()
      }
    },
    [initialCount, maxEvents],
  )

  return {
    events,
    isConnected,
    isLoading,
  }
}
