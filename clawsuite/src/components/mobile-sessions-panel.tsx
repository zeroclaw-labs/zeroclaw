import { useEffect } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import { Add01Icon, Chat01Icon } from '@hugeicons/core-free-icons'
import type { SessionMeta } from '@/screens/chat/types'
import { cn } from '@/lib/utils'

type Props = {
  open: boolean
  onClose: () => void
  sessions: Array<SessionMeta>
  activeFriendlyId: string
  onSelectSession: (key: string) => void
  onNewChat: () => void
}

function normalizeLabel(value: string | undefined): string {
  if (typeof value !== 'string') return ''
  const trimmed = value.trim()
  return trimmed.length > 0 ? trimmed : ''
}

function getSessionTitle(session: SessionMeta): string {
  const label = normalizeLabel(session.label)
  if (label) return label
  const derivedTitle = normalizeLabel(session.derivedTitle)
  if (derivedTitle) return derivedTitle
  const title = normalizeLabel(session.title)
  if (title) return title
  return `Session ${session.friendlyId.slice(0, 8)}`
}

const dayFormatter = new Intl.DateTimeFormat(undefined, {
  month: 'short',
  day: 'numeric',
})

const timeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: 'numeric',
  minute: '2-digit',
})

function formatUpdatedAt(updatedAt?: number): string {
  if (typeof updatedAt !== 'number') return ''
  const value = new Date(updatedAt)
  const now = new Date()
  if (value.toDateString() === now.toDateString()) {
    return timeFormatter.format(value)
  }
  return dayFormatter.format(value)
}

export function MobileSessionsPanel({
  open,
  onClose,
  sessions,
  activeFriendlyId,
  onSelectSession,
  onNewChat,
}: Props) {
  useEffect(() => {
    if (!open) return
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        onClose()
      }
    }
    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [open, onClose])

  if (!open) return null

  return (
    <div className="fixed inset-0 z-[85] no-swipe md:hidden">
      <button
        type="button"
        className="absolute inset-0 bg-black/40 animate-in fade-in duration-200"
        aria-label="Close sessions panel"
        onClick={onClose}
      />

      <aside className="no-swipe absolute inset-y-0 left-0 w-[80vw] max-w-sm border-r border-primary-200 bg-white shadow-2xl animate-in slide-in-from-left-8 duration-200 dark:border-gray-700 dark:bg-gray-900">
        <div className="flex h-full flex-col">
          <div className="flex items-center justify-between border-b border-primary-200 px-4 py-3 dark:border-gray-700">
            <h2 className="text-sm font-semibold text-ink">Sessions</h2>
            <button
              type="button"
              onClick={onNewChat}
              className="inline-flex items-center gap-1 rounded-lg border border-primary-200 bg-primary-50 px-2.5 py-1.5 text-xs font-medium text-primary-700 transition-colors hover:border-accent-200 hover:text-accent-600"
            >
              <HugeiconsIcon icon={Add01Icon} size={14} strokeWidth={1.8} />
              New Chat
            </button>
          </div>

          <div className="flex-1 overflow-y-auto p-2">
            {sessions.length === 0 ? (
              <div className="flex h-full flex-col items-center justify-center gap-2 px-3 text-center text-primary-500">
                <HugeiconsIcon icon={Chat01Icon} size={24} strokeWidth={1.6} />
                <p className="text-sm">No sessions yet.</p>
              </div>
            ) : (
              <div className="space-y-1">
                {sessions.map((session) => {
                  const active = session.friendlyId === activeFriendlyId
                  const timestamp = formatUpdatedAt(session.updatedAt)
                  return (
                    <button
                      key={session.key}
                      type="button"
                      onClick={() => onSelectSession(session.friendlyId)}
                      className={cn(
                        'w-full rounded-lg border px-3 py-2 text-left transition-colors',
                        active
                          ? 'border-accent-300 bg-accent-50'
                          : 'border-transparent bg-primary-50 hover:border-primary-200',
                      )}
                    >
                      <div className="truncate text-sm font-medium text-ink">
                        {getSessionTitle(session)}
                      </div>
                      <div className="mt-0.5 flex items-center justify-between gap-2 text-[11px] text-primary-500">
                        <span className="truncate">{session.friendlyId}</span>
                        {timestamp ? <span>{timestamp}</span> : null}
                      </div>
                    </button>
                  )
                })}
              </div>
            )}
          </div>
        </div>
      </aside>
    </div>
  )
}
