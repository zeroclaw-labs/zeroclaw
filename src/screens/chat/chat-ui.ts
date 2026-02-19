import type { QueryClient } from '@tanstack/react-query'

export type ChatUiState = {
  isSidebarCollapsed: boolean
}

const defaultChatUiState: ChatUiState = {
  isSidebarCollapsed: false,
}

export const chatUiQueryKey = ['chat', 'ui'] as const

export function getChatUiState(queryClient: QueryClient): ChatUiState {
  const cached = queryClient.getQueryData(chatUiQueryKey)
  if (cached && typeof cached === 'object') {
    return {
      ...defaultChatUiState,
      ...(cached as Partial<ChatUiState>),
    }
  }
  return defaultChatUiState
}

export function setChatUiState(
  queryClient: QueryClient,
  updater: (state: ChatUiState) => ChatUiState,
) {
  queryClient.setQueryData(chatUiQueryKey, function update(state: unknown) {
    const current =
      state && typeof state === 'object'
        ? {
            ...defaultChatUiState,
            ...(state as Partial<ChatUiState>),
          }
        : defaultChatUiState
    return updater(current)
  })
}
