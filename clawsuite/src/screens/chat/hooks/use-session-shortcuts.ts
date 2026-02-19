import { useEffect } from 'react'

type SessionShortcutOptions = {
  onNewSession: () => void
  onSearchSessions: () => void
}

function useSessionShortcuts({
  onNewSession,
  onSearchSessions,
}: SessionShortcutOptions) {
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) return
      if (event.altKey) return
      if (!(event.metaKey || event.ctrlKey)) return
      const target = event.target as HTMLElement | null
      if (!target) return
      const tag = target.tagName.toLowerCase()
      if (
        tag === 'input' ||
        tag === 'textarea' ||
        tag === 'select' ||
        target.isContentEditable
      ) {
        return
      }

      if (event.key.toLowerCase() === 'k' && !event.shiftKey) {
        event.preventDefault()
        onSearchSessions()
      }

      if (event.key.toLowerCase() === 'o' && event.shiftKey) {
        event.preventDefault()
        onNewSession()
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [onNewSession, onSearchSessions])
}

export { useSessionShortcuts }
