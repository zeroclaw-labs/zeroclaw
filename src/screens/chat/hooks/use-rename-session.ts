import { useCallback, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { chatQueryKeys } from '../chat-queries'
import { readError } from '../utils'
import { updateSessionTitleState } from '../session-title-store'

export type RenameSessionResult = {
  renameSession: (
    sessionKey: string,
    friendlyId: string | null,
    newTitle: string,
  ) => Promise<void>
  renaming: boolean
  error: string | null
}

type RenameSessionPayload = {
  sessionKey: string
  friendlyId?: string | null
  newTitle: string
}

export function useRenameSession(): RenameSessionResult {
  const queryClient = useQueryClient()
  const [renaming, setRenaming] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const mutation = useMutation({
    mutationFn: async function renameSessionRequest(
      payload: RenameSessionPayload,
    ) {
      const res = await fetch('/api/sessions', {
        method: 'PATCH',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          sessionKey: payload.sessionKey,
          friendlyId: payload.friendlyId ?? undefined,
          label: payload.newTitle,
        }),
      })
      if (!res.ok) throw new Error(await readError(res))
      return payload
    },
    onMutate: async function onMutate(payload) {
      setError(null)
      await queryClient.cancelQueries({ queryKey: chatQueryKeys.sessions })
      const previousSessions = queryClient.getQueryData(chatQueryKeys.sessions)

      const targetId = payload.friendlyId || payload.sessionKey
      // Optimistically update the session title in cache
      queryClient.setQueryData(
        chatQueryKeys.sessions,
        function update(sessions: unknown) {
          if (!Array.isArray(sessions)) return sessions
          return (sessions as Array<Record<string, unknown>>).map((session) => {
            const key = typeof session.key === 'string' ? session.key : ''
            const friendlyId =
              typeof session.friendlyId === 'string' ? session.friendlyId : ''
            if (key !== payload.sessionKey && friendlyId !== targetId)
              return session
            return {
              ...session,
              label: payload.newTitle,
              title: payload.newTitle,
              derivedTitle: payload.newTitle,
              titleStatus: 'ready',
              titleSource: 'manual',
              titleError: null,
            }
          })
        },
      )

      return { previousSessions, targetId }
    },
    onError: function onError(err, _payload, context) {
      if (context?.previousSessions) {
        queryClient.setQueryData(
          chatQueryKeys.sessions,
          context.previousSessions,
        )
      }
      setError(err instanceof Error ? err.message : String(err))
    },
    onSuccess: function onSuccess(payload) {
      const targetId = payload.friendlyId || payload.sessionKey
      updateSessionTitleState(targetId, {
        title: payload.newTitle,
        source: 'manual',
        status: 'ready',
        error: null,
      })
      // Invalidate to ensure we have the latest data
      queryClient.invalidateQueries({ queryKey: chatQueryKeys.sessions })
    },
    onSettled: function onSettled() {
      setRenaming(false)
    },
  })

  const renameSession = useCallback(
    async (sessionKey: string, friendlyId: string | null, newTitle: string) => {
      if (!sessionKey || !newTitle.trim()) return
      setRenaming(true)
      await mutation.mutateAsync({
        sessionKey,
        friendlyId: friendlyId ?? undefined,
        newTitle: newTitle.trim(),
      })
    },
    [mutation],
  )

  return { renameSession, renaming, error }
}
