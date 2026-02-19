import { memo, useCallback, useEffect, useState } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  Folder01Icon,
  ReloadIcon,
} from '@hugeicons/core-free-icons'
import { OpenClawStudioIcon } from '@/components/icons/clawsuite'
import { OrchestratorAvatar } from '@/components/orchestrator-avatar'
import { Button } from '@/components/ui/button'
import { UsageMeter } from '@/components/usage-meter'
import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

function toTitleCase(value: string): string {
  return value
    .replace(/[_-]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .replace(/\b\w/g, (char) => char.toUpperCase())
}

function formatMobileSessionTitle(rawTitle: string): string {
  const title = rawTitle.trim()
  if (!title) return 'New Chat'

  const normalized = title.toLowerCase()

  // Agent session patterns
  if (normalized === 'agent:main:main' || normalized === 'agent:main') {
    return 'Main Chat'
  }
  const parts = title.split(':').map((part) => part.trim()).filter(Boolean)
  if (
    parts.length >= 2 &&
    parts[0].toLowerCase() === 'agent' &&
    parts[1].length > 0
  ) {
    const candidate = parts[parts.length - 1]
    if (candidate.toLowerCase() === 'main') return 'Main Chat'
    return `${toTitleCase(candidate)} Chat`
  }

  // Common system prompts → friendly names
  if (normalized.startsWith('read heartbeat')) return 'Main Chat'
  if (normalized.startsWith('generate daily')) return 'Daily Brief'
  if (normalized.startsWith('morning check')) return 'Morning Check-in'

  // If it looks like a command/prompt (starts with a verb + long), summarize it
  const MAX_LEN = 20
  if (title.length > MAX_LEN) {
    // Extract first few meaningful words
    const words = title.split(/\s+/)
    let result = ''
    for (const word of words) {
      if ((result + ' ' + word).trim().length > MAX_LEN) break
      result = (result + ' ' + word).trim()
    }
    return result.length > 0 ? `${result}…` : `${title.slice(0, MAX_LEN)}…`
  }

  return title
}

function formatSyncAge(updatedAt: number): string {
  if (updatedAt <= 0) return ''
  const seconds = Math.round((Date.now() - updatedAt) / 1000)
  if (seconds < 5) return 'just now'
  if (seconds < 60) return `${seconds}s ago`
  const minutes = Math.round(seconds / 60)
  return `${minutes}m ago`
}

type ChatHeaderProps = {
  activeTitle: string
  wrapperRef?: React.Ref<HTMLDivElement>
  onOpenSessions?: () => void
  showFileExplorerButton?: boolean
  fileExplorerCollapsed?: boolean
  onToggleFileExplorer?: () => void
  /** Timestamp (ms) of last successful history fetch */
  dataUpdatedAt?: number
  /** Callback to manually refresh history */
  onRefresh?: () => void
  /** Current model id/name for compact mobile status */
  agentModel?: string
  /** Whether agent connection is healthy */
  agentConnected?: boolean
  /** Open agent details panel on mobile status tap */
  onOpenAgentDetails?: () => void
  /** Pull-to-refresh offset in px — header slides down */
  pullOffset?: number
}

function ChatHeaderComponent({
  activeTitle,
  wrapperRef,
  onOpenSessions,
  showFileExplorerButton = false,
  fileExplorerCollapsed = true,
  onToggleFileExplorer,
  dataUpdatedAt = 0,
  onRefresh,
  agentModel: _agentModel = '',
  agentConnected = true,
  onOpenAgentDetails,
  pullOffset = 0,
}: ChatHeaderProps) {
  const [syncLabel, setSyncLabel] = useState('')
  const [isRefreshing, setIsRefreshing] = useState(false)
  const [isMobile, setIsMobile] = useState(false)

  useEffect(() => {
    if (dataUpdatedAt <= 0) return
    const update = () => setSyncLabel(formatSyncAge(dataUpdatedAt))
    update()
    const id = setInterval(update, 5000)
    return () => clearInterval(id)
  }, [dataUpdatedAt])

  useEffect(() => {
    const media = window.matchMedia('(max-width: 767px)')
    const update = () => setIsMobile(media.matches)
    update()
    media.addEventListener('change', update)
    return () => media.removeEventListener('change', update)
  }, [])

  const isStale = dataUpdatedAt > 0 && Date.now() - dataUpdatedAt > 15000
  const mobileTitle = formatMobileSessionTitle(activeTitle)
  void _agentModel; void agentConnected // kept for prop compat

  const handleRefresh = useCallback(() => {
    if (!onRefresh) return
    setIsRefreshing(true)
    onRefresh()
    setTimeout(() => setIsRefreshing(false), 600)
  }, [onRefresh])

  const handleOpenAgentDetails = useCallback(() => {
    if (onOpenAgentDetails) {
      onOpenAgentDetails()
      return
    }
    window.dispatchEvent(new CustomEvent('clawsuite:chat-agent-details'))
  }, [onOpenAgentDetails])

  if (isMobile) {
    return (
      <div
        ref={wrapperRef}
        className="shrink-0 border-b border-primary-200 px-4 h-12 flex items-center justify-between bg-surface transition-transform"
        style={pullOffset > 0 ? { transform: `translateY(${pullOffset}px)` } : undefined}
      >
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <button
            type="button"
            onClick={onOpenSessions}
            className="shrink-0 rounded-lg transition-transform active:scale-95"
            aria-label="Open sessions"
          >
            <OpenClawStudioIcon className="size-8 rounded-lg" />
          </button>
          <div className="min-w-0 max-w-[45vw] overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold tracking-tight text-ink">
            {mobileTitle}
          </div>
        </div>

        <div className="ml-2 flex shrink-0 items-center gap-1">
          <button
            type="button"
            onClick={handleOpenAgentDetails}
            className="relative rounded-full transition-transform active:scale-90"
            aria-label="Open agent details"
          >
            <OrchestratorAvatar size={28} compact />
          </button>
        </div>
      </div>
    )
  }

  return (
    <div
      ref={wrapperRef}
      className="shrink-0 border-b border-primary-200 px-4 h-12 flex items-center bg-surface"
    >
      {showFileExplorerButton ? (
        <TooltipProvider>
          <TooltipRoot>
            <TooltipTrigger
              onClick={onToggleFileExplorer}
              render={
                <Button
                  size="icon-sm"
                  variant="ghost"
                  className="mr-2 text-primary-800 hover:bg-primary-100"
                  aria-label={
                    fileExplorerCollapsed ? 'Show files' : 'Hide files'
                  }
                >
                  <HugeiconsIcon
                    icon={Folder01Icon}
                    size={20}
                    strokeWidth={1.5}
                  />
                </Button>
              }
            />
            <TooltipContent side="bottom">
              {fileExplorerCollapsed ? 'Show files' : 'Hide files'}
            </TooltipContent>
          </TooltipRoot>
        </TooltipProvider>
      ) : null}
      <div
        className="min-w-0 flex-1 truncate text-sm font-medium text-balance"
        suppressHydrationWarning
      >
        {activeTitle}
      </div>
      {syncLabel ? (
        <span
          className={cn(
            'mr-1 text-[11px] tabular-nums transition-colors',
            isStale ? 'text-amber-500' : 'text-primary-400',
          )}
          title={
            dataUpdatedAt > 0
              ? `Last synced: ${new Date(dataUpdatedAt).toLocaleTimeString()}`
              : undefined
          }
        >
          {isStale ? '⚠ ' : ''}
          {syncLabel}
        </span>
      ) : null}
      {onRefresh ? (
        <TooltipProvider>
          <TooltipRoot>
            <TooltipTrigger
              onClick={handleRefresh}
              render={
                <Button
                  size="icon-sm"
                  variant="ghost"
                  className="mr-1 text-primary-500 hover:bg-primary-100 hover:text-primary-700"
                  aria-label="Refresh chat"
                >
                  <HugeiconsIcon
                    icon={ReloadIcon}
                    size={20}
                    strokeWidth={1.5}
                    className={cn(isRefreshing && 'animate-spin')}
                  />
                </Button>
              }
            />
            <TooltipContent side="bottom">Sync messages</TooltipContent>
          </TooltipRoot>
        </TooltipProvider>
      ) : null}
      <UsageMeter />
    </div>
  )
}

const MemoizedChatHeader = memo(ChatHeaderComponent)

export { MemoizedChatHeader as ChatHeader }
