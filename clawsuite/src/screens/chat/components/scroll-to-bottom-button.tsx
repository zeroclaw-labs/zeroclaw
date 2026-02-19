import { AnimatePresence, motion } from 'motion/react'
import { HugeiconsIcon } from '@hugeicons/react'
import { ArrowDown01Icon } from '@hugeicons/core-free-icons'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

const MotionButton = motion(Button)

type ScrollToBottomButtonProps = {
  className?: string
  isVisible: boolean
  unreadCount: number
  onClick: () => void
}

function ScrollToBottomButton({
  className,
  isVisible,
  unreadCount,
  onClick,
}: ScrollToBottomButtonProps) {
  return (
    <AnimatePresence>
      {isVisible ? (
        <MotionButton
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label="Scroll to bottom"
          className={cn(
            'pointer-events-auto relative rounded-full bg-gradient-to-br from-accent-500 via-accent-500 to-amber-500 text-white shadow-lg shadow-accent-500/30 transition-colors hover:from-accent-500 hover:to-accent-600 focus-visible:ring-2 focus-visible:ring-accent-400/70',
            className,
          )}
          initial={{ opacity: 0, y: 10 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: 'easeOut' }}
          onClick={onClick}
        >
          <HugeiconsIcon icon={ArrowDown01Icon} size={20} strokeWidth={1.5} />
          {unreadCount > 0 ? (
            <span className="absolute -right-1 -top-1 inline-flex min-w-5 items-center justify-center rounded-full bg-primary-900 px-1.5 text-xs font-medium tabular-nums text-primary-50">
              {unreadCount > 99 ? '99+' : unreadCount}
            </span>
          ) : null}
        </MotionButton>
      ) : null}
    </AnimatePresence>
  )
}

export { ScrollToBottomButton }
