import { useEffect } from 'react'
import type { RefObject } from 'react'

const TAP_DEBUG_STORAGE_KEY = 'clawsuite:tap-debug'
const TAP_DEBUG_EVENT = 'clawsuite:tap-debug-change'

function describeElement(value: Element | null): string {
  if (!value) return 'null'
  const id = value.id ? `#${value.id}` : ''
  const className =
    typeof value.className === 'string' && value.className.trim().length > 0
      ? `.${value.className.trim().split(/\s+/).join('.')}`
      : ''
  return `${value.tagName.toLowerCase()}${id}${className}`
}

function snapshotStyles(value: Element | null) {
  if (!value) return null
  const style = window.getComputedStyle(value)
  return {
    position: style.position,
    zIndex: style.zIndex,
    pointerEvents: style.pointerEvents,
    touchAction: style.touchAction,
    transform: style.transform,
    filter: style.filter,
    backdropFilter: style.backdropFilter,
    contain: style.contain,
    overflow: style.overflow,
  }
}

function collectAncestorStyles(value: Element | null, maxDepth = 4) {
  const chain: Array<{
    element: string
    styles: ReturnType<typeof snapshotStyles>
  }> = []
  let current: Element | null = value
  while (current && chain.length < maxDepth) {
    chain.push({
      element: describeElement(current),
      styles: snapshotStyles(current),
    })
    current = current.parentElement
  }
  return chain
}

type TapDebugWindow = Window & {
  __CLAWSUITE_TAP_DEBUG__?: boolean
  setChatTapDebug?: (enabled: boolean) => boolean
  toggleChatTapDebug?: () => boolean
}

function readEnabled(win: TapDebugWindow): boolean {
  if (typeof win.__CLAWSUITE_TAP_DEBUG__ === 'boolean') {
    return win.__CLAWSUITE_TAP_DEBUG__
  }
  try {
    return window.localStorage.getItem(TAP_DEBUG_STORAGE_KEY) === '1'
  } catch {
    return false
  }
}

type UseTapDebugOptions = {
  label?: string
}

export function useTapDebug(
  areaRef: RefObject<HTMLElement | null>,
  options: UseTapDebugOptions = {},
) {
  useEffect(() => {
    if (!import.meta.env.DEV) return

    const area = areaRef.current
    if (!area) return
    const label = options.label ?? 'chat-area'
    const debugWindow = window as TapDebugWindow
    let enabled = readEnabled(debugWindow)

    const applyEnabled = (next: boolean) => {
      enabled = next
      debugWindow.__CLAWSUITE_TAP_DEBUG__ = next
      try {
        window.localStorage.setItem(TAP_DEBUG_STORAGE_KEY, next ? '1' : '0')
      } catch {
        // Ignore storage issues in private browsing.
      }
      window.dispatchEvent(
        new CustomEvent<boolean>(TAP_DEBUG_EVENT, { detail: next }),
      )
    }

    // No visible button — use console: window.toggleChatTapDebug()

    debugWindow.setChatTapDebug = (next: boolean) => {
      applyEnabled(Boolean(next))
      return enabled
    }
    debugWindow.toggleChatTapDebug = () => {
      applyEnabled(!enabled)
      return enabled
    }

    const handleToggleEvent = (event: Event) => {
      const custom = event as CustomEvent<boolean>
      if (typeof custom.detail === 'boolean') {
        enabled = custom.detail
      } else {
        enabled = readEnabled(debugWindow)
      }
    }

    function logTap(
      point: { x: number; y: number },
      eventType: 'touchstart' | 'pointerdown',
      eventTarget: EventTarget | null,
    ) {
      if (!enabled) return

      const hit = document.elementFromPoint(point.x, point.y)
      const target =
        eventTarget instanceof Element
          ? eventTarget
          : eventTarget instanceof Node
            ? (eventTarget as Node).parentElement
            : null

      console.debug(`[tap-debug:${label}]`, {
        type: eventType,
        touch: point,
        hit: describeElement(hit),
        target: describeElement(target),
        hitStyles: snapshotStyles(hit),
        targetStyles: snapshotStyles(target),
        ancestors: collectAncestorStyles(hit),
      })
    }

    function handleTouchStart(event: TouchEvent) {
      const touch = event.touches[0]
      if (!touch) return
      logTap(
        { x: touch.clientX, y: touch.clientY },
        'touchstart',
        event.target,
      )
    }

    function handlePointerDown(event: PointerEvent) {
      logTap(
        { x: event.clientX, y: event.clientY },
        'pointerdown',
        event.target,
      )
    }

    area.addEventListener('touchstart', handleTouchStart, { passive: true })
    area.addEventListener('pointerdown', handlePointerDown, { passive: true })
    window.addEventListener(TAP_DEBUG_EVENT, handleToggleEvent as EventListener)

    console.info(
      `[tap-debug:${label}] toggle via overlay or window.toggleChatTapDebug()`,
    )
    return () => {
      area.removeEventListener('touchstart', handleTouchStart)
      area.removeEventListener('pointerdown', handlePointerDown)
      window.removeEventListener(
        TAP_DEBUG_EVENT,
        handleToggleEvent as EventListener,
      )
      delete debugWindow.setChatTapDebug
      delete debugWindow.toggleChatTapDebug
    }
  }, [areaRef, options.label])
}
