import { HugeiconsIcon } from '@hugeicons/react'
import { Cancel01Icon, Message01Icon } from '@hugeicons/core-free-icons'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

type AgentChatHeaderProps = {
  agentName: string
  statusLabel: string
  isDemoMode: boolean
  onClose: () => void
}

function getStatusClassName(statusLabel: string): string {
  const normalized = statusLabel.trim().toLowerCase()
  if (normalized === 'failed')
    return 'border-red-500/45 bg-red-500/10 text-red-300'
  if (normalized === 'queued')
    return 'border-primary-400/55 bg-primary-300/70 text-primary-800'
  if (normalized === 'complete')
    return 'border-emerald-500/45 bg-emerald-500/10 text-emerald-300'
  if (normalized === 'thinking')
    return 'border-accent-500/45 bg-accent-500/10 text-accent-300'
  return 'border-primary-400/55 bg-primary-200/60 text-primary-800'
}

export function AgentChatHeader({
  agentName,
  statusLabel,
  isDemoMode,
  onClose,
}: AgentChatHeaderProps) {
  return (
    <div className="flex items-start justify-between gap-3 border-b border-primary-300/70 bg-primary-100/65 px-4 py-3 backdrop-blur-sm">
      <div className="min-w-0 space-y-1">
        <h3 className="flex items-center gap-1.5 text-base font-medium text-balance text-primary-900">
          <HugeiconsIcon icon={Message01Icon} size={20} strokeWidth={1.5} />
          <span className="truncate">{agentName}</span>
        </h3>
        <div className="flex items-center gap-2 text-xs tabular-nums">
          <span
            className={cn(
              'inline-flex items-center rounded-full border px-2 py-0.5',
              getStatusClassName(statusLabel),
            )}
          >
            {statusLabel}
          </span>
          {isDemoMode ? (
            <span className="inline-flex items-center rounded-full border border-accent-500/35 bg-accent-500/10 px-2 py-0.5 text-accent-300">
              Demo Mode
            </span>
          ) : null}
        </div>
      </div>
      <Button
        size="icon-sm"
        variant="ghost"
        className="rounded-full"
        onClick={onClose}
        aria-label="Close agent chat"
      >
        <HugeiconsIcon icon={Cancel01Icon} size={20} strokeWidth={1.5} />
      </Button>
    </div>
  )
}
