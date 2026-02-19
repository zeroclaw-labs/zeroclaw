import { useCallback, useEffect, useRef } from 'react'
import { useGatewayChatStore } from '../stores/gateway-chat-store'
import type { GatewayMessage } from '../screens/chat/types'

type UseGatewayChatStreamOptions = {
  /** Session key to filter events for (optional - receives all if not specified) */
  sessionKey?: string
  /** Whether the stream should be active */
  enabled?: boolean
  /** Callback when a user message arrives from an external channel */
  onUserMessage?: (message: GatewayMessage, source?: string) => void
  /** Callback when assistant streaming chunk arrives */
  onChunk?: (text: string, sessionKey: string) => void
  /** Callback when assistant thinking updates */
  onThinking?: (text: string, sessionKey: string) => void
  /** Callback when a generation completes */
  onDone?: (state: string, sessionKey: string) => void
}

export function useGatewayChatStream(
  options: UseGatewayChatStreamOptions = {},
) {
  const {
    enabled = true,
    onUserMessage,
    onChunk,
    onThinking,
    onDone,
  } = options

  const { connectionState, setConnectionState, processEvent, lastError } =
    useGatewayChatStore()

  const eventSourceRef = useRef<EventSource | null>(null)
  const reconnectTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const reconnectAttempts = useRef(0)
  const mountedRef = useRef(true)

  // Store callbacks in refs to avoid reconnecting when they change
  const onUserMessageRef = useRef(onUserMessage)
  const onChunkRef = useRef(onChunk)
  const onThinkingRef = useRef(onThinking)
  const onDoneRef = useRef(onDone)
  onUserMessageRef.current = onUserMessage
  onChunkRef.current = onChunk
  onThinkingRef.current = onThinking
  onDoneRef.current = onDone

  const connect = useCallback(() => {
    if (!enabled || !mountedRef.current) return

    // Clean up existing connection
    if (eventSourceRef.current) {
      eventSourceRef.current.close()
      eventSourceRef.current = null
    }

    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current)
      reconnectTimeoutRef.current = null
    }

    setConnectionState('connecting')

    // Always connect without session filter — store handles filtering.
    // This prevents reconnects when sessionKey changes (which was causing red dot).
    const url = '/api/chat-events'

    const eventSource = new EventSource(url)
    eventSourceRef.current = eventSource

    // Native open event fires on initial connect AND every auto-reconnect
    eventSource.onopen = () => {
      if (!mountedRef.current) return
      reconnectAttempts.current = 0
      // Mark connected immediately — don't wait for custom 'connected' event
      setConnectionState('connected')
    }

    eventSource.addEventListener('connected', () => {
      if (!mountedRef.current) return
      reconnectAttempts.current = 0
      setConnectionState('connected')
    })

    eventSource.addEventListener('disconnected', () => {
      if (!mountedRef.current) return
      setConnectionState('disconnected')
      scheduleReconnect()
    })

    eventSource.addEventListener('error', () => {
      if (!mountedRef.current) return

      if (eventSource.readyState === EventSource.CLOSED) {
        setConnectionState('disconnected')
        scheduleReconnect()
      }
      // Don't set 'connecting' on transient errors — EventSource auto-reconnects
      // and onopen will fire when it succeeds. Avoids flashing red dot.
    })

    eventSource.addEventListener('heartbeat', () => {
      // Keep-alive received, connection is healthy
    })

    // Chat event handlers
    eventSource.addEventListener('chunk', (event) => {
      if (!mountedRef.current) return
      try {
        const data = JSON.parse(event.data) as {
          text: string
          runId?: string
          sessionKey: string
        }
        processEvent({ type: 'chunk', ...data })
        onChunkRef.current?.(data.text, data.sessionKey)
      } catch {
        // Ignore parse errors
      }
    })

    eventSource.addEventListener('thinking', (event) => {
      if (!mountedRef.current) return
      try {
        const data = JSON.parse(event.data) as {
          text: string
          runId?: string
          sessionKey: string
        }
        processEvent({ type: 'thinking', ...data })
        onThinkingRef.current?.(data.text, data.sessionKey)
      } catch {
        // Ignore parse errors
      }
    })

    eventSource.addEventListener('tool', (event) => {
      if (!mountedRef.current) return
      try {
        const data = JSON.parse(event.data) as {
          phase: string
          name: string
          toolCallId?: string
          args?: unknown
          runId?: string
          sessionKey: string
        }
        processEvent({ type: 'tool', ...data })
      } catch {
        // Ignore parse errors
      }
    })

    eventSource.addEventListener('user_message', (event) => {
      if (!mountedRef.current) return
      try {
        const data = JSON.parse(event.data) as {
          message: GatewayMessage
          sessionKey: string
          source?: string
        }
        processEvent({ type: 'user_message', ...data })
        onUserMessageRef.current?.(data.message, data.source)
      } catch {
        // Ignore parse errors
      }
    })

    eventSource.addEventListener('message', (event) => {
      if (!mountedRef.current) return
      try {
        const data = JSON.parse(event.data) as {
          message: GatewayMessage
          sessionKey: string
        }
        processEvent({ type: 'message', ...data })
      } catch {
        // Ignore parse errors
      }
    })

    eventSource.addEventListener('done', (event) => {
      if (!mountedRef.current) return
      try {
        const data = JSON.parse(event.data) as {
          state: string
          errorMessage?: string
          runId?: string
          sessionKey: string
          message?: GatewayMessage
        }
        processEvent({ type: 'done', ...data })
        onDoneRef.current?.(data.state, data.sessionKey)
      } catch {
        // Ignore parse errors
      }
    })

    eventSource.addEventListener('state', () => {
      // State changes (started, thinking) - used for UI indicators
    })
  }, [
    enabled,
    setConnectionState,
    processEvent,
  ])

  const scheduleReconnect = useCallback(() => {
    if (!mountedRef.current || !enabled) return

    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current)
    }

    const attempts = reconnectAttempts.current
    const delay = Math.min(1000 * Math.pow(2, attempts), 30000) // Exponential backoff, max 30s

    reconnectTimeoutRef.current = setTimeout(() => {
      if (!mountedRef.current) return
      reconnectAttempts.current++
      connect()
    }, delay)
  }, [enabled, connect])

  const disconnect = useCallback(() => {
    if (eventSourceRef.current) {
      eventSourceRef.current.close()
      eventSourceRef.current = null
    }

    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current)
      reconnectTimeoutRef.current = null
    }

    setConnectionState('disconnected')
  }, [setConnectionState])

  const reconnect = useCallback(() => {
    disconnect()
    reconnectAttempts.current = 0
    connect()
  }, [disconnect, connect])

  useEffect(() => {
    mountedRef.current = true

    if (enabled) {
      connect()
    }

    return () => {
      mountedRef.current = false
      disconnect()
    }
  }, [enabled, connect, disconnect])

  // No longer reconnect on sessionKey change — SSE receives all events,
  // store handles session filtering. This prevents connection drops.

  return {
    connectionState,
    lastError,
    reconnect,
    disconnect,
  }
}
