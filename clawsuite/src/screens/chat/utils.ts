import type {
  GatewayMessage,
  SessionMeta,
  SessionSummary,
  SessionTitleSource,
  SessionTitleStatus,
  ToolCallContent,
} from './types'

export function deriveFriendlyIdFromKey(key: string | undefined): string {
  if (!key) return 'main'
  const trimmed = key.trim()
  if (trimmed.length === 0) return 'main'
  const parts = trimmed.split(':')
  const tail = parts[parts.length - 1] ?? ''
  const tailTrimmed = tail.trim()
  return tailTrimmed.length > 0 ? tailTrimmed : trimmed
}

/**
 * Strip channel prefixes like "[2026-02-11 14:00 Telegram]" from messages.
 * These are added by the gateway for multi-channel routing.
 */
const CHANNEL_PREFIX_REGEX = /^\[([^\]]+)\]\s*/
const KNOWN_CHANNELS = [
  'WebChat',
  'WhatsApp',
  'Telegram',
  'Signal',
  'Slack',
  'Discord',
  'iMessage',
  'Teams',
  'GoogleChat',
]

function stripChannelPrefix(text: string): string {
  const match = text.match(CHANNEL_PREFIX_REGEX)
  if (!match) return text
  const bracket = match[1] ?? ''
  // Strip if it contains a timestamp or known channel name
  const hasTimestamp =
    /\d{4}-\d{2}-\d{2}/.test(bracket) || /\d{2}:\d{2}/.test(bracket)
  const hasChannel = KNOWN_CHANNELS.some((ch) => bracket.includes(ch))
  if (hasTimestamp || hasChannel) return text.slice(match[0].length)
  return text
}

/**
 * Strip OpenClaw system metadata from user messages.
 * Removes [media attached: ...] blocks, image-send instructions,
 * and [Telegram/Signal/etc ...] headers, leaving just the user's text.
 */
function cleanUserText(raw: string): string {
  let text = raw

  // Remove "Conversation info (untrusted metadata):" headers + JSON block
  // Format: "Conversation info (untrusted metadata):\n```json\n{...}\n```\n\n"
  text = text.replace(
    /Conversation info \(untrusted metadata\):\s*```json[\s\S]*?```\s*/gi,
    '',
  )

  // Remove timestamp prefixes like "[Fri 2026-02-13 10:45 EST]"
  text = text.replace(
    /^\[[A-Z][a-z]{2}\s+\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}\s+[A-Z]{3}\]\s*/gm,
    '',
  )

  // Remove [media attached: ...] blocks (may span multiple lines)
  text = text.replace(/\[media attached:[^\]]*\]\s*/gi, '')

  // Remove "To send an image back..." instruction block
  text = text.replace(
    /To send an image back.*?Keep caption in the text body\.\s*/gs,
    '',
  )

  // Extract user message after channel header like [Telegram ... EST]
  const channelHeaderMatch = text.match(
    /\[(?:Telegram|Signal|Discord|WhatsApp|iMessage|Slack|GoogleChat)\s[^\]]*\]\s*([\s\S]*)/i,
  )
  if (channelHeaderMatch) {
    text = channelHeaderMatch[1]
  }

  // Remove <media:audio> / <media:image> / <media:video> tags
  text = text.replace(/<media:\w+>/gi, '')

  // Remove System: [...] prefix messages (exec completions, heartbeat prompts)
  text = text.replace(/^System:\s*\[[^\]]*\]\s*/i, '')

  // Remove heartbeat prompt text
  text = text.replace(
    /Read HEARTBEAT\.md if it exists.*?reply HEARTBEAT_OK\.\s*/gs,
    '',
  )

  return text.trim()
}

export function textFromMessage(msg: GatewayMessage): string {
  const parts = Array.isArray(msg.content) ? msg.content : []
  const raw = parts
    .map((part) => (part.type === 'text' ? String(part.text ?? '') : ''))
    .join('')
    .trim()

  // Clean user messages (strip system metadata)
  if (msg.role === 'user') {
    return stripChannelPrefix(cleanUserText(raw))
  }

  // Clean assistant messages (strip reply tags and channel prefixes)
  const cleaned = raw.replace(/\[\[reply_to(?:_current|:\d+)\]\]/g, '').trim()
  return stripChannelPrefix(cleaned)
}

export function getToolCallsFromMessage(
  msg: GatewayMessage,
): Array<ToolCallContent> {
  const parts = Array.isArray(msg.content) ? msg.content : []
  return parts.filter(
    (part): part is ToolCallContent => part.type === 'toolCall',
  )
}

export function findToolResultForCall(
  toolCallId: string,
  messages: Array<GatewayMessage>,
): GatewayMessage | undefined {
  return messages.find(
    (msg) => msg.role === 'toolResult' && msg.toolCallId === toolCallId,
  )
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

export function getMessageTimestamp(message: GatewayMessage): number {
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

  return Date.now()
}

function deriveTitleStatus(
  label?: string,
  explicitTitle?: string,
  derivedTitle?: string,
  providedStatus?: SessionTitleStatus,
): SessionTitleStatus {
  if (providedStatus) return providedStatus
  if (label || explicitTitle || derivedTitle) return 'ready'
  return 'idle'
}

function deriveTitleSource(
  label?: string,
  explicitTitle?: string,
  derivedTitle?: string,
  providedSource?: SessionTitleSource,
): SessionTitleSource | undefined {
  if (providedSource) return providedSource
  if (label || explicitTitle) return 'manual'
  if (derivedTitle) return 'auto'
  return undefined
}

export function normalizeSessions(
  rows: Array<SessionSummary> | undefined,
): Array<SessionMeta> {
  if (!Array.isArray(rows)) return []
  return rows.map((session) => {
    const key =
      typeof session.key === 'string' && session.key.trim().length > 0
        ? session.key.trim()
        : deriveFriendlyIdFromKey(session.friendlyId ?? session.key)
    const friendlyIdCandidate =
      typeof session.friendlyId === 'string' &&
      session.friendlyId.trim().length > 0
        ? session.friendlyId.trim()
        : deriveFriendlyIdFromKey(key)

    const label =
      typeof session.label === 'string' && session.label.trim().length > 0
        ? session.label.trim()
        : undefined
    const explicitTitle =
      typeof session.title === 'string' && session.title.trim().length > 0
        ? session.title.trim()
        : undefined
    const derivedTitle =
      typeof session.derivedTitle === 'string' &&
      session.derivedTitle.trim().length > 0
        ? session.derivedTitle.trim()
        : undefined
    const titleStatus = deriveTitleStatus(
      label,
      explicitTitle,
      derivedTitle,
      session.titleStatus,
    )
    const titleSource = deriveTitleSource(
      label,
      explicitTitle,
      derivedTitle,
      session.titleSource,
    )

    return {
      key,
      friendlyId: friendlyIdCandidate,
      title: explicitTitle,
      derivedTitle,
      label,
      updatedAt:
        typeof session.updatedAt === 'number' ? session.updatedAt : undefined,
      lastMessage: session.lastMessage ?? null,
      titleStatus,
      titleSource,
      titleError: session.titleError ?? null,
    }
  })
}

export async function readError(res: Response): Promise<string> {
  try {
    const data = await res.json()
    if (data?.error) return String(data.error)
    if (data?.message) return String(data.message)
    return JSON.stringify(data)
  } catch {
    try {
      return await res.text()
    } catch {
      return res.statusText || 'Request failed'
    }
  }
}

export const missingGatewayAuthMessage =
  'Missing gateway auth. Set CLAWDBOT_GATEWAY_TOKEN (recommended) or CLAWDBOT_GATEWAY_PASSWORD in the server environment.'

export function isMissingGatewayAuth(message: string): boolean {
  return message.includes(missingGatewayAuthMessage)
}
