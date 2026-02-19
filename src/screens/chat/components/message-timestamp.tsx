import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'

type MessageTimestampProps = {
  timestamp: number
}

function isSameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  )
}

function formatShort(timestamp: number): string {
  const date = new Date(timestamp)
  const now = new Date()
  if (isSameDay(date, now)) {
    return new Intl.DateTimeFormat('en-US', {
      hour: 'numeric',
      minute: '2-digit',
      hour12: true,
    }).format(date)
  }

  return new Intl.DateTimeFormat('en-US', {
    day: '2-digit',
    month: 'short',
  }).format(date)
}

function formatFull(timestamp: number): string {
  const value = new Intl.DateTimeFormat('en-US', {
    day: '2-digit',
    month: 'short',
    year: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
    second: '2-digit',
    hour12: true,
  }).format(new Date(timestamp))
  return value
}

export function MessageTimestamp({ timestamp }: MessageTimestampProps) {
  const shortLabel = formatShort(timestamp)
  const fullLabel = formatFull(timestamp)

  return (
    <TooltipProvider>
      <TooltipRoot>
        <TooltipTrigger className="inline-flex items-center text-xs text-primary-600">
          {shortLabel}
        </TooltipTrigger>
        <TooltipContent side="top">{fullLabel}</TooltipContent>
      </TooltipRoot>
    </TooltipProvider>
  )
}
