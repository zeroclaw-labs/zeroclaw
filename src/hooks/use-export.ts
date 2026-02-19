import { useCallback } from 'react'
import { useQueryClient } from '@tanstack/react-query'

import { chatQueryKeys } from '../screens/chat/chat-queries'
import { getMessageTimestamp, textFromMessage } from '../screens/chat/utils'
import type { GatewayMessage, HistoryResponse } from '../screens/chat/types'

type ExportFormat = 'markdown' | 'json' | 'text'

type UseExportInput = {
  currentFriendlyId: string
  currentSessionKey: string
  sessionTitle: string
}

export function useExport({
  currentFriendlyId,
  currentSessionKey,
  sessionTitle,
}: UseExportInput) {
  const queryClient = useQueryClient()

  const exportConversation = useCallback(
    function exportConversation(format: ExportFormat) {
      const historyKey = chatQueryKeys.history(
        currentFriendlyId,
        currentSessionKey || currentFriendlyId,
      )
      const cached = queryClient.getQueryData<HistoryResponse>(historyKey)
      const messages = Array.isArray(cached?.messages) ? cached.messages : []

      if (messages.length === 0) return

      const title = sessionTitle || currentFriendlyId
      const chatMessages = messages.filter(
        (msg) => msg.role === 'user' || msg.role === 'assistant',
      )

      let content: string
      let extension: string
      let mimeType: string

      switch (format) {
        case 'markdown':
          content = toMarkdown(chatMessages, title)
          extension = 'md'
          mimeType = 'text/markdown'
          break
        case 'json':
          content = toJSON(chatMessages, title)
          extension = 'json'
          mimeType = 'application/json'
          break
        case 'text':
          content = toPlainText(chatMessages, title)
          extension = 'txt'
          mimeType = 'text/plain'
          break
      }

      const filename = sanitizeFilename(title) + '.' + extension
      downloadFile(content, filename, mimeType)
    },
    [currentFriendlyId, currentSessionKey, queryClient, sessionTitle],
  )

  return { exportConversation }
}

function formatTimestamp(message: GatewayMessage): string {
  const ts = getMessageTimestamp(message)
  return new Date(ts).toLocaleString('en-GB', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function roleLabel(role: string | undefined): string {
  if (role === 'user') return 'You'
  if (role === 'assistant') return 'Assistant'
  return role || 'Unknown'
}

function toMarkdown(messages: Array<GatewayMessage>, title: string): string {
  const lines: Array<string> = []
  lines.push('# ' + title)
  lines.push('')
  lines.push('Exported on ' + new Date().toLocaleString('en-GB'))
  lines.push('')
  lines.push('---')
  lines.push('')

  for (const message of messages) {
    const text = textFromMessage(message)
    if (!text) continue
    const label = roleLabel(message.role)
    const time = formatTimestamp(message)
    lines.push('### ' + label + ' — ' + time)
    lines.push('')
    lines.push(text)
    lines.push('')
    lines.push('---')
    lines.push('')
  }

  return lines.join('\n')
}

function toJSON(messages: Array<GatewayMessage>, title: string): string {
  const entries = messages
    .map(function mapMessage(message) {
      const text = textFromMessage(message)
      if (!text) return null
      return {
        role: message.role || 'unknown',
        text,
        timestamp: getMessageTimestamp(message),
      }
    })
    .filter(Boolean)

  return JSON.stringify(
    {
      title,
      exportedAt: new Date().toISOString(),
      messageCount: entries.length,
      messages: entries,
    },
    null,
    2,
  )
}

function toPlainText(messages: Array<GatewayMessage>, title: string): string {
  const lines: Array<string> = []
  lines.push(title)
  lines.push('Exported on ' + new Date().toLocaleString('en-GB'))
  lines.push('')

  for (const message of messages) {
    const text = textFromMessage(message)
    if (!text) continue
    const label = roleLabel(message.role)
    const time = formatTimestamp(message)
    lines.push('[' + label + ' — ' + time + ']')
    lines.push(text)
    lines.push('')
  }

  return lines.join('\n')
}

function sanitizeFilename(name: string): string {
  return (
    name
      .replace(/[^a-zA-Z0-9 _-]/g, '')
      .replace(/\s+/g, '-')
      .slice(0, 60)
      .toLowerCase() || 'conversation'
  )
}

function downloadFile(
  content: string,
  filename: string,
  mimeType: string,
): void {
  const blob = new Blob([content], { type: mimeType + ';charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')
  anchor.href = url
  anchor.download = filename
  document.body.appendChild(anchor)
  anchor.click()
  document.body.removeChild(anchor)
  URL.revokeObjectURL(url)
}
