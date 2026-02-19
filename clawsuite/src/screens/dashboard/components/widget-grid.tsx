import type { ReactNode } from 'react'
import type { WidgetSize } from './widget-shell'
import { cn } from '@/lib/utils'

export type WidgetGridItem = {
  id: string
  size: WidgetSize
  node: ReactNode
}

type WidgetGridProps = {
  items: Array<WidgetGridItem>
  className?: string
}

const GRID_SPAN_BY_SIZE: Record<WidgetSize, string> = {
  small: 'col-span-1',
  medium: 'col-span-2',
  large: 'col-span-2 md:col-span-4',
}

export function WidgetGrid({ items, className }: WidgetGridProps) {
  return (
    <div className={cn('grid grid-cols-2 gap-3 md:grid-cols-4 md:gap-4', className)}>
      {items.map((item) => (
        <div key={item.id} className={cn('min-w-0', GRID_SPAN_BY_SIZE[item.size])}>
          {item.node}
        </div>
      ))}
    </div>
  )
}
