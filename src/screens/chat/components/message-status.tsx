import { Message } from '@/components/prompt-kit/message'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

type MessageStatusProps = {
  title: string
  description: React.ReactNode
  detail?: string | null
  actionLabel?: string
  onAction?: () => void
  className?: string
}

export function MessageStatus({
  title,
  description,
  detail,
  actionLabel,
  onAction,
  className,
}: MessageStatusProps) {
  return (
    <div className={cn('w-full max-w-[900px]', className)}>
      <Message>
        <div className="w-full rounded-xl border border-primary-200 bg-primary-50 p-4 text-primary-900">
          <div className="text-balance font-medium">{title}</div>
          <div className="mt-2 text-pretty text-primary-700">{description}</div>
          {detail ? (
            <div className="mt-2 text-xs text-primary-600">{detail}</div>
          ) : null}
          {actionLabel && onAction ? (
            <div className="mt-3">
              <Button size="sm" variant="outline" onClick={onAction}>
                {actionLabel}
              </Button>
            </div>
          ) : null}
        </div>
      </Message>
    </div>
  )
}
