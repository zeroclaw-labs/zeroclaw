import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'

import {
  deriveFriendlyIdFromKey,
  isMissingGatewayAuth,
  readError,
  textFromMessage,
} from './utils'
import { createOptimisticMessage } from './chat-screen-utils'
import {
  appendHistoryMessage,
  chatQueryKeys,
  clearHistoryMessages,
  fetchGatewayStatus,
  updateHistoryMessageByClientId,
  updateSessionLastMessage,
} from './chat-queries'
import { ChatHeader } from './components/chat-header'
import { ChatMessageList } from './components/chat-message-list'
import { ChatEmptyState } from './components/chat-empty-state'
import { ChatComposer } from './components/chat-composer'
import { GatewayStatusMessage } from './components/gateway-status-message'
import {
  consumePendingSend,
  hasPendingGeneration,
  hasPendingSend,
  isRecentSession,
  resetPendingSend,
  setPendingGeneration,
} from './pending-send'
import { useChatMeasurements } from './hooks/use-chat-measurements'
import { useChatHistory } from './hooks/use-chat-history'
import { useRealtimeChatHistory } from './hooks/use-realtime-chat-history'
import { useChatMobile } from './hooks/use-chat-mobile'
import { useChatSessions } from './hooks/use-chat-sessions'
import { useAutoSessionTitle } from './hooks/use-auto-session-title'
import { ContextBar } from './components/context-bar'
import type {
  ChatComposerAttachment,
  ChatComposerHandle,
  ChatComposerHelpers,
} from './components/chat-composer'
import type { GatewayAttachment, GatewayMessage, SessionMeta } from './types'
import { cn } from '@/lib/utils'
import { toast } from '@/components/ui/toast'
import { FileExplorerSidebar } from '@/components/file-explorer'
import { SEARCH_MODAL_EVENTS } from '@/hooks/use-search-modal'
import { SIDEBAR_TOGGLE_EVENT } from '@/hooks/use-global-shortcuts'
import { useWorkspaceStore } from '@/stores/workspace-store'
import { TerminalPanel } from '@/components/terminal-panel'
import { AgentViewPanel } from '@/components/agent-view/agent-view-panel'
import { useAgentViewStore } from '@/hooks/use-agent-view'
import { useTerminalPanelStore } from '@/stores/terminal-panel-store'
import { useModelSuggestions } from '@/hooks/use-model-suggestions'
import { ModelSuggestionToast } from '@/components/model-suggestion-toast'
import { useChatActivityStore } from '@/stores/chat-activity-store'
import { MobileSessionsPanel } from '@/components/mobile-sessions-panel'
import { MOBILE_TAB_BAR_OFFSET } from '@/components/mobile-tab-bar'
import { useTapDebug } from '@/hooks/use-tap-debug'

type ChatScreenProps = {
  activeFriendlyId: string
  isNewChat?: boolean
  onSessionResolved?: (payload: {
    sessionKey: string
    friendlyId: string
  }) => void
  forcedSessionKey?: string
  /** Hide header + file explorer + terminal for panel mode */
  compact?: boolean
}

function normalizeMessageValue(value: unknown): string {
  if (typeof value !== 'string') return ''
  const trimmed = value.trim()
  return trimmed.length > 0 ? trimmed : ''
}

function getMessageClientId(message: GatewayMessage): string {
  const raw = message as Record<string, unknown>
  const directClientId = normalizeMessageValue(raw.clientId)
  if (directClientId) return directClientId

  const alternateClientId = normalizeMessageValue(raw.client_id)
  if (alternateClientId) return alternateClientId

  const optimisticId = normalizeMessageValue(raw.__optimisticId)
  if (optimisticId.startsWith('opt-')) {
    return optimisticId.slice(4)
  }
  return ''
}

function getRetryMessageKey(message: GatewayMessage): string {
  const clientId = getMessageClientId(message)
  if (clientId) return `client:${clientId}`

  const raw = message as Record<string, unknown>
  const optimisticId = normalizeMessageValue(raw.__optimisticId)
  if (optimisticId) return `optimistic:${optimisticId}`

  const messageId = normalizeMessageValue(raw.id)
  if (messageId) return `id:${messageId}`

  const timestamp = normalizeMessageValue(
    typeof raw.timestamp === 'number' ? String(raw.timestamp) : raw.timestamp,
  )
  const messageText = textFromMessage(message).trim()
  return `fallback:${message.role ?? 'unknown'}:${timestamp}:${messageText}`
}

function isRetryableQueuedMessage(message: GatewayMessage): boolean {
  if ((message.role || '') !== 'user') return false
  const raw = message as Record<string, unknown>
  const status = normalizeMessageValue(raw.status)
  const optimisticId = normalizeMessageValue(raw.__optimisticId)
  return status === 'sending' || status === 'error' || optimisticId.length > 0
}

function getMessageRetryAttachments(
  message: GatewayMessage,
): Array<GatewayAttachment> {
  if (!Array.isArray(message.attachments)) return []
  return message.attachments.filter((attachment) => {
    return Boolean(attachment) && typeof attachment === 'object'
  })
}

export function ChatScreen({
  activeFriendlyId,
  isNewChat = false,
  onSessionResolved,
  forcedSessionKey,
  compact = false,
}: ChatScreenProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [sending, setSending] = useState(false)
  const [_creatingSession, setCreatingSession] = useState(false)
  const [sessionsOpen, setSessionsOpen] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [isRedirecting, setIsRedirecting] = useState(false)
  const { headerRef, composerRef, mainRef, pinGroupMinHeight, headerHeight } =
    useChatMeasurements()
  useTapDebug(mainRef, { label: 'chat-main' })
  const [waitingForResponse, setWaitingForResponse] = useState(
    () => hasPendingSend() || hasPendingGeneration(),
  )
  const streamTimer = useRef<number | null>(null)
  const streamIdleTimer = useRef<number | null>(null)
  const lastAssistantSignature = useRef('')
  const refreshHistoryRef = useRef<() => void>(() => {})
  const retriedQueuedMessageKeysRef = useRef(new Set<string>())
  const hasSeenGatewayDisconnectRef = useRef(false)
  const hadGatewayErrorRef = useRef(false)

  const pendingStartRef = useRef(false)
  const composerHandleRef = useRef<ChatComposerHandle | null>(null)
  const [fileExplorerCollapsed, setFileExplorerCollapsed] = useState(() => {
    if (typeof window === 'undefined') return true
    const stored = localStorage.getItem('clawsuite-file-explorer-collapsed')
    return stored === null ? true : stored === 'true'
  })
  const { isMobile } = useChatMobile(queryClient)
  const mobileKeyboardInset = useWorkspaceStore((s) => s.mobileKeyboardInset)
  const mobileComposerFocused = useWorkspaceStore((s) => s.mobileComposerFocused)
  const mobileKeyboardActive = mobileKeyboardInset > 0 || mobileComposerFocused
  const isAgentViewOpen = useAgentViewStore((state) => state.isOpen)
  const setAgentViewOpen = useAgentViewStore((state) => state.setOpen)
  const isTerminalPanelOpen = useTerminalPanelStore(
    (state) => state.isPanelOpen,
  )
  const terminalPanelHeight = useTerminalPanelStore(
    (state) => state.panelHeight,
  )
  const {
    sessionsQuery,
    sessions,
    activeSession,
    activeExists,
    activeSessionKey,
    activeTitle,
    sessionsError,
    sessionsLoading: _sessionsLoading,
    sessionsFetching: _sessionsFetching,
    refetchSessions: _refetchSessions,
  } = useChatSessions({ activeFriendlyId, isNewChat, forcedSessionKey })
  const {
    historyQuery,
    historyMessages,
    messageCount,
    historyError,
    resolvedSessionKey,
    activeCanonicalKey,
    sessionKeyForHistory,
  } = useChatHistory({
    activeFriendlyId,
    activeSessionKey,
    forcedSessionKey,
    isNewChat,
    isRedirecting,
    activeExists,
    sessionsReady: sessionsQuery.isSuccess,
    queryClient,
  })

  // Wire SSE realtime stream for instant message delivery
  const {
    messages: realtimeMessages,
    lastCompletedRunAt,
    connectionState,
    isRealtimeStreaming,
    realtimeStreamingText,
    realtimeStreamingThinking,
    activeToolCalls,
  } = useRealtimeChatHistory({
      sessionKey: resolvedSessionKey || activeCanonicalKey,
      friendlyId: activeFriendlyId,
      historyMessages,
      enabled: !isNewChat && !isRedirecting,
      onUserMessage: useCallback(() => {
        // External message arrived (e.g. from Telegram) — show thinking indicator
        setWaitingForResponse(true)
        setPendingGeneration(true)
      }, []),
    })

  // Use realtime-merged messages for display (SSE + history)
  // Re-apply display filter to realtime messages
  const finalDisplayMessages = useMemo(() => {
    // Rebuild display filter on merged messages
    return realtimeMessages.filter((msg) => {
      if (msg.role === 'user') {
        const text = textFromMessage(msg)
        if (text.startsWith('A subagent task')) return false
        return true
      }
      if (msg.role === 'assistant') {
        if (msg.__streamingStatus === 'streaming') return true
        if ((msg as any).__optimisticId && !msg.content?.length) return true
        const content = msg.content
        if (!content || !Array.isArray(content)) return false
        if (content.length === 0) return false
        const hasText = content.some(
          (c) =>
            c.type === 'text' &&
            typeof c.text === 'string' &&
            c.text.trim().length > 0,
        )
        return hasText
      }
      return false
    })
  }, [realtimeMessages])

  // Derive streaming state from realtime SSE state (bug #2 fix)
  const derivedStreamingInfo = useMemo(() => {
    // Use actual realtime streaming state when available
    if (isRealtimeStreaming) {
      const last = finalDisplayMessages[finalDisplayMessages.length - 1]
      const id = last?.role === 'assistant'
        ? ((last as any).__optimisticId || (last as any).id || null)
        : null
      return { isStreaming: true, streamingMessageId: id }
    }
    // Fallback: waiting for response + last message is assistant
    if (waitingForResponse && finalDisplayMessages.length > 0) {
      const last = finalDisplayMessages[finalDisplayMessages.length - 1]
      if (last && last.role === 'assistant') {
        const id = (last as any).__optimisticId || (last as any).id || null
        return { isStreaming: true, streamingMessageId: id }
      }
    }
    return { isStreaming: false, streamingMessageId: null as string | null }
  }, [waitingForResponse, finalDisplayMessages, isRealtimeStreaming])

  // --- Stream management ---
  const streamStop = useCallback(() => {
    if (streamTimer.current) {
      window.clearTimeout(streamTimer.current)
      streamTimer.current = null
    }
    if (streamIdleTimer.current) {
      window.clearTimeout(streamIdleTimer.current)
      streamIdleTimer.current = null
    }
  }, [])

  useEffect(() => {
    return () => {
      streamStop()
    }
  }, [streamStop])

  const streamFinish = useCallback(() => {
    streamStop()
    setPendingGeneration(false)
    setWaitingForResponse(false)
  }, [streamStop])

  const streamStart = useCallback(() => {
    if (!activeFriendlyId || isNewChat) return
    // Bug #3 fix: no more 350ms polling loop — SSE handles realtime updates.
    // Single delayed fetch as fallback to catch the initial response.
    if (streamTimer.current) window.clearTimeout(streamTimer.current)
    streamTimer.current = window.setTimeout(() => {
      refreshHistoryRef.current()
    }, 2000)
  }, [activeFriendlyId, isNewChat])

  refreshHistoryRef.current = function refreshHistory() {
    if (historyQuery.isFetching) return
    void historyQuery.refetch()
  }

  // Track message count when waiting started — only clear when NEW assistant msg appears
  const messageCountAtSendRef = useRef(0)

  useEffect(() => {
    if (waitingForResponse) {
      messageCountAtSendRef.current = finalDisplayMessages.length
    }
  }, [waitingForResponse]) // eslint-disable-line react-hooks/exhaustive-deps

  // Clear waitingForResponse when a NEW assistant message appears after send
  // Use a ref to prevent the cleanup/restart race condition
  const clearTimerRef = useRef<number | null>(null)
  useEffect(() => {
    if (!waitingForResponse) {
      if (clearTimerRef.current) {
        window.clearTimeout(clearTimerRef.current)
        clearTimerRef.current = null
      }
      return
    }
    // Only check if display has grown since we sent
    if (finalDisplayMessages.length <= messageCountAtSendRef.current) return
    const last = finalDisplayMessages[finalDisplayMessages.length - 1]
    if (last && last.role === 'assistant') {
      // Already scheduled? Don't restart
      if (clearTimerRef.current) return
      clearTimerRef.current = window.setTimeout(() => {
        clearTimerRef.current = null
        streamFinish()
      }, 50) // Tiny delay to let React render the message first
    }
  }, [finalDisplayMessages.length, waitingForResponse, streamFinish])

  // Failsafe: clear after done event + 10s if response never shows in display
  useEffect(() => {
    if (lastCompletedRunAt && waitingForResponse) {
      const timer = window.setTimeout(() => streamFinish(), 10000)
      return () => window.clearTimeout(timer)
    }
  }, [lastCompletedRunAt, waitingForResponse, streamFinish])

  // Hard failsafe: if waiting for 5s+ and SSE missed the done event, refetch history
  useEffect(() => {
    if (!waitingForResponse) return
    const fallback = window.setTimeout(() => {
      refreshHistoryRef.current()
    }, 5000)
    return () => window.clearTimeout(fallback)
  }, [waitingForResponse])

  useAutoSessionTitle({
    friendlyId: activeFriendlyId,
    sessionKey: resolvedSessionKey,
    activeSession,
    messages: historyMessages,
    messageCount,
    enabled:
      !isNewChat && Boolean(resolvedSessionKey) && historyQuery.isSuccess,
  })

  // Phase 4.1: Smart Model Suggestions
  const modelsQuery = useQuery({
    queryKey: ['models'],
    queryFn: async () => {
      const res = await fetch('/api/models')
      if (!res.ok) return { models: [] }
      const data = await res.json()
      return data
    },
    staleTime: 5 * 60 * 1000, // 5 minutes
  })

  const currentModelQuery = useQuery({
    queryKey: ['gateway', 'session-status-model'],
    queryFn: async () => {
      try {
        const res = await fetch('/api/session-status')
        if (!res.ok) return ''
        const data = await res.json()
        const payload = data.payload ?? data
        // Same logic as chat-composer: read model from status payload
        if (payload.model) return String(payload.model)
        if (payload.currentModel) return String(payload.currentModel)
        if (payload.modelAlias) return String(payload.modelAlias)
        if (payload.resolved?.modelProvider && payload.resolved?.model) {
          return `${payload.resolved.modelProvider}/${payload.resolved.model}`
        }
        return ''
      } catch {
        return ''
      }
    },
    refetchInterval: 30_000,
    retry: false,
  })

  const availableModelIds = useMemo(() => {
    const models = modelsQuery.data?.models || []
    return models.map((m: any) => m.id).filter((id: string) => id)
  }, [modelsQuery.data])

  const currentModel = currentModelQuery.data || ''

  const { suggestion, dismiss, dismissForSession } = useModelSuggestions({
    currentModel, // Real model from session-status (fail closed if empty)
    sessionKey: resolvedSessionKey || 'main',
    messages: historyMessages.map((m) => ({
      role: m.role as 'user' | 'assistant',
      content: textFromMessage(m),
    })) as any,
    availableModels: availableModelIds,
  })

  const handleSwitchModel = useCallback(async () => {
    if (!suggestion) return

    try {
      const res = await fetch('/api/model-switch', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          sessionKey: resolvedSessionKey || 'main',
          model: suggestion.suggestedModel,
        }),
      })

      if (res.ok) {
        dismiss()
        // Optionally show success toast or update UI
      }
    } catch (err) {
      setError(
        `Failed to switch model. ${err instanceof Error ? err.message : String(err)}`,
      )
    }
  }, [suggestion, resolvedSessionKey, dismiss])

  // Sync chat activity to global store for sidebar orchestrator avatar
  const setLocalActivity = useChatActivityStore((s) => s.setLocalActivity)
  useEffect(() => {
    if (waitingForResponse) {
      setLocalActivity('thinking')
    } else {
      setLocalActivity('idle')
    }
  }, [waitingForResponse, setLocalActivity])

  const gatewayStatusQuery = useQuery({
    queryKey: ['gateway', 'status'],
    queryFn: fetchGatewayStatus,
    retry: 2,
    retryDelay: 1000,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
    refetchOnMount: true,
    staleTime: 30_000,
    refetchInterval: 60_000, // Re-check every 60s to clear stale errors
  })
  // Don't show gateway errors for new chats or when SSE is connected (proves gateway works)
  const gatewayStatusError =
    !isNewChat && connectionState !== 'connected' &&
    (gatewayStatusQuery.error instanceof Error
      ? gatewayStatusQuery.error.message
      : gatewayStatusQuery.data && !gatewayStatusQuery.data.ok
        ? gatewayStatusQuery.data.error || 'Gateway unavailable'
        : null)
  const gatewayError = gatewayStatusError ?? sessionsError ?? historyError
  const showErrorNotice = Boolean(gatewayError) && !isNewChat
  const handleGatewayRefetch = useCallback(() => {
    void gatewayStatusQuery.refetch()
    void sessionsQuery.refetch()
    void historyQuery.refetch()
  }, [gatewayStatusQuery, sessionsQuery, historyQuery])

  const handleRefreshHistory = useCallback(() => {
    void historyQuery.refetch()
  }, [historyQuery])

  useEffect(() => {
    const handleRefreshRequest = () => {
      void historyQuery.refetch()
    }
    window.addEventListener('clawsuite:chat-refresh', handleRefreshRequest)
    return () => {
      window.removeEventListener('clawsuite:chat-refresh', handleRefreshRequest)
    }
  }, [historyQuery])

  const terminalPanelInset =
    !isMobile && isTerminalPanelOpen ? terminalPanelHeight : 0
  const mobileScrollBottomOffset = useMemo(() => {
    if (!isMobile) return 0
    if (mobileKeyboardActive) {
      return 'calc(var(--chat-composer-height, 96px) + var(--kb-inset, 0px))'
    }
    return `calc(var(--chat-composer-height, 96px) + ${MOBILE_TAB_BAR_OFFSET})`
  }, [isMobile, mobileKeyboardActive])

  // Keep message list clear of composer, keyboard, and desktop terminal panel.
  const stableContentStyle = useMemo<React.CSSProperties>(() => {
    if (isMobile) {
      const mobileBase = mobileKeyboardActive
        ? 'calc(var(--chat-composer-height, 96px) + var(--kb-inset, 0px))'
        : `calc(var(--chat-composer-height, 96px) + ${MOBILE_TAB_BAR_OFFSET})`
      return {
        paddingBottom: `calc(${mobileBase} + var(--safe-b) + 16px)`,
      }
    }
    return {
      paddingBottom:
        terminalPanelInset > 0
          ? `${terminalPanelInset + 16}px`
          : '16px',
    }
  }, [isMobile, mobileKeyboardActive, terminalPanelInset])

  const shouldRedirectToNew =
    !isNewChat &&
    !forcedSessionKey &&
    !isRecentSession(activeFriendlyId) &&
    sessionsQuery.isSuccess &&
    sessions.length > 0 &&
    !sessions.some((session) => session.friendlyId === activeFriendlyId) &&
    !historyQuery.isFetching &&
    !historyQuery.isSuccess

  useEffect(() => {
    if (isRedirecting) {
      if (error) setError(null)
      return
    }
    if (shouldRedirectToNew) {
      if (error) setError(null)
      return
    }
    if (
      sessionsQuery.isSuccess &&
      !activeExists &&
      !sessionsError &&
      !historyError
    ) {
      if (error) setError(null)
      return
    }
    const messageText = sessionsError ?? historyError ?? gatewayStatusError
    if (!messageText) {
      if (error?.startsWith('Failed to load')) {
        setError(null)
      }
      return
    }
    if (isMissingGatewayAuth(messageText)) {
      navigate({ to: '/connect', replace: true })
    }
    const message = sessionsError
      ? `Failed to load sessions. ${sessionsError}`
      : historyError
        ? `Failed to load history. ${historyError}`
        : gatewayStatusError
          ? `Gateway unavailable. ${gatewayStatusError}`
          : null
    if (message) setError(message)
  }, [
    activeExists,
    error,
    gatewayStatusError,
    historyError,
    isRedirecting,
    navigate,
    sessionsError,
    sessionsQuery.isSuccess,
    shouldRedirectToNew,
  ])

  useEffect(() => {
    if (!isRedirecting) return
    if (isNewChat) {
      setIsRedirecting(false)
      return
    }
    if (!shouldRedirectToNew && sessionsQuery.isSuccess) {
      setIsRedirecting(false)
    }
  }, [isNewChat, isRedirecting, sessionsQuery.isSuccess, shouldRedirectToNew])

  useEffect(() => {
    if (isNewChat) return
    if (!sessionsQuery.isSuccess) return
    if (sessions.length === 0) return
    if (!shouldRedirectToNew) return
    resetPendingSend()
    clearHistoryMessages(queryClient, activeFriendlyId, sessionKeyForHistory)
    navigate({ to: '/new', replace: true })
  }, [
    activeFriendlyId,
    historyQuery.isFetching,
    historyQuery.isSuccess,
    isNewChat,
    navigate,
    queryClient,
    sessionKeyForHistory,
    sessions,
    sessionsQuery.isSuccess,
    shouldRedirectToNew,
  ])

  const hideUi = shouldRedirectToNew || isRedirecting
  const showComposer = !isRedirecting

  // Reset state when session changes
  useEffect(() => {
    const resetKey = isNewChat ? 'new' : activeFriendlyId
    if (!resetKey) return
    retriedQueuedMessageKeysRef.current.clear()
    if (pendingStartRef.current) {
      pendingStartRef.current = false
      return
    }
    if (hasPendingSend() || hasPendingGeneration()) {
      setWaitingForResponse(true)
      return
    }
    streamStop()
    lastAssistantSignature.current = ''
    setWaitingForResponse(false)
  }, [activeFriendlyId, isNewChat, streamStop])

  useLayoutEffect(() => {
    if (isNewChat) return
    const pending = consumePendingSend(
      forcedSessionKey || resolvedSessionKey || activeSessionKey,
      activeFriendlyId,
    )
    if (!pending) return
    pendingStartRef.current = true
    const historyKey = chatQueryKeys.history(
      pending.friendlyId,
      pending.sessionKey,
    )
    const cached = queryClient.getQueryData(historyKey)
    const cachedMessages = Array.isArray((cached as any)?.messages)
      ? (cached as any).messages
      : []
    const alreadyHasOptimistic = cachedMessages.some((message: any) => {
      if (pending.optimisticMessage.clientId) {
        if (message.clientId === pending.optimisticMessage.clientId) return true
        if (message.__optimisticId === pending.optimisticMessage.clientId)
          return true
      }
      if (pending.optimisticMessage.__optimisticId) {
        if (message.__optimisticId === pending.optimisticMessage.__optimisticId)
          return true
      }
      return false
    })
    if (!alreadyHasOptimistic) {
      appendHistoryMessage(
        queryClient,
        pending.friendlyId,
        pending.sessionKey,
        pending.optimisticMessage,
      )
    }
    setWaitingForResponse(true)
    sendMessage(
      pending.sessionKey,
      pending.friendlyId,
      pending.message,
      pending.attachments,
      true,
      typeof pending.optimisticMessage.clientId === 'string'
        ? pending.optimisticMessage.clientId
        : '',
    )
  }, [
    activeFriendlyId,
    activeSessionKey,
    forcedSessionKey,
    isNewChat,
    queryClient,
    resolvedSessionKey,
  ])

  /**
   * Simplified sendMessage - fire and forget.
   * Response arrives via SSE stream, not via this function.
   */
  function sendMessage(
    sessionKey: string,
    friendlyId: string,
    body: string,
    attachments: Array<GatewayAttachment> = [],
    skipOptimistic = false,
    existingClientId = '',
  ) {
    setLocalActivity('reading')
    const normalizedAttachments = attachments.map((attachment) => ({
      ...attachment,
      id: attachment.id ?? crypto.randomUUID(),
    }))

    let optimisticClientId = existingClientId
    if (!skipOptimistic) {
      const { clientId, optimisticMessage } = createOptimisticMessage(
        body,
        normalizedAttachments,
      )
      optimisticClientId = clientId
      appendHistoryMessage(
        queryClient,
        friendlyId,
        sessionKey,
        optimisticMessage,
      )
      updateSessionLastMessage(
        queryClient,
        sessionKey,
        friendlyId,
        optimisticMessage,
      )
    }

    setPendingGeneration(true)
    setSending(true)
    setError(null)
    setWaitingForResponse(true)

    // Failsafe: clear waitingForResponse after 120s no matter what
    // Prevents infinite spinner if SSE/idle detection both fail
    const failsafeTimer = window.setTimeout(() => {
      streamFinish()
    }, 120_000)

    // Map to gateway-expected field names:
    // gateway wants: mimeType, fileName, content (base64 without data: prefix)
    const payloadAttachments = normalizedAttachments.map((attachment) => ({
      id: attachment.id,
      fileName: attachment.name,
      mimeType: attachment.contentType,
      type: attachment.contentType?.startsWith('image/') ? 'image' : 'file',
      content: attachment.dataUrl?.replace(/^data:[^;]+;base64,/, '') ?? '',
      size: attachment.size,
    }))

    fetch('/api/send', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        sessionKey,
        friendlyId,
        message: body,
        attachments:
          payloadAttachments.length > 0 ? payloadAttachments : undefined,
        thinking: 'low',
        idempotencyKey: optimisticClientId || crypto.randomUUID(),
        clientId: optimisticClientId || undefined,
      }),
    })
      .then(async (res) => {
        if (!res.ok) {
          let errorText = `HTTP ${res.status}`
          try {
            errorText = await readError(res)
          } catch {
            /* ignore parse errors */
          }
          throw new Error(errorText)
        }
        // Stream setup is separate — don't let it trigger send failure
        try {
          streamStart()
        } catch (e) {
          if (import.meta.env.DEV)
            console.warn('[chat] streamStart error (non-fatal):', e)
        }
        setSending(false)
      })
      .catch((err: unknown) => {
        window.clearTimeout(failsafeTimer)
        setSending(false)
        const messageText = err instanceof Error ? err.message : String(err)
        if (isMissingGatewayAuth(messageText)) {
          try {
            navigate({ to: '/connect', replace: true })
          } catch {
            /* router not ready */
          }
          return
        }
        // Only mark as failed for actual network/API errors
        if (optimisticClientId) {
          updateHistoryMessageByClientId(
            queryClient,
            friendlyId,
            sessionKey,
            optimisticClientId,
            function markFailed(message) {
              return { ...message, status: 'error' }
            },
          )
        }
        const errorMessage = `Failed to send message. ${messageText}`
        setError(errorMessage)
        toast('Failed to send message', { type: 'error' })
        setPendingGeneration(false)
        setWaitingForResponse(false)
      })
  }

  const retryQueuedMessage = useCallback(
    function retryQueuedMessage(message: GatewayMessage, mode: 'manual' | 'auto') {
      if (!isRetryableQueuedMessage(message)) return false

      const body = textFromMessage(message).trim()
      const attachments = getMessageRetryAttachments(message)
      if (body.length === 0 && attachments.length === 0) return false

      const retryKey = getRetryMessageKey(message)
      if (mode === 'auto' && retriedQueuedMessageKeysRef.current.has(retryKey)) {
        return false
      }

      const sessionKeyForSend =
        forcedSessionKey || resolvedSessionKey || activeSessionKey || 'main'
      const sessionKeyForMessage = sessionKeyForHistory || sessionKeyForSend
      const existingClientId = getMessageClientId(message)

      if (existingClientId) {
        updateHistoryMessageByClientId(
          queryClient,
          activeFriendlyId,
          sessionKeyForMessage,
          existingClientId,
          function markSending(currentMessage) {
            return { ...currentMessage, status: 'sending' }
          },
        )
      }

      if (mode === 'auto') {
        retriedQueuedMessageKeysRef.current.add(retryKey)
      }

      sendMessage(
        sessionKeyForSend,
        activeFriendlyId,
        body,
        attachments,
        true,
        existingClientId,
      )
      return true
    },
    [
      activeFriendlyId,
      activeSessionKey,
      forcedSessionKey,
      queryClient,
      resolvedSessionKey,
      sessionKeyForHistory,
    ],
  )

  const flushRetryableMessages = useCallback(
    function flushRetryableMessages() {
      for (const message of finalDisplayMessages) {
        retryQueuedMessage(message, 'auto')
      }
    },
    [finalDisplayMessages, retryQueuedMessage],
  )

  const handleRetryMessage = useCallback(
    function handleRetryMessage(message: GatewayMessage) {
      const retryKey = getRetryMessageKey(message)
      retriedQueuedMessageKeysRef.current.delete(retryKey)
      retryQueuedMessage(message, 'manual')
    },
    [retryQueuedMessage],
  )

  useEffect(() => {
    if (connectionState === 'error' || connectionState === 'disconnected') {
      hasSeenGatewayDisconnectRef.current = true
      retriedQueuedMessageKeysRef.current.clear()
      return
    }

    if (connectionState === 'connected' && hasSeenGatewayDisconnectRef.current) {
      hasSeenGatewayDisconnectRef.current = false
      flushRetryableMessages()
    }
  }, [connectionState, flushRetryableMessages])

  useEffect(() => {
    if (gatewayStatusError) {
      hadGatewayErrorRef.current = true
      retriedQueuedMessageKeysRef.current.clear()
      return
    }

    const isGatewayHealthy = gatewayStatusQuery.data?.ok === true
    if (isGatewayHealthy && hadGatewayErrorRef.current) {
      hadGatewayErrorRef.current = false
      flushRetryableMessages()
    }
  }, [flushRetryableMessages, gatewayStatusError, gatewayStatusQuery.data])

  useEffect(() => {
    function handleGatewayHealthRestored() {
      retriedQueuedMessageKeysRef.current.clear()
      hadGatewayErrorRef.current = false
      flushRetryableMessages()
      handleGatewayRefetch()
    }

    window.addEventListener('gateway:health-restored', handleGatewayHealthRestored)
    return () => {
      window.removeEventListener(
        'gateway:health-restored',
        handleGatewayHealthRestored,
      )
    }
  }, [flushRetryableMessages, handleGatewayRefetch])

  const createSessionForMessage = useCallback(
    async (preferredFriendlyId?: string) => {
      setCreatingSession(true)
      try {
        const res = await fetch('/api/sessions', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(
            preferredFriendlyId && preferredFriendlyId.trim().length > 0
              ? { friendlyId: preferredFriendlyId }
              : {},
          ),
        })
        if (!res.ok) throw new Error(await readError(res))

        const data = (await res.json()) as {
          sessionKey?: string
          friendlyId?: string
        }

        const sessionKey =
          typeof data.sessionKey === 'string' ? data.sessionKey : ''
        const friendlyId =
          typeof data.friendlyId === 'string' &&
          data.friendlyId.trim().length > 0
            ? data.friendlyId.trim()
            : (preferredFriendlyId?.trim() ?? '') ||
              deriveFriendlyIdFromKey(sessionKey)

        if (!sessionKey || !friendlyId) {
          throw new Error('Invalid session response')
        }

        queryClient.invalidateQueries({ queryKey: chatQueryKeys.sessions })
        return { sessionKey, friendlyId }
      } finally {
        setCreatingSession(false)
      }
    },
    [queryClient],
  )

  const upsertSessionInCache = useCallback(
    (friendlyId: string, lastMessage: GatewayMessage) => {
      if (!friendlyId) return
      queryClient.setQueryData(
        chatQueryKeys.sessions,
        function upsert(existing: unknown) {
          const sessions = Array.isArray(existing)
            ? (existing as Array<SessionMeta>)
            : []
          const now = Date.now()
          const existingIndex = sessions.findIndex((session) => {
            return (
              session.friendlyId === friendlyId || session.key === friendlyId
            )
          })

          if (existingIndex === -1) {
            return [
              {
                key: friendlyId,
                friendlyId,
                updatedAt: now,
                lastMessage,
                titleStatus: 'idle',
              },
              ...sessions,
            ]
          }

          return sessions.map((session, index) => {
            if (index !== existingIndex) return session
            return {
              ...session,
              updatedAt: now,
              lastMessage,
            }
          })
        },
      )
    },
    [queryClient],
  )

  const send = useCallback(
    (
      body: string,
      attachments: Array<ChatComposerAttachment>,
      helpers: ChatComposerHelpers,
    ) => {
      const trimmedBody = body.trim()
      if (trimmedBody.length === 0 && attachments.length === 0) return
      helpers.reset()

      const attachmentPayload: Array<GatewayAttachment> = attachments.map(
        (attachment) => ({
          ...attachment,
          // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
          id: attachment.id ?? crypto.randomUUID(),
        }),
      )

      if (isNewChat) {
        const threadId = crypto.randomUUID()
        const { optimisticMessage } = createOptimisticMessage(
          trimmedBody,
          attachmentPayload,
        )
        appendHistoryMessage(queryClient, threadId, threadId, optimisticMessage)
        upsertSessionInCache(threadId, optimisticMessage)
        setPendingGeneration(true)
        setSending(true)
        setWaitingForResponse(true)

        void createSessionForMessage(threadId).catch((err: unknown) => {
          if (import.meta.env.DEV) {
            console.warn('[chat] failed to register new thread', err)
          }
          void queryClient.invalidateQueries({ queryKey: chatQueryKeys.sessions })
        })

        // Send using the new thread id — gateway can still resolve/reroute under the hood
        // Fire send BEFORE navigate — navigating unmounts the component and can cancel the fetch
        sendMessage(
          threadId,
          threadId,
          trimmedBody,
          attachmentPayload,
          true,
          typeof optimisticMessage.clientId === 'string'
            ? optimisticMessage.clientId
            : '',
        )
        // Navigate after send is fired (fetch is in-flight, won't be cancelled)
        navigate({
          to: '/chat/$sessionKey',
          params: { sessionKey: threadId },
          replace: true,
        })
        return
      }

      const sessionKeyForSend =
        forcedSessionKey || resolvedSessionKey || activeSessionKey || 'main'
      sendMessage(
        sessionKeyForSend,
        activeFriendlyId,
        trimmedBody,
        attachmentPayload,
      )
    },
    [
      activeFriendlyId,
      activeSessionKey,
      createSessionForMessage,
      forcedSessionKey,
      isNewChat,
      navigate,
      onSessionResolved,
      upsertSessionInCache,
      queryClient,
      resolvedSessionKey,
    ],
  )

  const toggleSidebar = useWorkspaceStore((s) => s.toggleSidebar)

  const handleToggleSidebarCollapse = useCallback(() => {
    toggleSidebar()
  }, [toggleSidebar])

  const handleToggleFileExplorer = useCallback(() => {
    setFileExplorerCollapsed((prev) => {
      const next = !prev
      if (typeof window !== 'undefined') {
        localStorage.setItem('clawsuite-file-explorer-collapsed', String(next))
      }
      return next
    })
  }, [])

  useEffect(() => {
    function handleToggleFileExplorerFromSearch() {
      handleToggleFileExplorer()
    }

    window.addEventListener(
      SEARCH_MODAL_EVENTS.TOGGLE_FILE_EXPLORER,
      handleToggleFileExplorerFromSearch,
    )
    window.addEventListener(SIDEBAR_TOGGLE_EVENT, handleToggleSidebarCollapse)
    return () => {
      window.removeEventListener(
        SEARCH_MODAL_EVENTS.TOGGLE_FILE_EXPLORER,
        handleToggleFileExplorerFromSearch,
      )
      window.removeEventListener(
        SIDEBAR_TOGGLE_EVENT,
        handleToggleSidebarCollapse,
      )
    }
  }, [handleToggleFileExplorer, handleToggleSidebarCollapse])

  const handleInsertFileReference = useCallback((reference: string) => {
    composerHandleRef.current?.insertText(reference)
  }, [])

  const historyLoading =
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
    (historyQuery.isLoading && !historyQuery.data) || isRedirecting
  const historyEmpty = !historyLoading && finalDisplayMessages.length === 0
  const gatewayNotice = useMemo(() => {
    if (!showErrorNotice) return null
    if (!gatewayError) return null
    return (
      <GatewayStatusMessage
        state="error"
        error={gatewayError}
        onRetry={handleGatewayRefetch}
      />
    )
  }, [gatewayError, handleGatewayRefetch, showErrorNotice])

  const mobileHeaderStatus: 'connected' | 'connecting' | 'disconnected' =
    connectionState === 'connected'
      ? 'connected'
      : gatewayStatusQuery.data?.ok === false || gatewayStatusQuery.isError
        ? 'disconnected'
        : 'connecting'

  // Pull-to-refresh offset removed

  const handleOpenAgentDetails = useCallback(() => {
    setAgentViewOpen(true)
  }, [setAgentViewOpen])

  // Listen for mobile header agent-details tap
  useEffect(() => {
    const handler = () => setAgentViewOpen(true)
    window.addEventListener('clawsuite:chat-agent-details', handler)
    return () => window.removeEventListener('clawsuite:chat-agent-details', handler)
  }, [setAgentViewOpen])

  return (
    <div
      className={cn(
        'relative min-w-0 flex flex-col overflow-hidden',
        compact ? 'flex-1 min-h-0' : 'h-full',
      )}
    >
      <div
        className={cn(
          'flex-1 min-h-0 overflow-hidden',
          compact
            ? 'flex flex-col w-full'
            : isMobile
              ? 'flex flex-col'
              : 'grid grid-cols-[auto_1fr] grid-rows-[minmax(0,1fr)]',
        )}
      >
        {hideUi || compact ? null : isMobile ? null : (
          <FileExplorerSidebar
            collapsed={fileExplorerCollapsed}
            onToggle={handleToggleFileExplorer}
            onInsertReference={handleInsertFileReference}
          />
        )}

        <main
          className={cn(
            'flex h-full flex-1 min-h-0 min-w-0 flex-col overflow-hidden transition-[margin-right,margin-bottom] duration-200',
            !compact && isAgentViewOpen ? 'min-[1024px]:mr-80' : 'mr-0',
          )}
          style={{
            marginBottom:
              terminalPanelInset > 0 ? `${terminalPanelInset}px` : undefined,
          }}
          ref={mainRef}
        >
          {!compact && (
            <ChatHeader
              activeTitle={activeTitle}
              wrapperRef={headerRef}
              onOpenSessions={() => setSessionsOpen(true)}
              showFileExplorerButton={!isMobile}
              fileExplorerCollapsed={fileExplorerCollapsed}
              onToggleFileExplorer={handleToggleFileExplorer}
              dataUpdatedAt={historyQuery.dataUpdatedAt}
              onRefresh={handleRefreshHistory}
              agentModel={currentModel}
              agentConnected={mobileHeaderStatus === 'connected'}
              onOpenAgentDetails={handleOpenAgentDetails}
              pullOffset={0}
            />
          )}

          <ContextBar compact={compact} />

          {gatewayNotice && <div className="sticky top-0 z-20 px-4 py-2">{gatewayNotice}</div>}

          {hideUi ? null : (
            <ChatMessageList
              messages={finalDisplayMessages}
              onRetryMessage={handleRetryMessage}
              onRefresh={handleRefreshHistory}
              loading={historyLoading}
              empty={historyEmpty}
              emptyState={
                <ChatEmptyState
                  compact={compact}
                  onSuggestionClick={(prompt) => {
                    composerHandleRef.current?.setValue(prompt + ' ')
                  }}
                />
              }
              notice={null}
              noticePosition="end"
              waitingForResponse={waitingForResponse}
              sessionKey={activeCanonicalKey}
              pinToTop={false}
              pinGroupMinHeight={pinGroupMinHeight}
              headerHeight={headerHeight}
              contentStyle={stableContentStyle}
              bottomOffset={isMobile ? mobileScrollBottomOffset : terminalPanelInset}
              keyboardInset={mobileKeyboardInset}
              isStreaming={derivedStreamingInfo.isStreaming}
              streamingMessageId={derivedStreamingInfo.streamingMessageId}
              streamingText={realtimeStreamingText || undefined}
              streamingThinking={realtimeStreamingThinking || undefined}
              hideSystemMessages={isMobile}
              activeToolCalls={activeToolCalls}
            />
          )}
          {showComposer ? (
            <ChatComposer
              onSubmit={send}
              isLoading={sending}
              disabled={sending || hideUi}
              sessionKey={
                isNewChat
                  ? undefined
                  : forcedSessionKey || resolvedSessionKey || activeSessionKey
              }
              wrapperRef={composerRef}
              composerRef={composerHandleRef}
              // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
              focusKey={`${isNewChat ? 'new' : activeFriendlyId}:${activeCanonicalKey ?? ''}`}
            />
          ) : null}
        </main>
        {!compact && <AgentViewPanel />}
      </div>
      {!compact && !hideUi && !isMobile && <TerminalPanel />}

      {suggestion && (
        <ModelSuggestionToast
          suggestedModel={suggestion.suggestedModel}
          reason={suggestion.reason}
          costImpact={suggestion.costImpact}
          onSwitch={handleSwitchModel}
          onDismiss={dismiss}
          onDismissForSession={dismissForSession}
        />
      )}

      {isMobile && (
        <MobileSessionsPanel
          open={sessionsOpen}
          onClose={() => setSessionsOpen(false)}
          sessions={sessions}
          activeFriendlyId={activeFriendlyId}
          onSelectSession={(friendlyId) => {
            setSessionsOpen(false)
            void navigate({ to: '/chat/$sessionKey', params: { sessionKey: friendlyId } })
          }}
          onNewChat={() => {
            setSessionsOpen(false)
            void navigate({ to: '/chat/$sessionKey', params: { sessionKey: 'new' } })
          }}
        />
      )}
    </div>
  )
}
