'use client'

import { useCallback, useLayoutEffect, useRef, useState } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import { ArrowDown01Icon } from '@hugeicons/core-free-icons'
import type { VariantProps } from 'class-variance-authority'
import type { buttonVariants } from '@/components/ui/button'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

export type ScrollButtonProps = {
  className?: string
  scrollRef: React.RefObject<HTMLDivElement | null>
  variant?: VariantProps<typeof buttonVariants>['variant']
  size?: VariantProps<typeof buttonVariants>['size']
} & React.ButtonHTMLAttributes<HTMLButtonElement>

function ScrollButton({
  className,
  variant = 'outline',
  scrollRef,
  ...props
}: ScrollButtonProps) {
  const [isAtBottom, setIsAtBottom] = useState(true)
  const [showButton, setShowButton] = useState(false)
  const lastScrollTopRef = useRef(0)

  const checkIsAtBottom = useCallback(() => {
    const element = scrollRef.current
    if (!element) return

    const isBottom =
      Math.abs(
        element.scrollHeight - element.scrollTop - element.clientHeight,
      ) < 100
    setIsAtBottom(isBottom)
  }, [scrollRef])

  useLayoutEffect(() => {
    const element = scrollRef.current
    if (!element) return

    const handleScroll = () => {
      lastScrollTopRef.current = element.scrollTop
      checkIsAtBottom()
    }

    const observer = new MutationObserver(() => {
      // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
      if (!element) return
      if (element.scrollTop !== lastScrollTopRef.current) {
        lastScrollTopRef.current = element.scrollTop
      }
      checkIsAtBottom()
    })

    checkIsAtBottom()
    element.addEventListener('scroll', handleScroll)
    observer.observe(element, { childList: true, subtree: true })

    return () => {
      element.removeEventListener('scroll', handleScroll)
      observer.disconnect()
    }
  }, [checkIsAtBottom, scrollRef])

  useLayoutEffect(() => {
    if (isAtBottom) {
      setShowButton(false)
      return
    }
    const timer = window.setTimeout(() => {
      setShowButton(true)
    }, 200)
    return () => window.clearTimeout(timer)
  }, [isAtBottom])

  return (
    <Button
      variant="secondary"
      size="icon-sm"
      className={cn(
        'pointer-events-auto rounded-full shadow-md',
        'transition-all duration-100 ease-in-out',
        !isAtBottom && showButton
          ? 'translate-y-0 scale-100 opacity-100'
          : 'pointer-events-none translate-y-4 scale-98 opacity-0',
        className,
      )}
      onClick={() => {
        const element = scrollRef.current
        if (!element) return
        element.scrollTop = element.scrollHeight
        setIsAtBottom(true)
      }}
      {...props}
    >
      <HugeiconsIcon icon={ArrowDown01Icon} size={18} strokeWidth={1.8} />
    </Button>
  )
}

export { ScrollButton }
