import { useLayoutEffect, useRef, useState } from 'react'

export type ChatMeasurements = {
  headerRef: React.RefObject<HTMLDivElement | null>
  composerRef: React.RefObject<HTMLDivElement | null>
  mainRef: React.RefObject<HTMLDivElement | null>
  pinGroupMinHeight: number
  headerHeight: number
}

export function useChatMeasurements(): ChatMeasurements {
  const headerRef = useRef<HTMLDivElement | null>(null)
  const composerRef = useRef<HTMLDivElement | null>(null)
  const mainRef = useRef<HTMLDivElement | null>(null)
  const [pinGroupMinHeight, setPinGroupMinHeight] = useState(0)
  const [headerHeight, setHeaderHeight] = useState(0)

  // Measure header/composer to keep pinned group exact.
  useLayoutEffect(() => {
    const headerEl = headerRef.current
    const composerEl = composerRef.current
    const mainEl = mainRef.current
    if (!mainEl) return

    const applySizes = () => {
      const nextHeaderHeight = headerEl?.offsetHeight ?? 0
      const composerHeight = composerEl?.offsetHeight ?? 0
      const mainHeight = mainEl.clientHeight

      // Safety: If all dimensions are 0, retry after paint to avoid race condition
      if (nextHeaderHeight === 0 && composerHeight === 0 && mainHeight === 0) {
        requestAnimationFrame(() => applySizes())
        return
      }

      mainEl.style.setProperty(
        '--chat-header-height',
        `${Math.max(0, nextHeaderHeight)}px`,
      )
      mainEl.style.setProperty(
        '--chat-composer-height',
        `${Math.max(0, composerHeight)}px`,
      )
      setHeaderHeight(nextHeaderHeight)
      setPinGroupMinHeight(
        Math.max(0, mainHeight - nextHeaderHeight - composerHeight),
      )
    }

    // Double requestAnimationFrame to ensure layout is complete before measuring
    // First rAF: queue after current script execution
    // Second rAF: queue after layout/paint
    requestAnimationFrame(() => {
      requestAnimationFrame(() => applySizes())
    })

    const observer = new ResizeObserver(() => applySizes())
    if (headerEl) observer.observe(headerEl)
    if (composerEl) observer.observe(composerEl)
    observer.observe(mainEl)
    return () => observer.disconnect()
  }, [])

  return {
    headerRef,
    composerRef,
    mainRef,
    pinGroupMinHeight,
    headerHeight,
  }
}
