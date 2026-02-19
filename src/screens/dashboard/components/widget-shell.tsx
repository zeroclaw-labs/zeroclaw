/**
 * WidgetShell — Unified iOS-style widget container
 *
 * Every dashboard widget wraps in this. Handles:
 * - Size variants: small (2×2), medium (4×2), large (4×4)
 * - Glass background + rounded-2xl + press state
 * - Edit mode: jiggle animation + delete badge
 * - Header: icon + title + optional action button
 * - Loading + error states
 */
import { Cancel01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import type * as React from 'react'
import { cn } from '@/lib/utils'
import type { DashboardIcon } from './dashboard-types'

export type WidgetSize = 'small' | 'medium' | 'large'

export type WidgetShellProps = {
  /** Controls padding, header size, and grid span */
  size: WidgetSize
  title: string
  icon?: DashboardIcon
  /** Optional element rendered in the header trailing slot (e.g. refresh button) */
  action?: React.ReactNode
  className?: string
  /** Tap/click handler — navigates or expands the widget */
  onPress?: () => void
  /** When true, widget jiggles and shows delete badge (edit mode) */
  editMode?: boolean
  /** Called when delete badge is tapped in edit mode */
  onRemove?: () => void
  /** Renders a skeleton shimmer instead of children */
  loading?: boolean
  /** Renders an error state with message */
  error?: string
  children?: React.ReactNode
}

/** Maps size → CSS grid-column span class */
export const WIDGET_SIZE_SPAN: Record<WidgetSize, string> = {
  small: 'col-span-1',
  medium: 'col-span-2',
  large: 'col-span-2', // full width on mobile; override on desktop if needed
}

/** Maps size → padding + min-height */
const SIZE_STYLES: Record<WidgetSize, string> = {
  small: 'p-4 md:p-3 min-h-[120px]',
  medium: 'p-4 min-h-[160px]',
  large: 'p-4 min-h-[260px]',
}

/** Maps size → title text size */
const TITLE_SIZE: Record<WidgetSize, string> = {
  small: 'text-[10px]',
  medium: 'text-xs',
  large: 'text-xs',
}

export function WidgetShell({
  size,
  title,
  icon,
  action,
  className,
  onPress,
  editMode = false,
  onRemove,
  loading = false,
  error,
  children,
}: WidgetShellProps) {
  const isInteractive = !!onPress && !editMode

  const shell = (
    <article
      role={isInteractive ? 'button' : 'region'}
      tabIndex={isInteractive ? 0 : undefined}
      aria-label={title}
      onClick={isInteractive ? onPress : undefined}
      onKeyDown={
        isInteractive
          ? (e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault()
                onPress?.()
              }
            }
          : undefined
      }
      className={cn(
        // Base glass card
        'group relative flex flex-col overflow-hidden rounded-2xl',
        'border border-white/30 dark:border-white/10',
        'bg-white/60 dark:bg-neutral-900/50 md:dark:bg-gray-900/60 backdrop-blur-md',
        'shadow-sm transition-shadow',
        // Size
        SIZE_STYLES[size],
        // Interactive press state
        isInteractive && [
          'cursor-pointer',
          'hover:shadow-md',
          'active:scale-[0.97]',
          'transition-transform duration-150',
          'select-none',
        ],
        // Edit mode jiggle
        editMode && 'animate-wiggle',
        className,
      )}
    >
      {/* Header */}
      <header
        className={cn(
          'mb-2 flex shrink-0 items-center justify-between gap-2',
          size === 'small' && 'mb-1.5',
        )}
      >
        <div className="flex min-w-0 items-center gap-1.5">
          {icon ? (
            <HugeiconsIcon
              icon={icon}
              size={size === 'small' ? 13 : 15}
              strokeWidth={1.5}
              className="shrink-0 text-primary-400"
            />
          ) : null}
          <h2
            className={cn(
              'truncate font-medium uppercase tracking-wide text-primary-500',
              TITLE_SIZE[size],
            )}
          >
            {title}
          </h2>
        </div>

        {/* Trailing action (only shown when not in edit mode) */}
        {action && !editMode ? (
          <div className="shrink-0">{action}</div>
        ) : null}
      </header>

      {/* Body */}
      <div className="min-h-0 flex-1 overflow-auto">
        {loading ? (
          <WidgetSkeleton size={size} />
        ) : error ? (
          <WidgetError message={error} />
        ) : (
          children
        )}
      </div>

      {/* Edit mode delete badge */}
      {editMode && onRemove ? (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation()
            onRemove()
          }}
          aria-label={`Remove ${title} widget`}
          className={cn(
            'absolute -right-1.5 -top-1.5 z-10',
            'flex h-5 w-5 items-center justify-center rounded-full',
            'bg-red-500 text-white shadow-md',
            'transition-transform hover:scale-110 active:scale-95',
          )}
        >
          <HugeiconsIcon icon={Cancel01Icon} size={11} strokeWidth={2.5} />
        </button>
      ) : null}
    </article>
  )

  return shell
}

// ---------------------------------------------------------------------------
// Loading skeleton
// ---------------------------------------------------------------------------

function WidgetSkeleton({ size }: { size: WidgetSize }) {
  return (
    <div className="flex h-full flex-col gap-2 pt-1">
      <div
        className={cn(
          'animate-shimmer rounded-lg bg-gray-200/65 dark:bg-gray-700/50',
          size === 'small' ? 'h-8' : 'h-10',
        )}
      />
      {size !== 'small' ? (
        <>
          <div className="h-3 w-3/4 animate-shimmer rounded bg-gray-200/55 dark:bg-gray-700/45" />
          <div className="h-3 w-1/2 animate-shimmer rounded bg-gray-200/45 dark:bg-gray-700/35" />
        </>
      ) : null}
    </div>
  )
}

// ---------------------------------------------------------------------------
// Error state
// ---------------------------------------------------------------------------

function WidgetError({ message }: { message: string }) {
  return (
    <div className="flex h-full items-center justify-center px-2 py-4 text-center">
      <p className="text-[11px] leading-relaxed text-red-400">{message}</p>
    </div>
  )
}
