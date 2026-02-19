import { useEffect, useRef } from 'react'
import { useWorkspaceStore } from '@/stores/workspace-store'

const OPEN_THRESHOLD = 24
const CLOSE_THRESHOLD = 12
/** Debounce before declaring keyboard closed (ms). Prevents flicker on iOS blur→refocus. */
const CLOSE_DEBOUNCE_MS = 200

export function useMobileKeyboard() {
  const setMobileKeyboardInset = useWorkspaceStore((s) => s.setMobileKeyboardInset)
  const setMobileKeyboardOpen = useWorkspaceStore(
    (s) => s.setMobileKeyboardOpen,
  )
  const lastVvhRef = useRef<number | null>(null)
  const lastKbInsetRef = useRef<number | null>(null)
  const lastKeyboardOpenRef = useRef<boolean | null>(null)
  const closeDebounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  useEffect(() => {
    const vv = window.visualViewport
    const rootStyle = document.documentElement.style

    const applyVvh = (height: number) => {
      if (lastVvhRef.current === height) return
      lastVvhRef.current = height
      rootStyle.setProperty('--vvh', `${height}px`)
    }

    const applyKeyboardInset = (inset: number) => {
      if (lastKbInsetRef.current === inset) return
      lastKbInsetRef.current = inset
      rootStyle.setProperty('--kb-inset', `${inset}px`)
      setMobileKeyboardInset(inset)
    }

    const applyKeyboardState = (open: boolean) => {
      if (lastKeyboardOpenRef.current === open) return

      if (!open) {
        // Debounce close to prevent flicker on iOS blur→refocus
        if (closeDebounceRef.current) return // already pending
        closeDebounceRef.current = setTimeout(() => {
          closeDebounceRef.current = null
          // Re-check: if viewport shrank again during debounce, stay open
          const currentInset = lastKbInsetRef.current ?? 0
          if (currentInset > CLOSE_THRESHOLD) return
          lastKeyboardOpenRef.current = false
          setMobileKeyboardOpen(false)
        }, CLOSE_DEBOUNCE_MS)
        return
      }

      // Opening — cancel any pending close and apply immediately
      if (closeDebounceRef.current) {
        clearTimeout(closeDebounceRef.current)
        closeDebounceRef.current = null
      }
      lastKeyboardOpenRef.current = true
      setMobileKeyboardOpen(true)
    }

    let frameId: number | null = null
    const scheduleUpdate = () => {
      if (frameId !== null) return
      frameId = window.requestAnimationFrame(() => {
        frameId = null
        const layoutHeight = Math.round(window.innerHeight)
        const visualHeight = Math.round(vv?.height ?? layoutHeight)
        const visualTop = Math.round(vv?.offsetTop ?? 0)
        const keyboardInset = Math.max(
          0,
          layoutHeight - (visualHeight + visualTop),
        )

        applyVvh(visualHeight)
        applyKeyboardInset(keyboardInset)

        const wasOpen = lastKeyboardOpenRef.current ?? false
        const nextOpen = wasOpen
          ? keyboardInset > CLOSE_THRESHOLD
          : keyboardInset > OPEN_THRESHOLD
        applyKeyboardState(nextOpen)
      })
    }

    if (!vv) {
      const updateFallback = () => {
        const fallbackHeight = Math.round(window.innerHeight)
        applyVvh(fallbackHeight)
        applyKeyboardInset(0)
        applyKeyboardState(false)
      }

      updateFallback()
      window.addEventListener('resize', updateFallback)

      return () => {
        window.removeEventListener('resize', updateFallback)
      }
    }

    scheduleUpdate()

    vv.addEventListener('resize', scheduleUpdate)
    vv.addEventListener('scroll', scheduleUpdate)
    window.addEventListener('resize', scheduleUpdate)

    return () => {
      vv.removeEventListener('resize', scheduleUpdate)
      vv.removeEventListener('scroll', scheduleUpdate)
      window.removeEventListener('resize', scheduleUpdate)
      if (frameId !== null) {
        window.cancelAnimationFrame(frameId)
      }
      if (closeDebounceRef.current) {
        clearTimeout(closeDebounceRef.current)
      }
    }
  }, [setMobileKeyboardInset, setMobileKeyboardOpen])
}
