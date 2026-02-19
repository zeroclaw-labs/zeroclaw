import { memo, useEffect, useMemo, useRef, useState } from 'react'
import {
  ArrowDown01Icon,
  Idea01Icon,
  Wrench01Icon,
} from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  getMessageTimestamp,
  getToolCallsFromMessage,
  textFromMessage,
} from '../utils'
import { MessageActionsBar } from './message-actions-bar'
import type {
  GatewayAttachment,
  GatewayMessage,
  ToolCallContent,
} from '../types'
import type { ToolPart } from '@/components/prompt-kit/tool'
import { Message, MessageContent } from '@/components/prompt-kit/message'
import { AssistantAvatar, UserAvatar } from '@/components/avatars'
import { Tool } from '@/components/prompt-kit/tool'
import { LoadingIndicator } from '@/components/loading-indicator'
import {
  Collapsible,
  CollapsiblePanel,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  selectChatProfileAvatarDataUrl,
  selectChatProfileDisplayName,
  useChatSettingsStore,
} from '@/hooks/use-chat-settings'
import { cn } from '@/lib/utils'

// Streaming cursor component — enhanced visibility
function StreamingCursor() {
  return (
    <span className="inline-flex items-center ml-0.5">
      <span className="size-1.5 rounded-full bg-primary-600 animate-pulse" />
    </span>
  )
}

const WORDS_PER_TICK = 4
const TICK_INTERVAL_MS = 50

function isWhitespaceCharacter(value: string): boolean {
  return /\s/.test(value)
}

function countWords(text: string): number {
  let count = 0
  let inWord = false

  for (const character of text) {
    if (isWhitespaceCharacter(character)) {
      if (inWord) {
        count += 1
        inWord = false
      }
      continue
    }
    inWord = true
  }

  if (inWord) {
    count += 1
  }

  return count
}

function getWordBoundaryIndex(text: string, wordCount: number): number {
  if (text.length === 0 || wordCount <= 0) return 0

  let count = 0
  let index = 0
  let inWord = false

  while (index < text.length) {
    const character = text[index] ?? ''
    if (isWhitespaceCharacter(character)) {
      if (inWord) {
        count += 1
        if (count >= wordCount) {
          return index
        }
        inWord = false
      }
    } else {
      inWord = true
    }
    index += 1
  }

  if (inWord) {
    count += 1
    if (count >= wordCount) {
      return text.length
    }
  }

  return text.length
}

type MessageItemProps = {
  message: GatewayMessage
  toolResultsByCallId?: Map<string, GatewayMessage>
  onRetryMessage?: (message: GatewayMessage) => void
  forceActionsVisible?: boolean
  wrapperRef?: React.RefObject<HTMLDivElement | null>
  wrapperClassName?: string
  wrapperDataMessageId?: string
  wrapperScrollMarginTop?: number
  bubbleClassName?: string
  isStreaming?: boolean
  streamingText?: string
  streamingThinking?: string
  simulateStreaming?: boolean
  streamingKey?: string | null
  expandAllToolSections?: boolean
}

function mapToolCallToToolPart(
  toolCall: ToolCallContent,
  resultMessage: GatewayMessage | undefined,
): ToolPart {
  const hasResult = resultMessage !== undefined
  const isError = resultMessage?.isError ?? false

  let state: ToolPart['state']
  if (!hasResult) {
    state = 'input-available'
  } else if (isError) {
    state = 'output-error'
  } else {
    state = 'output-available'
  }

  // Extract error text from result message content
  let errorText: string | undefined
  if (isError && resultMessage?.content?.[0]?.type === 'text') {
    errorText = resultMessage.content[0].text || 'Unknown error'
  }

  return {
    type: toolCall.name || 'unknown',
    state,
    input: toolCall.arguments,
    output: resultMessage?.details,
    toolCallId: toolCall.id,
    errorText,
  }
}

function toolCallsSignature(message: GatewayMessage): string {
  const toolCalls = getToolCallsFromMessage(message)
  return toolCalls
    .map((toolCall) => {
      const id = toolCall.id ?? ''
      const name = toolCall.name ?? ''
      const partialJson = toolCall.partialJson ?? ''
      const args = toolCall.arguments ? JSON.stringify(toolCall.arguments) : ''
      return `${id}|${name}|${partialJson}|${args}`
    })
    .join('||')
}

function toolResultSignature(result: GatewayMessage | undefined): string {
  if (!result) return 'missing'
  const content = Array.isArray(result.content) ? result.content : []
  const text = content
    .map((part) => (part.type === 'text' ? String(part.text ?? '') : ''))
    .join('')
    .trim()
  const details = result.details ? JSON.stringify(result.details) : ''
  return `${result.toolCallId ?? ''}|${result.toolName ?? ''}|${result.isError ? '1' : '0'}|${text}|${details}`
}

function toolResultsSignature(
  message: GatewayMessage,
  toolResultsByCallId: Map<string, GatewayMessage> | undefined,
): string {
  if (!toolResultsByCallId) return ''
  const toolCalls = getToolCallsFromMessage(message)
  if (toolCalls.length === 0) return ''
  return toolCalls
    .map((toolCall) => {
      if (!toolCall.id) return 'missing'
      return toolResultSignature(toolResultsByCallId.get(toolCall.id))
    })
    .join('||')
}

function normalizeTimestamp(value: unknown): number | null {
  if (typeof value === 'number' && Number.isFinite(value)) {
    if (value < 1_000_000_000_000) return value * 1000
    return value
  }
  if (typeof value === 'string') {
    const parsed = Date.parse(value)
    if (!Number.isNaN(parsed)) return parsed
  }
  return null
}

function rawTimestamp(message: GatewayMessage): number | null {
  const candidates = [
    (message as any).createdAt,
    (message as any).created_at,
    (message as any).timestamp,
    (message as any).time,
    (message as any).ts,
  ]
  for (const candidate of candidates) {
    const normalized = normalizeTimestamp(candidate)
    if (normalized) return normalized
  }
  return null
}

function thinkingFromMessage(msg: GatewayMessage): string | null {
  const parts = Array.isArray(msg.content) ? msg.content : []
  const thinkingPart = parts.find((part) => part.type === 'thinking')
  if (thinkingPart && 'thinking' in thinkingPart) {
    return String(thinkingPart.thinking ?? '')
  }
  return null
}

function summarizeToolNames(toolCalls: Array<ToolCallContent>): string {
  const seen = new Set<string>()
  const uniqueNames: Array<string> = []
  for (const toolCall of toolCalls) {
    const normalized = (toolCall.name || '').trim()
    const name = normalized.length > 0 ? normalized : 'unknown'
    if (seen.has(name)) continue
    seen.add(name)
    uniqueNames.push(name)
  }
  if (uniqueNames.length === 0) return 'tools'
  if (uniqueNames.length <= 3) return uniqueNames.join(', ')
  return `${uniqueNames.slice(0, 3).join(', ')} +${uniqueNames.length - 3} more`
}

function attachmentSource(attachment: GatewayAttachment | undefined): string {
  if (!attachment) return ''
  const candidates = [attachment.previewUrl, attachment.dataUrl, attachment.url]
  for (const candidate of candidates) {
    if (typeof candidate === 'string' && candidate.trim().length > 0) {
      return candidate
    }
  }
  return ''
}

function MessageItemComponent({
  message,
  toolResultsByCallId,
  onRetryMessage,
  forceActionsVisible = false,
  wrapperRef,
  wrapperClassName,
  wrapperDataMessageId,
  wrapperScrollMarginTop,
  bubbleClassName,
  isStreaming = false,
  streamingText,
  streamingThinking,
  simulateStreaming: _simulateStreaming = false,
  streamingKey: _streamingKey,
  expandAllToolSections = false,
}: MessageItemProps) {
  const role = message.role || 'assistant'
  const profileDisplayName = useChatSettingsStore(selectChatProfileDisplayName)
  const profileAvatarDataUrl = useChatSettingsStore(
    selectChatProfileAvatarDataUrl,
  )

  const messageStreamingText =
    typeof message.__streamingText === 'string'
      ? message.__streamingText
      : undefined
  const messageStreamingThinking =
    typeof message.__streamingThinking === 'string'
      ? message.__streamingThinking
      : undefined
  const remoteStreamingText =
    streamingText !== undefined ? streamingText : messageStreamingText
  const remoteStreamingThinking =
    streamingThinking !== undefined
      ? streamingThinking
      : messageStreamingThinking
  // Only treat as streaming if explicitly passed isStreaming prop (active stream)
  // Ignore stale __streamingStatus from history
  const remoteStreamingActive = isStreaming === true

  const fullText = useMemo(() => textFromMessage(message), [message])
  const initialDisplayText = remoteStreamingActive
    ? (remoteStreamingText ?? fullText)
    : fullText
  const [displayText, setDisplayText] = useState(() => initialDisplayText)
  const [revealedWordCount, setRevealedWordCount] = useState(() =>
    remoteStreamingActive || _simulateStreaming
      ? 0
      : countWords(initialDisplayText),
  )
  const [revealedText, setRevealedText] = useState(() =>
    remoteStreamingActive || _simulateStreaming ? '' : initialDisplayText,
  )
  const revealTimerRef = useRef<number | null>(null)
  const targetWordCountRef = useRef(countWords(initialDisplayText))
  const previousTextRef = useRef(initialDisplayText)
  const previousTextLengthRef = useRef(initialDisplayText.length)

  // Track if this is a newly appeared message (for fade-in animation)
  const isNewRef = useRef(true)
  const [isNew, setIsNew] = useState(true)
  useEffect(() => {
    if (!isNewRef.current) return
    isNewRef.current = false
    const timer = window.setTimeout(() => setIsNew(false), 600)
    return () => window.clearTimeout(timer)
  }, [])

  useEffect(() => {
    if (remoteStreamingActive) {
      setDisplayText(remoteStreamingText ?? fullText)
      return
    }

    setDisplayText((current) => (current === fullText ? current : fullText))
  }, [remoteStreamingActive, remoteStreamingText, fullText])

  // Reset word count when simulate streaming starts for a new message
  useEffect(() => {
    if (_simulateStreaming && !remoteStreamingActive) {
      setRevealedWordCount(0)
    }
  }, [_streamingKey, _simulateStreaming, remoteStreamingActive])

  useEffect(() => {
    return () => {
      if (revealTimerRef.current !== null) {
        window.clearInterval(revealTimerRef.current)
      }
    }
  }, [])

  // Simulate streaming is only active while words are still being revealed
  const totalWords = countWords(displayText)
  const revealComplete = revealedWordCount >= totalWords && totalWords > 0
  const effectiveIsStreaming =
    remoteStreamingActive || (_simulateStreaming && !revealComplete)
  const assistantDisplayText = effectiveIsStreaming ? revealedText : displayText

  useEffect(() => {
    const totalWords = countWords(displayText)
    const previousText = previousTextRef.current
    const previousLength = previousTextLengthRef.current
    const textGrew =
      displayText.length > previousLength &&
      displayText.startsWith(previousText)
    const textChanged = displayText !== previousText

    targetWordCountRef.current = totalWords
    previousTextRef.current = displayText
    previousTextLengthRef.current = displayText.length

    if (!effectiveIsStreaming) {
      if (revealTimerRef.current !== null) {
        window.clearInterval(revealTimerRef.current)
        revealTimerRef.current = null
      }
      setRevealedWordCount(totalWords)
      return
    }

    if (textChanged && !textGrew) {
      setRevealedWordCount(totalWords)
      return
    }

    if (revealTimerRef.current !== null) {
      return
    }

    // Don't start animation if already fully revealed
    setRevealedWordCount((currentWordCount) => {
      if (currentWordCount >= totalWords) {
        return currentWordCount
      }

      function tick() {
        setRevealedWordCount((currentWordCount) => {
          const targetWordCount = targetWordCountRef.current
          if (currentWordCount >= targetWordCount) {
            if (revealTimerRef.current !== null) {
              window.clearInterval(revealTimerRef.current)
              revealTimerRef.current = null
            }
            return currentWordCount
          }

          const nextWordCount = Math.min(
            targetWordCount,
            currentWordCount + WORDS_PER_TICK,
          )

          if (
            nextWordCount >= targetWordCount &&
            revealTimerRef.current !== null
          ) {
            window.clearInterval(revealTimerRef.current)
            revealTimerRef.current = null
          }

          return nextWordCount
        })
      }

      revealTimerRef.current = window.setInterval(tick, TICK_INTERVAL_MS)
      return currentWordCount
    })
  }, [displayText, effectiveIsStreaming])

  useEffect(() => {
    if (!effectiveIsStreaming) {
      setRevealedText((currentText) =>
        currentText === displayText ? currentText : displayText,
      )
      return
    }

    const boundaryIndex = getWordBoundaryIndex(displayText, revealedWordCount)
    const nextRevealedText = displayText.slice(0, boundaryIndex)
    setRevealedText((currentText) =>
      currentText === nextRevealedText ? currentText : nextRevealedText,
    )
  }, [displayText, effectiveIsStreaming, revealedWordCount])

  const thinking =
    remoteStreamingActive && remoteStreamingThinking !== undefined
      ? remoteStreamingThinking
      : thinkingFromMessage(message)
  const isUser = role === 'user'
  const timestamp = getMessageTimestamp(message)
  const attachments = Array.isArray(message.attachments)
    ? message.attachments.filter(
        (attachment) => attachmentSource(attachment).length > 0,
      )
    : []
  const hasAttachments = attachments.length > 0

  // Extract inline images from content array (gateway sends images as content blocks)
  const inlineImages = useMemo(() => {
    const parts = Array.isArray(message.content) ? message.content : []
    return parts
      .filter((p: any) => p.type === 'image' && p.source)
      .map((p: any, i: number) => {
        const src =
          p.source?.type === 'base64' && p.source?.data
            ? `data:${p.source.media_type || 'image/jpeg'};base64,${p.source.data}`
            : p.source?.url || p.url || ''
        return { id: `inline-img-${i}`, src }
      })
      .filter((img) => img.src.length > 0)
  }, [message.content])
  const hasInlineImages = inlineImages.length > 0

  const hasText = displayText.length > 0
  const hasRevealedText = effectiveIsStreaming ? assistantDisplayText.length > 0 : hasText
  const canRetryMessage = isUser && (hasText || hasAttachments || hasInlineImages)

  // Get tool calls from this message (for assistant messages)
  const toolCalls = role === 'assistant' ? getToolCallsFromMessage(message) : []
  const hasToolCalls = toolCalls.length > 0
  const toolParts = useMemo(() => {
    return toolCalls.map((toolCall) => {
      const resultMessage = toolCall.id
        ? toolResultsByCallId?.get(toolCall.id)
        : undefined
      return mapToolCallToToolPart(toolCall, resultMessage)
    })
  }, [toolCalls, toolResultsByCallId])
  const toolSummary = useMemo(() => {
    if (!hasToolCalls) return ''
    const count = toolCalls.length
    const toolLabel = count === 1 ? 'tool' : 'tools'
    return `Used: ${summarizeToolNames(toolCalls)} (${count} ${toolLabel})`
  }, [hasToolCalls, toolCalls])
  const hasToolErrors = useMemo(
    () => toolParts.some((toolPart) => toolPart.state === 'output-error'),
    [toolParts],
  )
  const [toolCallsOpen, setToolCallsOpen] = useState(false)

  useEffect(() => {
    if (expandAllToolSections) {
      setToolCallsOpen(true)
    }
  }, [expandAllToolSections])

  // Never show "Queued" — messages are sent instantly to the gateway.
  // The old "sending" status was misleading since the API call takes <100ms.
  const isQueued = false
  const isFailed = message.status === 'error'

  return (
    <div
      ref={wrapperRef}
      data-chat-message-role={role}
      data-chat-message-id={wrapperDataMessageId}
      style={
        typeof wrapperScrollMarginTop === 'number'
          ? { scrollMarginTop: `${wrapperScrollMarginTop}px` }
          : undefined
      }
      className={cn(
        'group flex flex-col',
        hasText || hasAttachments ? 'gap-0.5 md:gap-1' : 'gap-0',
        wrapperClassName,
        isUser ? 'items-end' : 'items-start',
        !isUser && isNew && 'animate-[message-fade-in_0.4s_ease-out]',
      )}
    >
      {thinking && !hasText && (
        <div className="w-full max-w-[900px]">
          <Collapsible defaultOpen={false}>
            <CollapsibleTrigger className="w-fit">
              <HugeiconsIcon
                icon={Idea01Icon}
                size={20}
                strokeWidth={1.5}
                className="opacity-70"
              />
              <span>💭 Thinking...</span>
              {effectiveIsStreaming ? (
                <LoadingIndicator ariaLabel="Assistant thinking" />
              ) : null}
              <HugeiconsIcon
                icon={ArrowDown01Icon}
                size={20}
                strokeWidth={1.5}
                className="opacity-60 transition-transform duration-150 group-data-panel-open:rotate-180"
              />
            </CollapsibleTrigger>
            <CollapsiblePanel>
              <div className="rounded-md border border-primary-200 bg-primary-50 p-3">
                <p className="text-sm text-primary-700 whitespace-pre-wrap text-pretty">
                  {thinking}
                </p>
              </div>
            </CollapsiblePanel>
          </Collapsible>
        </div>
      )}
      {/* Narration messages (tool-call activity) — compact collapsible row */}
      {!isUser && (message as any).__isNarration && hasText && (
        <div className="w-full max-w-[900px]">
          <details className="group/narration rounded-lg border border-primary-200/50 bg-primary-50/30 hover:bg-primary-50/50 transition-colors">
            <summary className="flex items-center gap-2 cursor-pointer select-none px-3 py-2 list-none [&::-webkit-details-marker]:hidden">
              <span className="size-6 flex items-center justify-center rounded-full bg-accent-500/15 shrink-0">
                <span className="text-xs">⚡</span>
              </span>
              <span className="text-xs font-medium truncate flex-1 text-primary-700">
                {displayText.slice(0, 120)}
                {displayText.length > 120 ? '...' : ''}
              </span>
              <HugeiconsIcon
                icon={ArrowDown01Icon}
                size={16}
                strokeWidth={1.5}
                className="text-primary-400 shrink-0 transition-transform group-open/narration:rotate-180"
              />
            </summary>
            <div className="px-3 pb-3 pt-1 text-[13px] text-primary-600 whitespace-pre-wrap text-pretty max-h-[400px] overflow-y-auto">
              {displayText}
            </div>
          </details>
        </div>
      )}
      {(hasText || hasAttachments || hasInlineImages || effectiveIsStreaming) &&
        !(message as any).__isNarration && (
          <Message className={cn('gap-2 md:gap-3', isUser ? 'flex-row-reverse' : '')}>
            {isUser ? (
              <UserAvatar
                size={24}
                className="mt-0.5"
                src={profileAvatarDataUrl}
                alt={profileDisplayName}
              />
            ) : (
              <AssistantAvatar size={24} className="mt-0.5" />
            )}
            <div
              data-chat-message-bubble={isUser ? 'true' : undefined}
              className={cn(
                'rounded-[12px] break-words whitespace-normal min-w-0 text-primary-900 flex flex-col gap-2',
                '',
                !isUser
                  ? 'bg-transparent w-full'
                  : 'bg-primary-100 max-w-[75%] rounded-2xl px-3 py-2 md:px-4 md:py-2.5',
                isQueued && isUser && !isFailed && 'opacity-70',
                isFailed && isUser && 'bg-red-50/50 border border-red-300',
                bubbleClassName,
              )}
            >
              {hasAttachments && (
                <div className="flex flex-wrap gap-2">
                  {attachments.map((attachment) => (
                    <a
                      key={attachment.id}
                      href={attachmentSource(attachment)}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="block overflow-hidden rounded-lg border border-primary-200 hover:border-primary-400 transition-colors max-w-full"
                    >
                      <img
                        src={attachmentSource(attachment)}
                        alt={attachment.name || 'Attached image'}
                        className="max-h-64 w-auto max-w-full object-contain"
                        loading="lazy"
                      />
                    </a>
                  ))}
                </div>
              )}
              {hasInlineImages && (
                <div className="flex flex-wrap gap-2">
                  {inlineImages.map((img) => (
                    <a
                      key={img.id}
                      href={img.src}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="block overflow-hidden rounded-lg border border-primary-200 hover:border-primary-400 transition-colors max-w-full"
                    >
                      <img
                        src={img.src}
                        alt="Shared image"
                        className="max-h-64 w-auto max-w-full object-contain"
                        loading="lazy"
                      />
                    </a>
                  ))}
                </div>
              )}
              {hasText &&
                (isUser ? (
                  <span className="text-pretty max-h-[600px] overflow-y-auto">
                    {displayText}
                  </span>
                ) : hasRevealedText ? (
                  <div className="relative max-h-[800px] overflow-y-auto">
                    <MessageContent
                      markdown
                      className={cn(
                        'text-primary-900 bg-transparent w-full text-pretty',
                        effectiveIsStreaming && 'chat-streaming-content',
                      )}
                    >
                      {assistantDisplayText}
                    </MessageContent>
                    {effectiveIsStreaming && <StreamingCursor />}
                  </div>
                ) : null)}
              {effectiveIsStreaming && !hasRevealedText && (
                <div className="flex items-center gap-1.5 py-1">
                  <LoadingIndicator ariaLabel="Assistant is responding" />
                </div>
              )}
            </div>
          </Message>
        )}

      {/* Render tool calls - only when message is tool-call-only (no text) */}
      {hasToolCalls && !hasText && (
        <div className="w-full max-w-[900px] mt-2">
          <Collapsible open={toolCallsOpen} onOpenChange={setToolCallsOpen}>
            <CollapsibleTrigger className="w-full justify-between bg-primary-50/50 hover:bg-primary-100/80 data-panel-open:bg-primary-100/80">
              <span
                className={cn(
                  'size-2 shrink-0 rounded-full',
                  hasToolErrors ? 'bg-red-500' : 'bg-emerald-500',
                )}
              />
              <HugeiconsIcon
                icon={Wrench01Icon}
                size={20}
                strokeWidth={1.5}
                className="opacity-70"
              />
              <span className="flex-1 truncate text-left">{toolSummary}</span>
              <HugeiconsIcon
                icon={ArrowDown01Icon}
                size={20}
                strokeWidth={1.5}
                className="text-primary-700 transition-transform duration-150 group-data-panel-open:rotate-180"
              />
            </CollapsibleTrigger>
            <CollapsiblePanel>
              <div className="flex flex-col gap-3 border-l-2 border-primary-200 pl-2">
                {toolParts.map((toolPart, index) => (
                  <Tool
                    key={toolPart.toolCallId || `${toolPart.type}-${index}`}
                    toolPart={toolPart}
                    defaultOpen={false}
                  />
                ))}
              </div>
            </CollapsiblePanel>
          </Collapsible>
        </div>
      )}

      {(!hasToolCalls || hasText) && (
        <MessageActionsBar
          text={fullText}
          timestamp={timestamp}
          align={isUser ? 'end' : 'start'}
          forceVisible={forceActionsVisible}
          isQueued={isUser && isQueued && !isFailed}
          isFailed={isUser && isFailed}
          onRetry={
            canRetryMessage && (isQueued || isFailed) && onRetryMessage
              ? () => onRetryMessage(message)
              : undefined
          }
        />
      )}
    </div>
  )
}

function areMessagesEqual(
  prevProps: MessageItemProps,
  nextProps: MessageItemProps,
): boolean {
  if (prevProps.forceActionsVisible !== nextProps.forceActionsVisible) {
    return false
  }
  if (prevProps.wrapperClassName !== nextProps.wrapperClassName) return false
  if (prevProps.onRetryMessage !== nextProps.onRetryMessage) return false
  if (prevProps.wrapperDataMessageId !== nextProps.wrapperDataMessageId) {
    return false
  }
  if (prevProps.wrapperRef !== nextProps.wrapperRef) return false
  if (prevProps.wrapperScrollMarginTop !== nextProps.wrapperScrollMarginTop) {
    return false
  }
  if (prevProps.bubbleClassName !== nextProps.bubbleClassName) return false
  // Check streaming state
  if (prevProps.isStreaming !== nextProps.isStreaming) {
    return false
  }
  if (prevProps.streamingText !== nextProps.streamingText) {
    return false
  }
  if (prevProps.streamingThinking !== nextProps.streamingThinking) {
    return false
  }
  if (prevProps.simulateStreaming !== nextProps.simulateStreaming) {
    return false
  }
  if (prevProps.streamingKey !== nextProps.streamingKey) {
    return false
  }
  if (prevProps.expandAllToolSections !== nextProps.expandAllToolSections) {
    return false
  }
  if (
    prevProps.message.__streamingStatus !== nextProps.message.__streamingStatus
  ) {
    return false
  }
  if (prevProps.message.__streamingText !== nextProps.message.__streamingText) {
    return false
  }
  if (
    prevProps.message.__streamingThinking !==
    nextProps.message.__streamingThinking
  ) {
    return false
  }
  if (
    (prevProps.message.role || 'assistant') !==
    (nextProps.message.role || 'assistant')
  ) {
    return false
  }
  if (
    textFromMessage(prevProps.message) !== textFromMessage(nextProps.message)
  ) {
    return false
  }
  if (
    thinkingFromMessage(prevProps.message) !==
    thinkingFromMessage(nextProps.message)
  ) {
    return false
  }
  if (
    toolCallsSignature(prevProps.message) !==
    toolCallsSignature(nextProps.message)
  ) {
    return false
  }
  if (
    toolResultsSignature(prevProps.message, prevProps.toolResultsByCallId) !==
    toolResultsSignature(nextProps.message, nextProps.toolResultsByCallId)
  ) {
    return false
  }
  if (rawTimestamp(prevProps.message) !== rawTimestamp(nextProps.message)) {
    return false
  }
  // Check attachments
  const prevAttachments = Array.isArray(prevProps.message.attachments)
    ? prevProps.message.attachments
    : []
  const nextAttachments = Array.isArray(nextProps.message.attachments)
    ? nextProps.message.attachments
    : []
  if (prevAttachments.length !== nextAttachments.length) {
    return false
  }
  // No need to check settings here as the hook will cause a re-render
  // and areMessagesEqual is for props only.
  // However, memo components with hooks will re-render if the hook state changes.
  return true
}

const MemoizedMessageItem = memo(MessageItemComponent, areMessagesEqual)

export { MemoizedMessageItem as MessageItem }
