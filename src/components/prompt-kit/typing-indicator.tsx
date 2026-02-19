'use client'

import { cn } from '@/lib/utils'
import { AssistantAvatar } from '@/components/avatars'

export type TypingIndicatorProps = {
  className?: string
}

/**
 * iMessage-style typing indicator with bouncing dots + assistant avatar.
 */
function TypingIndicator({ className }: TypingIndicatorProps) {
  return (
    <div className={cn('flex items-start gap-2', className)}>
      <AssistantAvatar size={24} className="mt-0.5" />
      <div className="rounded-2xl bg-primary-100 px-4 py-3 flex items-center gap-1">
        <span className="size-2 rounded-full bg-primary-400 animate-[typing-bounce_1.4s_ease-in-out_infinite]" />
        <span className="size-2 rounded-full bg-primary-400 animate-[typing-bounce_1.4s_ease-in-out_0.2s_infinite]" />
        <span className="size-2 rounded-full bg-primary-400 animate-[typing-bounce_1.4s_ease-in-out_0.4s_infinite]" />
      </div>
    </div>
  )
}

export { TypingIndicator }
