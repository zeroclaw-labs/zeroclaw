import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'

import { chatQueryKeys, fetchSessions } from '../chat-queries'
import { isRecentSession } from '../pending-send'
import { filterSessionsWithTombstones } from '../session-tombstones'
import { useSessionTitles } from '../session-title-store'
import type { SessionTitleInfo } from '../session-title-store'
import type { SessionMeta } from '../types'

function mergeSessionTitle(
  session: SessionMeta,
  stored: SessionTitleInfo | undefined,
): SessionMeta {
  if (!stored) return session

  const hasManualTitle = Boolean(session.label || session.title)
  const derivedTitle = hasManualTitle
    ? session.derivedTitle
    : (stored.title ?? session.derivedTitle)
  // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
  const titleStatus = stored.status ?? session.titleStatus
  const titleSource = hasManualTitle
    ? 'manual'
    : (stored.source ?? session.titleSource)
  const titleError = stored.error ?? session.titleError

  return {
    ...session,
    derivedTitle,
    titleStatus,
    titleSource,
    titleError,
  }
}

type UseChatSessionsInput = {
  activeFriendlyId: string
  isNewChat: boolean
  forcedSessionKey?: string
}

export function useChatSessions({
  activeFriendlyId,
  isNewChat,
  forcedSessionKey,
}: UseChatSessionsInput) {
  const sessionsQuery = useQuery({
    queryKey: chatQueryKeys.sessions,
    queryFn: fetchSessions,
    refetchInterval: 30000,
  })
  const storedTitles = useSessionTitles()

  const sessions = useMemo(() => {
    const rawSessions = sessionsQuery.data ?? []
    const filtered = filterSessionsWithTombstones(rawSessions)
    if (!filtered.length) return filtered
    return filtered.map((session) =>
      mergeSessionTitle(session, storedTitles[session.friendlyId]),
    )
  }, [sessionsQuery.data, storedTitles])

  const activeSession = useMemo(() => {
    return sessions.find((session) => session.friendlyId === activeFriendlyId)
  }, [sessions, activeFriendlyId])
  const activeExists = useMemo(() => {
    if (isNewChat) return true
    if (forcedSessionKey) return true
    if (isRecentSession(activeFriendlyId)) return true
    return sessions.some((session) => session.friendlyId === activeFriendlyId)
  }, [activeFriendlyId, forcedSessionKey, isNewChat, sessions])
  const activeSessionKey = activeSession?.key ?? ''
  const activeTitle = useMemo(() => {
    if (activeSession) {
      if (activeSession.label) return activeSession.label
      if (activeSession.title) return activeSession.title
      if (activeSession.derivedTitle) return activeSession.derivedTitle
      if (activeSession.titleStatus === 'generating') return 'Naming…'
      if (activeSession.titleStatus === 'error') return 'New Session'
      return 'New Session'
    }
    return activeFriendlyId === 'main' ? 'ClawSuite' : activeFriendlyId
  }, [activeFriendlyId, activeSession])

  const sessionsError =
    sessionsQuery.error instanceof Error ? sessionsQuery.error.message : null
  const sessionsLoading = sessionsQuery.isLoading && !sessionsQuery.data
  const sessionsFetching = sessionsQuery.isFetching

  return {
    sessionsQuery,
    sessions,
    activeSession,
    activeExists,
    activeSessionKey,
    activeTitle,
    sessionsError,
    sessionsLoading,
    sessionsFetching,
    refetchSessions: sessionsQuery.refetch,
  }
}
