import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'motion/react'
import { AgentChatHeader } from './AgentChatHeader'
import { AgentChatInput } from './AgentChatInput'
import { AgentChatMessages } from './AgentChatMessages'
import type { AgentChatMessage } from './AgentChatMessages'
import type { GatewayMessage } from '@/screens/chat/types'
import { DialogContent, DialogRoot } from '@/components/ui/dialog'
import {
  getMessageTimestamp,
  readError,
  textFromMessage,
} from '@/screens/chat/utils'

type AgentChatModalProps = {
  open: boolean
  sessionKey: string
  agentName: string
  statusLabel: string
  onOpenChange: (open: boolean) => void
}

type HistoryPayload = {
  messages?: Array<GatewayMessage>
}

function mapRole(value: unknown): 'user' | 'agent' {
  const role = typeof value === 'string' ? value.trim().toLowerCase() : ''
  if (role === 'user') return 'user'
  return 'agent'
}

function readMessageText(message: GatewayMessage): string {
  const fromContent = textFromMessage(message)
  if (fromContent.length > 0) return fromContent

  const directText = (message as Record<string, unknown>).text
  if (typeof directText === 'string' && directText.trim().length > 0) {
    return directText.trim()
  }

  return ''
}

function toChatMessages(
  messages: Array<GatewayMessage>,
): Array<AgentChatMessage> {
  return messages
    .map(function mapMessage(message, index) {
      const text = readMessageText(message)
      if (!text) return null

      const rawId = (message as Record<string, unknown>).id
      const id =
        typeof rawId === 'string' && rawId.trim().length > 0
          ? rawId.trim()
          : `history-${index}-${getMessageTimestamp(message)}`

      return {
        id,
        role: mapRole(message.role),
        text,
        timestamp: getMessageTimestamp(message),
      } satisfies AgentChatMessage
    })
    .filter(function filterNull(message): message is AgentChatMessage {
      return message !== null
    })
}

function buildDemoReply(agentName: string, text: string): string {
  return `${agentName} (demo): Received "${text}". Gateway is unavailable, so this is a simulated response.`
}

export function AgentChatModal({
  open,
  sessionKey,
  agentName,
  statusLabel,
  onOpenChange,
}: AgentChatModalProps) {
  const [messages, setMessages] = useState<Array<AgentChatMessage>>([])
  const [isLoadingHistory, setIsLoadingHistory] = useState(false)
  const [isSending, setIsSending] = useState(false)
  const [isTyping, setIsTyping] = useState(false)
  const [isDemoMode, setIsDemoMode] = useState(false)
  const [errorMessage, setErrorMessage] = useState<string | null>(null)

  const typingExpectedAgentCountRef = useRef<number | null>(null)
  const messagesRef = useRef<Array<AgentChatMessage>>([])
  const isDemoModeRef = useRef(false)

  // Keep refs in sync with state
  messagesRef.current = messages
  isDemoModeRef.current = isDemoMode

  const agentMessageCount = useMemo(
    function getAgentMessageCount() {
      return messages.filter(function onlyAgent(message) {
        return message.role === 'agent'
      }).length
    },
    [messages],
  )

  const loadHistory = useCallback(
    async function loadHistory() {
      if (!open || isDemoModeRef.current) return

      try {
        setIsLoadingHistory((current) =>
          messagesRef.current.length === 0 ? true : current,
        )
        const query = new URLSearchParams({ sessionKey, limit: '150' })
        const response = await fetch(`/api/history?${query.toString()}`)
        if (!response.ok) {
          throw new Error(await readError(response))
        }

        const payload = (await response.json()) as HistoryPayload
        const nextMessages = Array.isArray(payload.messages)
          ? toChatMessages(payload.messages)
          : []

        setMessages(nextMessages)
        setErrorMessage(null)

        const expectedAgentCount = typingExpectedAgentCountRef.current
        if (
          expectedAgentCount !== null &&
          nextMessages.filter(function onlyAgent(message) {
            return message.role === 'agent'
          }).length >= expectedAgentCount
        ) {
          setIsTyping(false)
          typingExpectedAgentCountRef.current = null
        }
      } catch (error) {
        const message =
          error instanceof Error ? error.message : 'Unable to load chat history'
        setErrorMessage(message)
        if (messagesRef.current.length === 0) {
          setIsDemoMode(true)
          setMessages([
            {
              id: `demo-intro-${sessionKey}`,
              role: 'agent',
              text: 'Gateway unavailable. Running in demo mode with simulated responses.',
              timestamp: Date.now(),
            },
          ])
        }
      } finally {
        setIsLoadingHistory(false)
      }
    },
    [open, sessionKey],
  )

  // Stable ref for loadHistory to avoid effect dependency loops
  const loadHistoryRef = useRef(loadHistory)
  loadHistoryRef.current = loadHistory

  useEffect(
    function handleEscapeToClose() {
      if (!open) return

      function onKeyDown(event: KeyboardEvent) {
        if (event.key === 'Escape') {
          event.preventDefault()
          onOpenChange(false)
        }
      }

      window.addEventListener('keydown', onKeyDown)
      return function cleanupEscapeToClose() {
        window.removeEventListener('keydown', onKeyDown)
      }
    },
    [onOpenChange, open],
  )

  useEffect(
    function initializeModalState() {
      if (!open) return
      setMessages([])
      setIsLoadingHistory(true)
      setIsSending(false)
      setIsTyping(false)
      setIsDemoMode(false)
      setErrorMessage(null)
      typingExpectedAgentCountRef.current = null
      void loadHistoryRef.current()
    },
    [open, sessionKey],
  )

  useEffect(
    function pollLiveHistory() {
      if (!open || isDemoMode) return

      const timer = window.setInterval(function refreshHistory() {
        void loadHistoryRef.current()
      }, 2000)

      return function cleanupLivePoll() {
        window.clearInterval(timer)
      }
    },
    [isDemoMode, open],
  )

  function sendDemoReply(text: string) {
    window.setTimeout(function deliverDemoReply() {
      setMessages(function appendDemoReply(previous) {
        return [
          ...previous,
          {
            id: `demo-reply-${crypto.randomUUID()}`,
            role: 'agent',
            text: buildDemoReply(agentName, text),
            timestamp: Date.now(),
          },
        ]
      })
      setIsTyping(false)
      setIsSending(false)
      typingExpectedAgentCountRef.current = null
    }, 2000)
  }

  async function handleSend(message: string) {
    const sentAt = Date.now()

    const optimisticId = `local-user-${crypto.randomUUID()}`
    setMessages(function appendOptimisticUser(previous) {
      return [
        ...previous,
        {
          id: optimisticId,
          role: 'user',
          text: message,
          timestamp: sentAt,
          status: 'sending',
        },
      ]
    })

    setIsSending(true)
    setIsTyping(true)
    setErrorMessage(null)
    typingExpectedAgentCountRef.current = agentMessageCount + 1

    if (isDemoMode) {
      setMessages(function markSent(previous) {
        return previous.map(function mapMessage(entry) {
          if (entry.id !== optimisticId) return entry
          return { ...entry, status: undefined }
        })
      })
      sendDemoReply(message)
      return
    }

    try {
      const response = await fetch('/api/sessions/send', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ sessionKey, message }),
      })

      if (!response.ok) {
        throw new Error(await readError(response))
      }

      setMessages(function markSent(previous) {
        return previous.map(function mapMessage(entry) {
          if (entry.id !== optimisticId) return entry
          return { ...entry, status: undefined }
        })
      })

      await loadHistory()
    } catch (error) {
      const messageText =
        error instanceof Error ? error.message : 'Unable to send message'

      setMessages(function markFailed(previous) {
        return previous.map(function mapMessage(entry) {
          if (entry.id !== optimisticId) return entry
          return { ...entry, status: 'error' }
        })
      })

      setErrorMessage(messageText)
      setIsDemoMode(true)
      typingExpectedAgentCountRef.current = null
      sendDemoReply(message)
      return
    }

    setIsSending(false)
  }

  return (
    <DialogRoot open={open} onOpenChange={onOpenChange}>
      <DialogContent className="h-[min(85vh,720px)] w-[min(860px,96vw)] overflow-hidden rounded-3xl border border-primary-300/70 bg-primary-100/55 p-0 backdrop-blur-xl max-md:bottom-0 max-md:left-0 max-md:h-[90dvh] max-md:w-screen max-md:translate-x-0 max-md:translate-y-0 max-md:rounded-b-none max-md:rounded-t-3xl max-md:top-auto z-50">
        <motion.div
          initial={false}
          animate={{ opacity: 1 }}
          className="flex h-full flex-col overflow-hidden"
        >
          <AgentChatHeader
            agentName={agentName}
            statusLabel={statusLabel}
            isDemoMode={isDemoMode}
            onClose={function handleClose() {
              onOpenChange(false)
            }}
          />

          <div className="min-h-0 flex-1 overflow-y-auto bg-linear-to-b from-primary-100/55 via-primary-100/45 to-primary-200/35">
            <AgentChatMessages
              messages={messages}
              isLoading={isLoadingHistory}
              isTyping={isTyping}
            />
          </div>

          <AnimatePresence initial={false}>
            {errorMessage ? (
              <motion.p
                key={errorMessage}
                initial={{ opacity: 0, y: 4 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -4 }}
                className="border-t border-primary-300/70 bg-primary-100/70 px-4 py-2 text-xs text-pretty text-red-300"
              >
                {errorMessage}
              </motion.p>
            ) : null}
          </AnimatePresence>

          <AgentChatInput
            disabled={isLoadingHistory}
            isSending={isSending}
            onSend={handleSend}
          />
        </motion.div>
      </DialogContent>
    </DialogRoot>
  )
}
