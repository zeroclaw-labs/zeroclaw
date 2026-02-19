import { normalizeSessions, readError } from './utils'
import type { QueryClient } from '@tanstack/react-query'
import type {
  GatewayMessage,
  HistoryResponse,
  SessionListResponse,
  SessionMeta,
} from './types'

type GatewayStatusResponse = {
  ok: boolean
  error?: string
}

function normalizeId(value: unknown): string {
  if (typeof value !== 'string') return ''
  const trimmed = value.trim()
  return trimmed.length > 0 ? trimmed : ''
}

function getMessageClientId(message: GatewayMessage): string {
  const raw = message as Record<string, unknown>
  const candidates = [raw.clientId, raw.client_id]
  for (const candidate of candidates) {
    const normalized = normalizeId(candidate)
    if (normalized) return normalized
  }
  return ''
}

function getMessageOptimisticId(message: GatewayMessage): string {
  return normalizeId(message.__optimisticId)
}

function isMatchingClientMessage(
  message: GatewayMessage,
  clientId: string,
  optimisticId: string,
): boolean {
  const messageClientId = getMessageClientId(message)
  if (messageClientId === clientId) return true

  const messageOptimisticId = getMessageOptimisticId(message)
  if (!messageOptimisticId) return false
  if (messageOptimisticId === clientId) return true
  if (messageOptimisticId === optimisticId) return true
  return false
}

export const chatQueryKeys = {
  sessions: ['chat', 'sessions'] as const,
  history: function history(friendlyId: string, sessionKey: string) {
    return ['chat', 'history', friendlyId, sessionKey] as const
  },
} as const

export async function fetchSessions(): Promise<Array<SessionMeta>> {
  const res = await fetch('/api/sessions')
  if (!res.ok) throw new Error(await readError(res))
  const data = (await res.json()) as SessionListResponse
  return normalizeSessions(data.sessions)
}

export async function fetchHistory(payload: {
  sessionKey: string
  friendlyId: string
}): Promise<HistoryResponse> {
  const query = new URLSearchParams({ limit: '1000' })
  if (payload.sessionKey) query.set('sessionKey', payload.sessionKey)
  if (payload.friendlyId) query.set('friendlyId', payload.friendlyId)
  const res = await fetch(`/api/history?${query.toString()}`)
  if (!res.ok) throw new Error(await readError(res))
  return (await res.json()) as HistoryResponse
}

export async function fetchGatewayStatus(): Promise<GatewayStatusResponse> {
  const controller = new AbortController()
  const timeout = window.setTimeout(() => controller.abort(), 5000)

  try {
    const res = await fetch('/api/ping', { signal: controller.signal })
    if (!res.ok) throw new Error(await readError(res))
    return (await res.json()) as GatewayStatusResponse
  } catch (err) {
    if (err instanceof DOMException && err.name === 'AbortError') {
      throw new Error('Gateway check timed out')
    }
    throw err
  } finally {
    window.clearTimeout(timeout)
  }
}

export function updateHistoryMessages(
  queryClient: QueryClient,
  friendlyId: string,
  sessionKey: string,
  updater: (messages: Array<GatewayMessage>) => Array<GatewayMessage>,
) {
  const queryKey = chatQueryKeys.history(friendlyId, sessionKey)
  queryClient.setQueryData(queryKey, function update(data: unknown) {
    const current = data as HistoryResponse | undefined
    const messages = Array.isArray(current?.messages) ? current.messages : []
    const nextMessages = updater(messages)
    return {
      sessionKey: current?.sessionKey ?? sessionKey,
      sessionId: current?.sessionId,
      messages: nextMessages,
    }
  })
}

export function appendHistoryMessage(
  queryClient: QueryClient,
  friendlyId: string,
  sessionKey: string,
  message: GatewayMessage,
) {
  updateHistoryMessages(
    queryClient,
    friendlyId,
    sessionKey,
    function append(messages) {
      return [...messages, message]
    },
  )
}

export function updateHistoryMessageByClientId(
  queryClient: QueryClient,
  friendlyId: string,
  sessionKey: string,
  clientId: string,
  updater: (message: GatewayMessage) => GatewayMessage,
) {
  const normalizedClientId = normalizeId(clientId)
  if (!normalizedClientId) return
  const optimisticId = `opt-${normalizedClientId}`
  updateHistoryMessages(
    queryClient,
    friendlyId,
    sessionKey,
    function update(messages) {
      return messages.map((message) => {
        if (
          isMatchingClientMessage(message, normalizedClientId, optimisticId)
        ) {
          return updater(message)
        }
        return message
      })
    },
  )
}

export function removeHistoryMessageByClientId(
  queryClient: QueryClient,
  friendlyId: string,
  sessionKey: string,
  clientId: string,
  optimisticId?: string,
) {
  const normalizedClientId = normalizeId(clientId)
  if (!normalizedClientId) return
  const resolvedOptimisticId =
    normalizeId(optimisticId) || `opt-${normalizedClientId}`

  updateHistoryMessages(
    queryClient,
    friendlyId,
    sessionKey,
    function remove(messages) {
      return messages.filter((message) => {
        return !isMatchingClientMessage(
          message,
          normalizedClientId,
          resolvedOptimisticId,
        )
      })
    },
  )
}

export function clearHistoryMessages(
  queryClient: QueryClient,
  friendlyId: string,
  sessionKey: string,
) {
  const queryKey = chatQueryKeys.history(friendlyId, sessionKey)
  queryClient.setQueryData(queryKey, {
    sessionKey,
    messages: [],
  })
}

export function moveHistoryMessages(
  queryClient: QueryClient,
  fromFriendlyId: string,
  fromSessionKey: string,
  toFriendlyId: string,
  toSessionKey: string,
) {
  const fromKey = chatQueryKeys.history(fromFriendlyId, fromSessionKey)
  const toKey = chatQueryKeys.history(toFriendlyId, toSessionKey)
  const fromData = queryClient.getQueryData(fromKey) as
    | Record<string, unknown>
    | undefined
  if (!fromData) return
  const messages = Array.isArray(fromData.messages) ? fromData.messages : []
  queryClient.setQueryData(toKey, {
    sessionKey: toSessionKey,
    sessionId: (fromData as any).sessionId,
    messages,
  })
  queryClient.removeQueries({ queryKey: fromKey, exact: true })
}

export function updateSessionLastMessage(
  queryClient: QueryClient,
  sessionKey: string,
  friendlyId: string,
  message: GatewayMessage,
) {
  queryClient.setQueryData(
    chatQueryKeys.sessions,
    function update(messages: unknown) {
      if (!Array.isArray(messages)) return messages
      return (messages as Array<SessionMeta>).map((session) => {
        if (session.key !== sessionKey && session.friendlyId !== friendlyId) {
          return session
        }
        return {
          ...session,
          lastMessage: message,
        }
      })
    },
  )
}

export function removeSessionFromCache(
  queryClient: QueryClient,
  sessionKey: string,
  friendlyId: string,
) {
  queryClient.setQueryData(
    chatQueryKeys.sessions,
    function update(messages: unknown) {
      if (!Array.isArray(messages)) return messages
      return (messages as Array<SessionMeta>).filter((session) => {
        return session.key !== sessionKey && session.friendlyId !== friendlyId
      })
    },
  )

  queryClient.removeQueries({
    queryKey: ['chat', 'history', friendlyId],
    exact: false,
  })
  if (sessionKey && sessionKey !== friendlyId) {
    queryClient.removeQueries({
      queryKey: ['chat', 'history', sessionKey],
      exact: false,
    })
  }
}
