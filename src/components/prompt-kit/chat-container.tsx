'use client'

import * as React from 'react'
import { cn } from '@/lib/utils'

export type ChatContainerRootProps = {
  children: React.ReactNode
  overlay?: React.ReactNode
  className?: string
  stickToBottom?: boolean
  onUserScroll?: (metrics: {
    scrollTop: number
    scrollHeight: number
    clientHeight: number
  }) => void
} & React.HTMLAttributes<HTMLDivElement>

export type ChatContainerContentProps = {
  children: React.ReactNode
  className?: string
} & React.HTMLAttributes<HTMLDivElement>

export type ChatContainerScrollAnchorProps = {
  className?: string
  ref?: React.Ref<HTMLDivElement>
} & React.HTMLAttributes<HTMLDivElement>

const NEAR_BOTTOM_THRESHOLD = 200

function ChatContainerRoot({
  children,
  overlay,
  className,
  stickToBottom = true,
  onUserScroll,
  ...props
}: ChatContainerRootProps) {
  const scrollRef = React.useRef<HTMLDivElement | null>(null)
  const stickToBottomRef = React.useRef(stickToBottom)
  const lastScrollTopRef = React.useRef(0)

  React.useLayoutEffect(() => {
    stickToBottomRef.current = stickToBottom
  }, [stickToBottom])

  React.useLayoutEffect(() => {
    const element = scrollRef.current
    if (!element) return

    const handleScroll = () => {
      // Track stick-to-bottom internally based on actual scroll position
      const distFromBottom =
        element.scrollHeight - element.scrollTop - element.clientHeight
      const wasScrollingUp = element.scrollTop < lastScrollTopRef.current - 5
      lastScrollTopRef.current = element.scrollTop

      if (wasScrollingUp && distFromBottom > NEAR_BOTTOM_THRESHOLD) {
        stickToBottomRef.current = false
      } else if (distFromBottom <= NEAR_BOTTOM_THRESHOLD) {
        stickToBottomRef.current = true
      }

      onUserScroll?.({
        scrollTop: element.scrollTop,
        scrollHeight: element.scrollHeight,
        clientHeight: element.clientHeight,
      })
    }

    element.addEventListener('scroll', handleScroll, { passive: true })
    return () => element.removeEventListener('scroll', handleScroll)
  }, [onUserScroll])

  // ResizeObserver: re-anchor to bottom when content expands
  React.useLayoutEffect(() => {
    const viewport = scrollRef.current
    if (!viewport || typeof ResizeObserver === 'undefined') return

    let resizeObserver: ResizeObserver | null = null

    const initObserver = () => {
      const content = viewport.firstElementChild
      if (!(content instanceof HTMLElement)) {
        // Content not ready yet, retry after next frame
        requestAnimationFrame(initObserver)
        return
      }

      let previousHeight = content.getBoundingClientRect().height

      resizeObserver = new ResizeObserver((entries) => {
        const entry = entries[0]
        if (!entry) return
        const nextHeight = entry.contentRect.height
        const heightDelta = nextHeight - previousHeight
        if (heightDelta === 0) return

        // Re-anchor to bottom when content grows and we're in stick-to-bottom mode.
        // stickToBottomRef tracks actual scroll position (set false when user scrolls up),
        // so this won't fight user scroll.
        if (heightDelta > 0 && stickToBottomRef.current) {
          viewport.scrollTo({ top: viewport.scrollHeight, behavior: 'auto' })
        }

        previousHeight = nextHeight
      })

      resizeObserver.observe(content)
    }

    // Use requestAnimationFrame to ensure content is mounted before observing
    requestAnimationFrame(initObserver)

    return () => {
      if (resizeObserver) {
        resizeObserver.disconnect()
      }
    }
  }, [])

  return (
    <div
      className={cn(
        'relative flex-1 min-h-0 overflow-hidden flex flex-col',
        className,
      )}
    >
      <div
        ref={scrollRef}
        className="flex-1 min-h-0 overflow-y-auto overscroll-contain"
        style={{ overflowAnchor: 'none' }}
        data-chat-scroll-viewport
        {...props}
      >
        {children}
      </div>
      {overlay}
    </div>
  )
}

function ChatContainerContent({
  children,
  className,
  ...props
}: ChatContainerContentProps) {
  return (
    <div className={cn('flex w-full flex-col', className)} {...props}>
      <div
        className="mx-auto w-full px-3 sm:px-5 flex flex-col"
        style={{ maxWidth: 'min(768px, 100%)' }}
      >
        <div className="flex flex-col space-y-3">{children}</div>
      </div>
    </div>
  )
}

function ChatContainerScrollAnchor({
  ...props
}: ChatContainerScrollAnchorProps) {
  return (
    <div
      className="h-px w-full shrink-0 scroll-mt-4 pt-8 pb-4"
      style={{ overflowAnchor: 'auto' }}
      aria-hidden="true"
      {...props}
    />
  )
}

export { ChatContainerRoot, ChatContainerContent, ChatContainerScrollAnchor }
