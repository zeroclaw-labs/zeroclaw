/**
 * ChatPanel — collapsible right-panel chat overlay for non-chat routes.
 * Renders a full ChatScreen in a side panel so users can chat while
 * viewing dashboard, skills, gateway pages, etc.
 */
import { useCallback, useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  ArrowExpand01Icon,
  Cancel01Icon,
  PencilEdit02Icon,
} from '@hugeicons/core-free-icons'
import { motion, AnimatePresence } from 'motion/react'
import { ChatScreen } from '@/screens/chat/chat-screen'
import { chatQueryKeys, moveHistoryMessages } from '@/screens/chat/chat-queries'
import { useWorkspaceStore } from '@/stores/workspace-store'
import { Button } from '@/components/ui/button'
import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import type { SessionMeta } from '@/screens/chat/types'

export function ChatPanel() {
  const isOpen = useWorkspaceStore((s) => s.chatPanelOpen)
  const sessionKey = useWorkspaceStore((s) => s.chatPanelSessionKey)
  const setChatPanelOpen = useWorkspaceStore((s) => s.setChatPanelOpen)
  const setChatPanelSessionKey = useWorkspaceStore(
    (s) => s.setChatPanelSessionKey,
  )
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const [forcedSession, setForcedSession] = useState<{
    friendlyId: string
    sessionKey: string
  } | null>(null)

  const isNewChat = sessionKey === 'new'
  const activeFriendlyId = sessionKey || 'main'
  const forcedSessionKey =
    forcedSession?.friendlyId === activeFriendlyId
      ? forcedSession.sessionKey
      : undefined

  // Session list for the dropdown
  const sessionsQuery = useQuery({
    queryKey: chatQueryKeys.sessions,
    queryFn: async () => {
      const res = await fetch('/api/sessions')
      if (!res.ok) return []
      const data = await res.json()
      return Array.isArray(data?.sessions)
        ? data.sessions
        : Array.isArray(data)
          ? data
          : []
    },
    staleTime: 10_000,
  })
  const sessions: SessionMeta[] = sessionsQuery.data ?? []

  // Current session title
  const activeSession = sessions.find((s) => s.friendlyId === activeFriendlyId)
  const panelTitle = activeSession
    ? activeSession.label ||
      activeSession.title ||
      activeSession.derivedTitle ||
      'Chat'
    : activeFriendlyId === 'main'
      ? 'Main Session'
      : isNewChat
        ? 'New Chat'
        : 'Chat'

  const handleSessionResolved = useCallback(
    (payload: { friendlyId: string; sessionKey: string }) => {
      moveHistoryMessages(
        queryClient,
        'new',
        'new',
        payload.friendlyId,
        payload.sessionKey,
      )
      setForcedSession({
        friendlyId: payload.friendlyId,
        sessionKey: payload.sessionKey,
      })
      setChatPanelSessionKey(payload.friendlyId)
    },
    [queryClient, setChatPanelSessionKey],
  )

  const handleExpand = useCallback(() => {
    setChatPanelOpen(false)
    navigate({
      to: '/chat/$sessionKey',
      params: { sessionKey: activeFriendlyId },
    })
  }, [activeFriendlyId, navigate, setChatPanelOpen])

  const handleClose = useCallback(() => {
    setChatPanelOpen(false)
  }, [setChatPanelOpen])

  const handleNewChat = useCallback(() => {
    setForcedSession(null)
    setChatPanelSessionKey('new')
  }, [setChatPanelSessionKey])

  const handleSelectSession = useCallback(
    (friendlyId: string) => {
      setForcedSession(null)
      setChatPanelSessionKey(friendlyId)
    },
    [setChatPanelSessionKey],
  )

  // Simple dropdown state
  const [showSessionList, setShowSessionList] = useState(false)

  return (
    <AnimatePresence>
      {isOpen && (
        <>
          {/* Backdrop for narrow screens */}
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.15 }}
            className="fixed inset-0 bg-black/20 z-10 min-[1200px]:hidden"
            onClick={handleClose}
            aria-hidden
          />
          <motion.div
            initial={{ x: '100%', opacity: 0 }}
            animate={{ x: 0, opacity: 1 }}
            exit={{ x: '100%', opacity: 0 }}
            transition={{ duration: 0.2, ease: [0.4, 0, 0.2, 1] }}
            className="fixed right-0 top-0 h-full w-[420px] max-w-[100vw] border-l border-primary-200 bg-surface overflow-hidden flex flex-col z-20 shadow-xl"
          >
            {/* Panel header */}
            <div className="flex items-center justify-between h-10 px-3 border-b border-primary-200 shrink-0">
              <div className="flex items-center gap-1.5 min-w-0">
                <button
                  type="button"
                  onClick={() => setShowSessionList((v) => !v)}
                  className="text-xs font-medium text-primary-700 hover:text-primary-900 truncate max-w-[200px] transition-colors"
                  title={panelTitle}
                >
                  {panelTitle}
                </button>
              </div>
              <div className="flex items-center gap-0.5">
                <TooltipProvider>
                  <TooltipRoot>
                    <TooltipTrigger
                      onClick={handleNewChat}
                      render={
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          className="text-primary-600 hover:text-primary-900"
                          aria-label="New chat"
                        >
                          <HugeiconsIcon
                            icon={PencilEdit02Icon}
                            size={14}
                            strokeWidth={1.5}
                          />
                        </Button>
                      }
                    />
                    <TooltipContent side="bottom">New chat</TooltipContent>
                  </TooltipRoot>
                  <TooltipRoot>
                    <TooltipTrigger
                      onClick={handleExpand}
                      render={
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          className="text-primary-600 hover:text-primary-900"
                          aria-label="Expand to full chat"
                        >
                          <HugeiconsIcon
                            icon={ArrowExpand01Icon}
                            size={14}
                            strokeWidth={1.5}
                          />
                        </Button>
                      }
                    />
                    <TooltipContent side="bottom">Full view</TooltipContent>
                  </TooltipRoot>
                </TooltipProvider>
                <Button
                  size="icon-sm"
                  variant="ghost"
                  onClick={handleClose}
                  className="text-primary-600 hover:text-primary-900"
                  aria-label="Close chat panel"
                >
                  <HugeiconsIcon
                    icon={Cancel01Icon}
                    size={14}
                    strokeWidth={1.5}
                  />
                </Button>
              </div>
            </div>

            {/* Session switcher dropdown */}
            <AnimatePresence>
              {showSessionList && (
                <motion.div
                  initial={{ height: 0, opacity: 0 }}
                  animate={{ height: 'auto', opacity: 1 }}
                  exit={{ height: 0, opacity: 0 }}
                  transition={{ duration: 0.15 }}
                  className="border-b border-primary-200 overflow-hidden"
                >
                  <div className="max-h-48 overflow-y-auto py-1">
                    {sessions.map((s) => (
                      <button
                        key={s.key}
                        type="button"
                        onClick={() => {
                          handleSelectSession(s.friendlyId)
                          setShowSessionList(false)
                        }}
                        className={`w-full text-left px-3 py-1.5 text-xs truncate transition-colors ${
                          s.friendlyId === activeFriendlyId
                            ? 'bg-accent-500/10 text-accent-600'
                            : 'text-primary-700 hover:bg-primary-100'
                        }`}
                      >
                        {s.label || s.title || s.derivedTitle || s.friendlyId}
                      </button>
                    ))}
                  </div>
                </motion.div>
              )}
            </AnimatePresence>

            {/* Chat content */}
            <div className="flex-1 min-h-0 overflow-hidden relative">
              <ChatScreen
                key={activeFriendlyId}
                activeFriendlyId={activeFriendlyId}
                isNewChat={isNewChat}
                forcedSessionKey={forcedSessionKey}
                onSessionResolved={
                  isNewChat ? handleSessionResolved : undefined
                }
                compact
              />
            </div>
          </motion.div>
        </>
      )}
    </AnimatePresence>
  )
}
