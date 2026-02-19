'use client'

import { cn } from '@/lib/utils'

export type ThreeDotsSpinnerProps = {
  className?: string
  dotClassName?: string
}

function ThreeDotsSpinner({ className, dotClassName }: ThreeDotsSpinnerProps) {
  return (
    <span className={cn('three-dots-spinner', className)} aria-hidden="true">
      <span className={cn('three-dots-spinner-dot', dotClassName)} />
      <span className={cn('three-dots-spinner-dot', dotClassName)} />
      <span className={cn('three-dots-spinner-dot', dotClassName)} />
    </span>
  )
}

export { ThreeDotsSpinner }
