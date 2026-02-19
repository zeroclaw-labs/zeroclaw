import { useCallback, useRef } from 'react'
import type { TouchEvent } from 'react'
import { useNavigate, useRouterState } from '@tanstack/react-router'
import { useWorkspaceStore } from '@/stores/workspace-store'

const TAB_ORDER = [
  '/dashboard',
  '/agent-swarm',
  '/chat/main',
  '/skills',
  '/settings',
] as const

const EDGE_ZONE = 24
const LOCK_THRESHOLD = 12
const SWIPE_MIN_X = 60
const SWIPE_MAX_Y = 25
const SWIPE_MAX_TIME = 500

type GestureState = {
  startX: number
  startY: number
  startTime: number
  locked: null | 'horizontal' | 'vertical'
  edgeSwipe: boolean
}

function findCurrentTabIndex(pathname: string): number {
  if (pathname.startsWith('/dashboard')) return 0
  if (pathname.startsWith('/agent-swarm') || pathname.startsWith('/agents')) {
    return 1
  }
  if (pathname.startsWith('/chat') || pathname === '/new' || pathname === '/') {
    return 2
  }
  if (pathname.startsWith('/skills')) return 3
  if (pathname.startsWith('/settings')) return 4
  return -1
}

function isOnChatRoute(pathname: string): boolean {
  return pathname.startsWith('/chat') || pathname === '/new' || pathname === '/'
}

function shouldIgnoreTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false
  return Boolean(
    target.closest(
      'input, textarea, button, select, a, pre, code, [role="button"], [role="slider"], [contenteditable], .no-swipe',
    ),
  )
}

function triggerHaptic() {
  try {
    if (typeof navigator !== 'undefined' && 'vibrate' in navigator) {
      navigator.vibrate(10)
    }
  } catch {
    // no-op
  }
}

export function useSwipeNavigation() {
  const navigate = useNavigate()
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const gestureRef = useRef<GestureState | null>(null)

  const onTouchStart = useCallback((event: TouchEvent<HTMLElement>) => {
    // Disable swipe when keyboard is open (typing mode)
    const workspace = useWorkspaceStore.getState()
    if (workspace.mobileKeyboardOpen || workspace.mobileComposerFocused) {
      gestureRef.current = null
      return
    }
    const touch = event.touches[0]
    if (!touch || shouldIgnoreTarget(event.target)) {
      gestureRef.current = null
      return
    }

    const screenWidth = window.innerWidth
    const isEdge =
      touch.clientX <= EDGE_ZONE || touch.clientX >= screenWidth - EDGE_ZONE

    gestureRef.current = {
      startX: touch.clientX,
      startY: touch.clientY,
      startTime: Date.now(),
      locked: null,
      edgeSwipe: isEdge,
    }
  }, [])

  const onTouchMove = useCallback((event: TouchEvent<HTMLElement>) => {
    const gesture = gestureRef.current
    if (!gesture) return

    const touch = event.touches[0]
    if (!touch) return

    if (!gesture.locked) {
      const dx = Math.abs(touch.clientX - gesture.startX)
      const dy = Math.abs(touch.clientY - gesture.startY)

      if (dx >= LOCK_THRESHOLD || dy >= LOCK_THRESHOLD) {
        gesture.locked = dx > dy ? 'horizontal' : 'vertical'
      }
    }

    if (gesture.locked === 'horizontal') {
      event.preventDefault()
    }
  }, [])

  const onTouchEnd = useCallback(
    (event: TouchEvent<HTMLElement>) => {
      const gesture = gestureRef.current
      gestureRef.current = null
      if (!gesture) return

      const touch = event.changedTouches[0]
      if (!touch) return

      const dx = touch.clientX - gesture.startX
      const dy = touch.clientY - gesture.startY
      const dt = Date.now() - gesture.startTime

      if (gesture.locked === 'vertical') return
      if (
        Math.abs(dx) < SWIPE_MIN_X ||
        Math.abs(dy) > SWIPE_MAX_Y ||
        dt >= SWIPE_MAX_TIME
      ) {
        return
      }

      if (isOnChatRoute(pathname) && !gesture.edgeSwipe) return

      const currentIndex = findCurrentTabIndex(pathname)
      if (currentIndex === -1) return

      const nextIndex =
        dx < 0
          ? Math.min(currentIndex + 1, TAB_ORDER.length - 1)
          : Math.max(currentIndex - 1, 0)
      if (nextIndex === currentIndex) return

      triggerHaptic()
      void navigate({ to: TAB_ORDER[nextIndex] as string })
    },
    [navigate, pathname],
  )

  return { onTouchStart, onTouchMove, onTouchEnd }
}
