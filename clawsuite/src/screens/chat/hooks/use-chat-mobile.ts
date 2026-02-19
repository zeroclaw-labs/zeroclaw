import { useLayoutEffect, useState } from 'react'
import { setChatUiState } from '../chat-ui'
import type { QueryClient } from '@tanstack/react-query'

export function useChatMobile(queryClient: QueryClient) {
  const [isMobile, setIsMobile] = useState(false)

  useLayoutEffect(() => {
    const media = window.matchMedia('(max-width: 768px)')
    const update = () => setIsMobile(media.matches)
    update()
    media.addEventListener('change', update)
    return () => media.removeEventListener('change', update)
  }, [])

  useLayoutEffect(() => {
    if (!isMobile) return
    setChatUiState(queryClient, function collapse(state) {
      return { ...state, isSidebarCollapsed: true }
    })
  }, [isMobile, queryClient])

  return { isMobile }
}
