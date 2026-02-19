'use client'

import { Link } from '@tanstack/react-router'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  Delete01Icon,
  MoreHorizontalIcon,
  Pen01Icon,
  PinIcon,
} from '@hugeicons/core-free-icons'
import { memo, useMemo } from 'react'
import { getMessageTimestamp } from '../../utils'
import type { SessionMeta } from '../../types'
import { cn } from '@/lib/utils'
import {
  MenuContent,
  MenuItem,
  MenuRoot,
  MenuTrigger,
} from '@/components/ui/menu'

type SessionItemProps = {
  session: SessionMeta
  active: boolean
  isPinned: boolean
  onSelect?: () => void
  onTogglePin: (session: SessionMeta) => void
  onRename: (session: SessionMeta) => void
  onDelete: (session: SessionMeta) => void
}

const dayFormatter = new Intl.DateTimeFormat(undefined, {
  month: 'short',
  day: 'numeric',
})

const timeFormatter = new Intl.DateTimeFormat(undefined, {
  hour: 'numeric',
  minute: '2-digit',
})

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i

function formatSessionTimestamp(timestamp?: number | null): string {
  if (!timestamp) return ''
  const date = new Date(timestamp)
  const now = new Date()
  const sameDay = date.toDateString() === now.toDateString()
  return (sameDay ? timeFormatter : dayFormatter).format(date)
}

function isUuidLike(value: string): boolean {
  return UUID_PATTERN.test(value.trim())
}

function normalizeTitleValue(value: string | undefined): string | null {
  if (typeof value !== 'string') return null
  const trimmed = value.trim()
  if (trimmed.length === 0) return null
  if (isUuidLike(trimmed)) return null
  return trimmed
}

function getSessionShortId(session: SessionMeta): string {
  const candidates = [session.friendlyId, session.key]
  for (const candidate of candidates) {
    if (typeof candidate !== 'string') continue
    const trimmed = candidate.trim()
    if (trimmed.length === 0) continue
    return trimmed.slice(0, 8)
  }
  return ''
}

function getSessionDisplayTitle(
  session: SessionMeta,
  isGenerating: boolean,
): string {
  const label = normalizeTitleValue(session.label)
  if (label) return label

  const derivedTitle = normalizeTitleValue(session.derivedTitle)
  if (derivedTitle) return derivedTitle

  if (isGenerating) return 'Naming…'
  const shortId = getSessionShortId(session)
  return shortId ? `Session ${shortId}` : 'Session'
}

function getFriendlyIdLabel(friendlyId: string): string {
  if (!isUuidLike(friendlyId)) return friendlyId
  return `ID ${friendlyId.slice(0, 8)}`
}

function SessionItemComponent({
  session,
  active,
  isPinned,
  onSelect,
  onTogglePin,
  onRename,
  onDelete,
}: SessionItemProps) {
  const isGenerating = session.titleStatus === 'generating'
  const isError = session.titleStatus === 'error'
  const baseTitle = getSessionDisplayTitle(session, isGenerating)

  const updatedAt = useMemo(() => {
    if (typeof session.updatedAt === 'number') return session.updatedAt
    if (session.lastMessage) return getMessageTimestamp(session.lastMessage)
    return null
  }, [session.lastMessage, session.updatedAt])

  const subtitle = useMemo(() => {
    if (isError) {
      return session.titleError || 'Could not generate a title'
    }
    const parts: Array<string> = []
    const formatted = formatSessionTimestamp(updatedAt)
    if (formatted) parts.push(formatted)
    if (session.friendlyId) parts.push(getFriendlyIdLabel(session.friendlyId))
    return parts.join(' • ')
  }, [isError, session.friendlyId, session.titleError, updatedAt])

  return (
    <Link
      to="/chat/$sessionKey"
      params={{ sessionKey: session.friendlyId }}
      onClick={onSelect}
      className={cn(
        'group inline-flex items-center justify-between',
        'w-full text-left pl-1.5 pr-0.5 h-14 rounded-lg transition-colors duration-0',
        'select-none',
        active
          ? 'bg-primary-200 text-primary-950'
          : 'bg-transparent text-primary-950 [&:hover:not(:has(button:hover))]:bg-primary-200',
      )}
    >
      <div className="flex-1 min-w-0 py-1.5">
        <div
          className={cn(
            'truncate text-sm font-[500]',
            isGenerating ? 'text-primary-700' : '',
          )}
        >
          <span className={cn(isGenerating ? 'animate-pulse' : undefined)}>
            {baseTitle}
          </span>
        </div>
        <div
          className={cn(
            'mt-0.5 text-[11px] text-primary-600 truncate',
            isError ? 'text-red-600' : undefined,
          )}
        >
          {subtitle}
        </div>
      </div>
      <MenuRoot>
        <MenuTrigger
          type="button"
          onClick={(event) => {
            event.preventDefault()
            event.stopPropagation()
          }}
          className={cn(
            'ml-2 inline-flex size-7 items-center justify-center rounded-md text-primary-700',
            'opacity-0 transition-opacity group-hover:opacity-100 hover:bg-primary-200',
            'aria-expanded:opacity-100 aria-expanded:bg-primary-200',
          )}
          aria-label="Session options"
        >
          <HugeiconsIcon
            icon={MoreHorizontalIcon}
            size={20}
            strokeWidth={1.5}
          />
        </MenuTrigger>
        <MenuContent side="bottom" align="end">
          <MenuItem
            onClick={(event) => {
              event.preventDefault()
              event.stopPropagation()
              onTogglePin(session)
            }}
            className="gap-2"
          >
            <HugeiconsIcon icon={PinIcon} size={20} strokeWidth={1.5} />{' '}
            {isPinned ? 'Unpin session' : 'Pin session'}
          </MenuItem>
          <MenuItem
            onClick={(event) => {
              event.preventDefault()
              event.stopPropagation()
              onRename(session)
            }}
            className="gap-2"
          >
            <HugeiconsIcon icon={Pen01Icon} size={20} strokeWidth={1.5} />{' '}
            Rename
          </MenuItem>
          <MenuItem
            onClick={(event) => {
              event.preventDefault()
              event.stopPropagation()
              onDelete(session)
            }}
            className="text-red-700 gap-2 hover:bg-red-50/80 data-highlighted:bg-red-50/80"
          >
            <HugeiconsIcon icon={Delete01Icon} size={20} strokeWidth={1.5} />{' '}
            Delete
          </MenuItem>
        </MenuContent>
      </MenuRoot>
    </Link>
  )
}

function areSessionItemsEqual(prev: SessionItemProps, next: SessionItemProps) {
  if (prev.active !== next.active) return false
  if (prev.isPinned !== next.isPinned) return false
  if (prev.onSelect !== next.onSelect) return false
  if (prev.onTogglePin !== next.onTogglePin) return false
  if (prev.onRename !== next.onRename) return false
  if (prev.onDelete !== next.onDelete) return false
  if (prev.session === next.session) return true
  return (
    prev.session.key === next.session.key &&
    prev.session.friendlyId === next.session.friendlyId &&
    prev.session.label === next.session.label &&
    prev.session.title === next.session.title &&
    prev.session.derivedTitle === next.session.derivedTitle &&
    prev.session.updatedAt === next.session.updatedAt &&
    prev.session.titleStatus === next.session.titleStatus &&
    prev.session.titleSource === next.session.titleSource &&
    prev.session.titleError === next.session.titleError
  )
}

const SessionItem = memo(SessionItemComponent, areSessionItemsEqual)

export { SessionItem }
