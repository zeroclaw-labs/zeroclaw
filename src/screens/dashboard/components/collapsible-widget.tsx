import { ArrowDown01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useState } from 'react'
import type * as React from 'react'
import {
  Collapsible,
  CollapsiblePanel,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { cn } from '@/lib/utils'

type CollapsibleWidgetProps = {
  title: string
  summary: string
  defaultOpen?: boolean
  action?: React.ReactNode
  className?: string
  contentClassName?: string
  children: React.ReactNode
}

export function CollapsibleWidget({
  title,
  summary,
  defaultOpen = false,
  action,
  className,
  contentClassName,
  children,
}: CollapsibleWidgetProps) {
  const [isOpen, setIsOpen] = useState(defaultOpen)

  return (
    <Collapsible open={isOpen} onOpenChange={setIsOpen}>
      <section
        className={cn(
          'rounded-xl border border-primary-200 bg-primary-50/85 p-2.5 shadow-sm',
          className,
        )}
      >
        <CollapsibleTrigger
          render={
            <button
              type="button"
              className="w-full justify-between rounded-lg px-2 py-1.5"
            />
          }
          className="w-full justify-between bg-transparent p-0 hover:bg-transparent"
        >
          <span className="min-w-0 text-left">
            <p className="truncate text-sm font-medium text-ink">{title}</p>
            <p className="truncate text-xs text-primary-500">{summary}</p>
          </span>
          <span className="ml-3 inline-flex items-center gap-2">
            {action}
            <HugeiconsIcon
              icon={ArrowDown01Icon}
              size={15}
              strokeWidth={1.5}
              className={cn(
                'shrink-0 text-primary-500 transition-transform duration-200',
                isOpen && 'rotate-180',
              )}
            />
          </span>
        </CollapsibleTrigger>
        <CollapsiblePanel
          className="pt-0"
          contentClassName={cn('pt-2.5', contentClassName)}
        >
          {children}
        </CollapsiblePanel>
      </section>
    </Collapsible>
  )
}
