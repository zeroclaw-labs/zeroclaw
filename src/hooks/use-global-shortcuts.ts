/**
 * Phase 3.1: Centralized global keyboard shortcuts
 * Handles Cmd/Ctrl+P, Cmd/Ctrl+B, Cmd/Ctrl+Shift+L
 */
import { useEffect } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useSearchModal } from '@/hooks/use-search-modal'
import { useWorkspaceStore } from '@/stores/workspace-store'

function _isInputFocused(): boolean {
  const active = document.activeElement
  if (!active) return false
  const tag = active.tagName.toLowerCase()
  if (tag === 'input' || tag === 'textarea') return true
  if ((active as HTMLElement).isContentEditable) return true
  return false
}

// Sidebar toggle event — listened by the sidebar component
export const SIDEBAR_TOGGLE_EVENT = 'global:toggle-sidebar'

export function emitSidebarToggle() {
  if (typeof window === 'undefined') return
  window.dispatchEvent(new CustomEvent(SIDEBAR_TOGGLE_EVENT))
}

export function useGlobalShortcuts() {
  const navigate = useNavigate()
  const openModal = useSearchModal((state) => state.openModal)
  const setScope = useSearchModal((state) => state.setScope)
  const toggleChatPanel = useWorkspaceStore((s) => s.toggleChatPanel)

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.isComposing) return

      const mod = event.metaKey || event.ctrlKey

      // Cmd/Ctrl+P — Quick open file
      if (mod && event.key.toLowerCase() === 'p' && !event.shiftKey) {
        event.preventDefault()
        setScope('files')
        openModal()
        return
      }

      // Cmd/Ctrl+B — Toggle sidebar
      if (mod && event.key.toLowerCase() === 'b' && !event.shiftKey) {
        event.preventDefault()
        emitSidebarToggle()
        return
      }

      // Cmd/Ctrl+J — Toggle chat panel
      if (mod && event.key.toLowerCase() === 'j' && !event.shiftKey) {
        event.preventDefault()
        toggleChatPanel()
        return
      }

      // Cmd/Ctrl+Shift+L — Focus activity log
      if (mod && event.shiftKey && event.key.toLowerCase() === 'l') {
        event.preventDefault()
        void navigate({ to: '/activity' })
        return
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [navigate, openModal, setScope, toggleChatPanel])
}

// Preserve for future input-focus checking
void _isInputFocused
