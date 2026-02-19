import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useGatewayChatStream } from '../../../hooks/use-gateway-chat-stream'
import { useGatewayChatStore } from '../../../stores/gateway-chat-store'
import { appendHistoryMessage, chatQueryKeys } from '../chat-queries'
import { toast } from '../../../components/ui/toast'
import type { GatewayMessage } from '../types'

type UseRealtimeChatHistoryOptions = {
  sessionKey: string
  friendlyId: string
  historyMessages: Array<GatewayMessage>
  enabled?: boolean
  onUserMessage?: (message: GatewayMessage, source?: string) => void
}

/**
 * Hook that makes SSE the PRIMARY source for new messages and streaming.
 * - Streaming chunks update the gateway-chat-store (already happens)
 * - When 'done' arrives, the complete message is immediately available
 * - History polling is now just a backup/backfill mechanism
 */
export function useRealtimeChatHistory({
  sessionKey,
  friendlyId,
  historyMessages,
  enabled = true,
  onUserMessage,
}: UseRealtimeChatHistoryOptions) {
  const queryClient = useQueryClient()
  const [lastCompletedRunAt, setLastCompletedRunAt] = useState<number | null>(
    null,
  )

  const { connectionState, lastError, reconnect } = useGatewayChatStream({
    sessionKey: sessionKey === 'new' ? undefined : sessionKey,
    enabled: enabled && sessionKey !== 'new',
    onUserMessage: useCallback(
      (message: GatewayMessage, source?: string) => {
        // When we receive a user message from an external channel,
        // append it to the query cache immediately for instant display
        if (sessionKey && sessionKey !== 'new') {
          appendHistoryMessage(queryClient, friendlyId, sessionKey, {
            ...message,
            __realtimeSource: source,
          })
        }
        onUserMessage?.(message, source)
      },
      [queryClient, friendlyId, sessionKey, onUserMessage],
    ),
    onDone: useCallback(
      (_state: string, eventSessionKey: string) => {
        // Track when generation completes for this session
        if (
          eventSessionKey === sessionKey ||
          !sessionKey ||
          sessionKey === 'new'
        ) {
          setLastCompletedRunAt(Date.now())
          // Refetch history after generation completes — keeps chat in sync
          if (sessionKey && sessionKey !== 'new') {
            const key = chatQueryKeys.history(friendlyId, sessionKey)
            const prevData = queryClient.getQueryData(key) as
              | { messages?: GatewayMessage[] }
              | undefined
            const prevCount = prevData?.messages?.length ?? 0

            // Refetch immediately — done event message is already in realtime store
            queryClient.invalidateQueries({ queryKey: key }).then(() => {
              // Check for compaction — significant message count drop
              const newData = queryClient.getQueryData(key) as
                | { messages?: GatewayMessage[] }
                | undefined
              const newCount = newData?.messages?.length ?? 0
              if (
                prevCount > 10 &&
                newCount > 0 &&
                newCount < prevCount * 0.6
              ) {
                toast(
                  'Context compacted — older messages were summarized to free up space',
                  {
                    type: 'info',
                    icon: '🗜️',
                    duration: 8000,
                  },
                )
              }
            })
          }
        }
      },
      [sessionKey, friendlyId, queryClient],
    ),
  })

  const { mergeHistoryMessages, clearSession, lastEventAt } =
    useGatewayChatStore()

  // Subscribe directly to streaming state — useMemo with stable fn ref was stale (bug #1)
  const streamingState = useGatewayChatStore((s) => s.streamingState.get(sessionKey) ?? null)

  // Merge history with real-time messages
  // Re-merge when realtime events arrive (lastEventAt changes)
  const mergedMessages = useMemo(() => {
    if (sessionKey === 'new') return historyMessages
    return mergeHistoryMessages(sessionKey, historyMessages)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionKey, historyMessages, mergeHistoryMessages, lastEventAt])

  // Periodic history sync — catch missed messages every 30s
  // Skip during active streaming to prevent race conditions
  const syncIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null)
  useEffect(() => {
    if (!sessionKey || sessionKey === 'new' || !enabled) return
    syncIntervalRef.current = setInterval(() => {
      // Don't poll during active streaming — causes flicker/overwrites
      if (streamingState !== null) return
      const key = chatQueryKeys.history(friendlyId, sessionKey)
      queryClient.invalidateQueries({ queryKey: key })
    }, 30000)
    return () => {
      if (syncIntervalRef.current) clearInterval(syncIntervalRef.current)
    }
  }, [sessionKey, friendlyId, enabled, queryClient])

  // Clear realtime buffer when session changes
  useEffect(() => {
    if (!sessionKey || sessionKey === 'new') return undefined

    // Clear on unmount/session change after a delay
    // to allow history to catch up
    return () => {
      setTimeout(() => {
        clearSession(sessionKey)
      }, 5000)
    }
  }, [sessionKey, clearSession])

  // Compute streaming UI state
  const isRealtimeStreaming =
    streamingState !== null && streamingState.text.length > 0
  const realtimeStreamingText = streamingState?.text ?? ''
  const realtimeStreamingThinking = streamingState?.thinking ?? ''

  return {
    messages: mergedMessages,
    connectionState,
    lastError,
    reconnect,
    isRealtimeStreaming,
    realtimeStreamingText,
    realtimeStreamingThinking,
    streamingRunId: streamingState?.runId ?? null,
    activeToolCalls: streamingState?.toolCalls ?? [],
    lastCompletedRunAt, // Parent watches this to clear waitingForResponse
  }
}
