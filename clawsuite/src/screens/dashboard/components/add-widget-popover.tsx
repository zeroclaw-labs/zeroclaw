/**
 * Popover listing widgets not currently on the dashboard.
 * Each row shows name + tier badge + Add button.
 */
import { Add01Icon, Cancel01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useEffect, useRef, useState } from 'react'
import type { WidgetId } from '../constants/grid-config'
import { WIDGET_META } from '../constants/widget-meta'
import { cn } from '@/lib/utils'

type AddWidgetPopoverProps = {
  visibleIds: WidgetId[]
  onAdd: (id: WidgetId) => void
  buttonClassName?: string
  compact?: boolean
}

export function AddWidgetPopover({
  visibleIds,
  onAdd,
  buttonClassName,
  compact,
}: AddWidgetPopoverProps) {
  const [open, setOpen] = useState(false)
  const popoverRef = useRef<HTMLDivElement>(null)

  const hiddenWidgets = WIDGET_META.filter(
    (w) => !visibleIds.includes(w.id as WidgetId),
  )

  useEffect(() => {
    if (!open) return
    function handleClick(e: MouseEvent) {
      if (
        popoverRef.current &&
        !popoverRef.current.contains(e.target as Node)
      ) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', handleClick)
    return () => document.removeEventListener('mousedown', handleClick)
  }, [open])

  return (
    <div className="relative">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        className={cn(
          'inline-flex items-center gap-1 rounded-md px-2 py-1 text-[11px] text-primary-400 transition-colors hover:text-primary-700 disabled:opacity-30 dark:hover:text-primary-300',
          buttonClassName,
        )}
        aria-label="Widgets"
        title="Widgets"
        disabled={hiddenWidgets.length === 0}
      >
        <HugeiconsIcon icon={Add01Icon} size={compact ? 16 : 13} strokeWidth={1.5} />
        {!compact ? <span>Widgets</span> : null}
      </button>

      {open ? (
        <div
          ref={popoverRef}
          className="absolute right-0 top-full z-[9999] mt-2 w-64 rounded-xl border border-primary-200 bg-primary-50 p-3 shadow-xl backdrop-blur-xl dark:bg-primary-100"
        >
          <div className="mb-2 flex items-center justify-between">
            <h3 className="text-xs font-medium uppercase tracking-wide text-primary-500">
              Widgets
            </h3>
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="rounded p-0.5 text-primary-400 hover:text-primary-700"
            >
              <HugeiconsIcon icon={Cancel01Icon} size={14} strokeWidth={1.5} />
            </button>
          </div>

          {hiddenWidgets.length === 0 ? (
            <p className="py-4 text-center text-xs text-primary-400">
              All widgets are visible
            </p>
          ) : (
            <ul className="space-y-1">
              {hiddenWidgets.map((w) => (
                <li
                  key={w.id}
                  className="flex items-center justify-between rounded-lg px-2 py-1.5 hover:bg-primary-100 dark:hover:bg-primary-200/50"
                >
                  <div className="flex items-center gap-2 min-w-0">
                    <span className="truncate text-sm text-ink">{w.label}</span>
                    {w.tier === 'demo' ? (
                      <span className="shrink-0 rounded bg-amber-100 px-1 py-px text-[10px] font-medium text-amber-700 dark:bg-amber-900/50 dark:text-amber-400">
                        Demo
                      </span>
                    ) : null}
                  </div>
                  <button
                    type="button"
                    onClick={() => {
                      onAdd(w.id as WidgetId)
                    }}
                    className="shrink-0 rounded-md border border-primary-200 px-2 py-0.5 text-[11px] font-medium text-primary-600 transition-colors hover:border-primary-300 hover:text-ink"
                  >
                    Add
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : null}
    </div>
  )
}
