import { lazy, Suspense, useCallback, useEffect, useRef } from 'react'
import { AnimatePresence, motion } from 'motion/react'
import { useNavigate } from '@tanstack/react-router'
const TerminalWorkspace = lazy(() =>
  import('@/components/terminal/terminal-workspace').then((m) => ({
    default: m.TerminalWorkspace,
  })),
)
import {
  DEFAULT_PANEL_HEIGHT,
  MIN_PANEL_HEIGHT,
  useTerminalPanelStore,
} from '@/stores/terminal-panel-store'

const MAX_VIEWPORT_RATIO = 0.6

export function TerminalPanel() {
  const navigate = useNavigate()
  const isPanelOpen = useTerminalPanelStore((state) => state.isPanelOpen)
  const panelHeight = useTerminalPanelStore((state) => state.panelHeight)
  const setPanelOpen = useTerminalPanelStore((state) => state.setPanelOpen)
  const setPanelHeight = useTerminalPanelStore((state) => state.setPanelHeight)

  const dragStateRef = useRef<{
    startY: number
    startHeight: number
  } | null>(null)

  const handleMinimize = useCallback(
    function handleMinimize() {
      setPanelOpen(false)
    },
    [setPanelOpen],
  )

  const handleMaximize = useCallback(
    function handleMaximize() {
      navigate({ to: '/terminal' })
    },
    [navigate],
  )

  const handleClose = useCallback(
    function handleClose() {
      setPanelOpen(false)
    },
    [setPanelOpen],
  )

  const handleResizeStart = useCallback(
    function handleResizeStart(event: React.MouseEvent<HTMLDivElement>) {
      event.preventDefault()
      dragStateRef.current = {
        startY: event.clientY,
        startHeight: panelHeight || DEFAULT_PANEL_HEIGHT,
      }

      function onMove(moveEvent: MouseEvent) {
        const dragState = dragStateRef.current
        if (!dragState) return
        const delta = dragState.startY - moveEvent.clientY
        const maxHeight = Math.floor(window.innerHeight * MAX_VIEWPORT_RATIO)
        const nextHeight = Math.max(
          MIN_PANEL_HEIGHT,
          Math.min(maxHeight, dragState.startHeight + delta),
        )
        setPanelHeight(nextHeight)
      }

      function onUp() {
        dragStateRef.current = null
        window.removeEventListener('mousemove', onMove)
        window.removeEventListener('mouseup', onUp)
      }

      window.addEventListener('mousemove', onMove)
      window.addEventListener('mouseup', onUp)
    },
    [panelHeight, setPanelHeight],
  )

  useEffect(
    function clampHeightToViewport() {
      function clamp() {
        const maxHeight = Math.floor(window.innerHeight * MAX_VIEWPORT_RATIO)
        if (panelHeight > maxHeight) {
          setPanelHeight(maxHeight)
        }
      }

      clamp()
      window.addEventListener('resize', clamp)
      return function cleanup() {
        window.removeEventListener('resize', clamp)
      }
    },
    [panelHeight, setPanelHeight],
  )

  return (
    <AnimatePresence initial={false}>
      {isPanelOpen ? (
        <motion.section
          key="terminal-panel"
          initial={{ y: 36, opacity: 0 }}
          animate={{ y: 0, opacity: 1 }}
          exit={{ y: 32, opacity: 0 }}
          transition={{ duration: 0.2, ease: 'easeOut' }}
          className="absolute inset-x-0 bottom-0 z-40 border-t border-primary-300 bg-primary-50 shadow-[0_-12px_40px_rgba(0,0,0,0.45)]"
          style={{ height: panelHeight }}
        >
          <div
            className="absolute inset-x-0 top-0 h-1 cursor-row-resize bg-primary-300/50 transition-colors hover:bg-[#ea580c]/80"
            onMouseDown={handleResizeStart}
            role="separator"
            aria-label="Resize terminal panel"
          />
          <div className="h-full pt-1">
            <Suspense
              fallback={
                <div className="flex h-full items-center justify-center text-xs text-primary-500">
                  Loading terminal…
                </div>
              }
            >
              <TerminalWorkspace
                mode="panel"
                panelVisible={isPanelOpen}
                onMinimizePanel={handleMinimize}
                onMaximizePanel={handleMaximize}
                onClosePanel={handleClose}
              />
            </Suspense>
          </div>
        </motion.section>
      ) : null}
    </AnimatePresence>
  )
}
