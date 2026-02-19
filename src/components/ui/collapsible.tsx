'use client'

import { Collapsible as BaseCollapsible } from '@base-ui/react/collapsible'
import * as React from 'react'
import { cn } from '@/lib/utils'

function Collapsible(props: React.ComponentProps<typeof BaseCollapsible.Root>) {
  return <BaseCollapsible.Root {...props} />
}

function CollapsibleTrigger({
  className,
  ...props
}: React.ComponentProps<typeof BaseCollapsible.Trigger>) {
  return (
    <BaseCollapsible.Trigger
      className={cn(
        'group inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-left text-xs font-medium text-primary-500 transition-colors hover:bg-primary-100 hover:text-primary-700 data-panel-open:text-primary-700',
        className,
      )}
      {...props}
    />
  )
}

type CollapsiblePanelProps = React.ComponentProps<
  typeof BaseCollapsible.Panel
> & {
  contentClassName?: string
}

function CollapsiblePanel({
  className,
  contentClassName,
  children,
  ...props
}: CollapsiblePanelProps) {
  return (
    <BaseCollapsible.Panel
      keepMounted
      className={cn(
        'flex h-(--collapsible-panel-height) flex-col overflow-hidden text-sm transition-all duration-150 ease-out data-ending-style:h-0 data-starting-style:h-0',
        className,
      )}
      {...props}
    >
      <div className={cn('pt-1', contentClassName)}>{children}</div>
    </BaseCollapsible.Panel>
  )
}

export { Collapsible, CollapsibleTrigger, CollapsiblePanel }
