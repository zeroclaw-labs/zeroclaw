import { useMemo, useRef } from 'react'
import { useQuery } from '@tanstack/react-query'

import { chatQueryKeys, fetchHistory } from '../chat-queries'
import { getMessageTimestamp, textFromMessage } from '../utils'
import { useChatSettingsStore } from '../../../hooks/use-chat-settings'
import type { QueryClient } from '@tanstack/react-query'
import type { GatewayMessage, HistoryResponse } from '../types'

type UseChatHistoryInput = {
  activeFriendlyId: string
  activeSessionKey: string
  forcedSessionKey?: string
  isNewChat: boolean
  isRedirecting: boolean
  activeExists: boolean
  sessionsReady: boolean
  queryClient: QueryClient
}

function normalizeSessionCandidate(value: string | undefined): string {
  if (!value) return ''
  const trimmed = value.trim()
  if (!trimmed) return ''
  if (trimmed === 'new') return ''
  return trimmed
}

export function useChatHistory({
  activeFriendlyId,
  activeSessionKey,
  forcedSessionKey,
  isNewChat,
  isRedirecting,
  activeExists,
  sessionsReady,
  queryClient,
}: UseChatHistoryInput) {
  const explicitRouteSessionKey = useMemo(() => {
    const normalizedFriendlyId = normalizeSessionCandidate(activeFriendlyId)
    if (!normalizedFriendlyId) return ''
    if (normalizedFriendlyId === 'main') return ''
    return normalizedFriendlyId
  }, [activeFriendlyId])

  const sessionKeyForHistory = useMemo(() => {
    const candidates = [
      normalizeSessionCandidate(forcedSessionKey),
      normalizeSessionCandidate(activeSessionKey),
      explicitRouteSessionKey,
    ]
    const match = candidates.find((candidate) => candidate.length > 0)
    return match || 'main'
  }, [activeSessionKey, explicitRouteSessionKey, forcedSessionKey])

  const historyKey = chatQueryKeys.history(
    activeFriendlyId,
    sessionKeyForHistory,
  )
  const historyQuery = useQuery({
    queryKey: historyKey,
    queryFn: async function fetchHistoryForSession() {
      const cached = queryClient.getQueryData(historyKey) as
        | Record<string, unknown>
        | undefined
      const optimisticMessages = Array.isArray((cached as any)?.messages)
        ? (cached as any).messages.filter((message: any) => {
            if (message.status === 'sending') return true
            if (message.__optimisticId) return true
            return Boolean(message.clientId)
          })
        : []

      const serverData = await fetchHistory({
        sessionKey: sessionKeyForHistory,
        friendlyId: activeFriendlyId,
      })
      if (!optimisticMessages.length) return serverData

      const merged = mergeOptimisticHistoryMessages(
        serverData.messages,
        optimisticMessages,
      )

      return {
        ...serverData,
        messages: merged,
      }
    },
    enabled:
      !isNewChat && // Don't fetch history for new chats
      Boolean(sessionKeyForHistory) &&
      !isRedirecting &&
      (!sessionsReady || activeExists || Boolean(explicitRouteSessionKey)),
    placeholderData: function useCachedHistory(): HistoryResponse | undefined {
      return queryClient.getQueryData(historyKey)
    },
    refetchOnWindowFocus: true,
    gcTime: 1000 * 60 * 10,
    structuralSharing: true,
    notifyOnChangeProps: ['data', 'error', 'isError'],
  })

  const stableHistorySignatureRef = useRef('')
  const stableHistoryMessagesRef = useRef<Array<GatewayMessage>>([])
  const historyMessages = useMemo(() => {
    const messages = Array.isArray(historyQuery.data?.messages)
      ? historyQuery.data.messages
      : []
    const last = messages[messages.length - 1]
    const lastId =
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
      last && typeof (last as { id?: string }).id === 'string'
        ? (last as { id?: string }).id
        : ''
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
    const signature = `${messages.length}:${last?.role ?? ''}:${lastId}:${textFromMessage(last ?? { role: 'user', content: [] }).slice(-32)}`
    if (signature === stableHistorySignatureRef.current) {
      return stableHistoryMessagesRef.current
    }
    stableHistorySignatureRef.current = signature
    stableHistoryMessagesRef.current = messages
    return messages
  }, [historyQuery.data?.messages])

  const showToolMessages = useChatSettingsStore(
    (s) => s.settings.showToolMessages,
  )

  // Filter messages for display - hide tool calls, system events, etc.
  const displayMessages = useMemo(() => {
    const filtered = historyMessages.filter((msg: GatewayMessage) => {
      // Always show user messages (unless system events)
      if (msg.role === 'user') {
        const text = textFromMessage(msg)
        // Filter out system event forwards (subagent task announcements etc)
        if (text.startsWith('A subagent task')) return false
        if (text.startsWith('[Queued announce messages')) return false
        if (text.startsWith('Pre-compaction memory flush')) return false
        if (text.includes('Pre-compaction memory flush')) return false
        if (text.includes('Store durable memories now')) return false
        if (text.includes('Summarize this naturally for the user')) return false
        if (text.includes('APPEND new content only and do not overwrite'))
          return false
        if (
          text.includes('Stats: runtime') &&
          text.includes('sessionKey agent:codex:subagent:')
        )
          return false
        return true
      }

      // Show assistant messages only if they have displayable content
      if (msg.role === 'assistant') {
        // Keep streaming placeholders (they show typing indicator)
        if (msg.__streamingStatus === 'streaming') return true
        // Keep optimistic messages that are pending
        if (msg.__optimisticId && !msg.content?.length) return true

        const content = msg.content
        if (!content || !Array.isArray(content)) return false
        if (content.length === 0) return false

        // Has at least one text block with actual content?
        const hasText = content.some(
          (c) =>
            c.type === 'text' &&
            typeof c.text === 'string' &&
            c.text.trim().length > 0,
        )
        if (!hasText) return false

        return true
      }

      // Hide everything else (toolResult, tool, system messages)
      return false
    })

    // Second pass: mark intermediate assistant messages as narration
    // Only hide messages that are PURELY tool calls (no substantial text)
    // Messages with real text + tool calls are real responses — always show them
    for (let i = 0; i < filtered.length; i++) {
      const msg = filtered[i]
      if (msg.role !== 'assistant') continue
      const content = Array.isArray(msg.content) ? msg.content : []
      const hasToolCall = content.some(
        (c: any) =>
          c.type === 'toolCall' ||
          c.type === 'tool_use' ||
          c.type === 'toolUse',
      )
      if (!hasToolCall) continue

      // Check if this message has substantial text (not just empty/whitespace)
      const substantialText = content.some(
        (c: any) =>
          c.type === 'text' &&
          typeof c.text === 'string' &&
          c.text.trim().length > 20,
      )
      // If it has real text content, it's a response — never hide it
      if (substantialText) continue

      const hasLater = filtered
        .slice(i + 1)
        .some((m: GatewayMessage) => m.role === 'assistant')
      if (hasLater) {
        if (!showToolMessages) {
          // Hide intermediate narration entirely
          filtered.splice(i, 1)
          i--
        } else {
          ;(msg as any).__isNarration = true
        }
      }
    }

    return filtered
  }, [historyMessages, showToolMessages])

  const messageCount = useMemo(() => {
    return historyMessages.filter((message) => {
      if (message.role !== 'user' && message.role !== 'assistant') return false
      return Boolean(textFromMessage(message))
    }).length
  }, [historyMessages])

  const historyError =
    historyQuery.error instanceof Error ? historyQuery.error.message : null
  const resolvedSessionKey = useMemo(() => {
    const normalizedForced = normalizeSessionCandidate(forcedSessionKey)
    if (normalizedForced) return normalizedForced
    const key = historyQuery.data?.sessionKey
    if (typeof key === 'string' && key.trim().length > 0) {
      return key.trim()
    }
    const normalizedActive = normalizeSessionCandidate(activeSessionKey)
    if (normalizedActive) return normalizedActive
    if (explicitRouteSessionKey) return explicitRouteSessionKey
    return 'main'
  }, [
    activeSessionKey,
    explicitRouteSessionKey,
    forcedSessionKey,
    historyQuery.data?.sessionKey,
  ])
  const activeCanonicalKey =
    resolvedSessionKey || sessionKeyForHistory || 'main'

  return {
    historyQuery,
    historyMessages,
    displayMessages,
    messageCount,
    historyError,
    resolvedSessionKey,
    activeCanonicalKey,
    sessionKeyForHistory,
  }
}

function mergeOptimisticHistoryMessages(
  serverMessages: Array<GatewayMessage>,
  optimisticMessages: Array<GatewayMessage>,
): Array<GatewayMessage> {
  if (!optimisticMessages.length) return serverMessages

  const merged = [...serverMessages]

  for (const optimisticMessage of optimisticMessages) {
    // Check if this optimistic message has been confirmed by the server
    const hasMatch = serverMessages.some((serverMessage) => {
      // Primary match: clientId (most reliable)
      if (
        optimisticMessage.clientId &&
        serverMessage.clientId &&
        optimisticMessage.clientId === serverMessage.clientId
      ) {
        return true
      }

      // Secondary match: __optimisticId
      if (
        optimisticMessage.__optimisticId &&
        serverMessage.__optimisticId &&
        optimisticMessage.__optimisticId === serverMessage.__optimisticId
      ) {
        return true
      }

      // Fallback match: same text content + role + timestamp within 10s
      if (optimisticMessage.role && serverMessage.role) {
        if (optimisticMessage.role !== serverMessage.role) return false
      }
      const optimisticText = textFromMessage(optimisticMessage)
      if (!optimisticText) return false
      if (optimisticText !== textFromMessage(serverMessage)) return false
      const optimisticTime = getMessageTimestamp(optimisticMessage)
      const serverTime = getMessageTimestamp(serverMessage)
      return Math.abs(optimisticTime - serverTime) <= 10000
    })

    if (!hasMatch) {
      // Preserve unconfirmed optimistic messages regardless of age
      // They will be shown with a "queued" indicator
      const isSending =
        optimisticMessage.status === 'sending' ||
        Boolean(optimisticMessage.__optimisticId)

      if (isSending) {
        merged.push(optimisticMessage)
      }
    }
  }

  return merged
}
