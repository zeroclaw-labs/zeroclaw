import { useEffect } from 'react'
import { useTerminalPanelStore } from '@/stores/terminal-panel-store'

export function TerminalShortcutListener() {
  const togglePanel = useTerminalPanelStore((state) => state.togglePanel)

  useEffect(
    function registerGlobalTerminalShortcut() {
      function onKeyDown(event: KeyboardEvent) {
        const hasModifier = event.ctrlKey || event.metaKey
        const isBacktick = event.key === '`' || event.code === 'Backquote'
        if (!hasModifier || !isBacktick) return
        event.preventDefault()
        togglePanel()
      }

      window.addEventListener('keydown', onKeyDown)
      return function cleanup() {
        window.removeEventListener('keydown', onKeyDown)
      }
    },
    [togglePanel],
  )

  return null
}
