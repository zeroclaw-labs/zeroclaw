import { randomUUID } from 'node:crypto'
import { pushEvent } from './activity-events'
import { onGatewayEvent, gatewayConnectCheck } from './gateway'
import type { GatewayFrame } from './gateway'
import type { ActivityEvent } from '../types/activity-event'

export type ActivityStreamStatus = 'connecting' | 'connected' | 'disconnected'

export type ActivityStreamDiagnostics = {
  status: ActivityStreamStatus
  connectedSinceMs: number | null
  lastDisconnectedAtMs: number | null
}

const SENSITIVE_FIELD_KEYWORDS = [
  'apikey',
  'token',
  'secret',
  'password',
  'refresh',
]

let streamStatus: ActivityStreamStatus = 'disconnected'
let cleanupListener: (() => void) | null = null
let connectedSinceMs: number | null = null
let lastDisconnectedAtMs: number | null = null

export function getActivityStreamStatus(): ActivityStreamStatus {
  return streamStatus
}

export function getActivityStreamDiagnostics(): ActivityStreamDiagnostics {
  return {
    status: streamStatus,
    connectedSinceMs,
    lastDisconnectedAtMs,
  }
}

export function ensureActivityStreamStarted(): Promise<void> {
  if (cleanupListener) return Promise.resolve() // already listening

  return connectToGateway()
}

async function connectToGateway() {
  streamStatus = 'connecting'

  try {
    await gatewayConnectCheck()

    // Subscribe to events on the shared gateway connection
    cleanupListener = onGatewayEvent((frame: GatewayFrame) => {
      if (frame.type !== 'evt' && frame.type !== 'event') return

      const payload = parsePayload(frame)
      const eventName = (frame as any).event as string

      if (eventName === 'agent') {
        pushEvent(normalizeAgentEvent(payload))
      } else if (eventName === 'chat') {
        pushEvent(normalizeChatEvent(payload))
      } else {
        pushEvent(normalizeOtherEvent(eventName, payload))
      }
    })

    streamStatus = 'connected'
    connectedSinceMs = Date.now()

    pushEvent(
      createActivityEvent({
        type: 'gateway',
        title: 'Gateway connected',
        level: 'info',
      }),
    )
  } catch (error) {
    streamStatus = 'disconnected'
    connectedSinceMs = null
    lastDisconnectedAtMs = Date.now()
    pushEvent(normalizeErrorEvent(error))
  }
}

function parsePayload(frame: any): unknown {
  if (frame.payload !== undefined) return frame.payload
  if (typeof frame.payloadJSON === 'string') {
    try { return JSON.parse(frame.payloadJSON) } catch { return null }
  }
  return null
}

export async function reconnectActivityStream(): Promise<void> {
  if (cleanupListener) {
    cleanupListener()
    cleanupListener = null
  }
  streamStatus = 'disconnected'
  connectedSinceMs = null
  lastDisconnectedAtMs = Date.now()

  await ensureActivityStreamStarted()
}

// ── Event normalization (unchanged from original) ──────────────

function normalizeAgentEvent(payload: unknown): ActivityEvent {
  const sanitizedPayload = sanitizeValue(payload)
  const modelName = extractModelName(sanitizedPayload)

  if (modelName) {
    return createActivityEvent({
      type: 'model',
      title: `Model switched to ${modelName}`,
      detail: formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  const toolName = extractToolName(sanitizedPayload)
  if (toolName) {
    return createActivityEvent({
      type: 'tool',
      title: `Tool activity: ${toolName}`,
      detail: formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  if (hasUsageFields(sanitizedPayload)) {
    return createActivityEvent({
      type: 'usage',
      title: 'Usage updated',
      detail:
        formatUsageDetail(sanitizedPayload) ?? formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  const agentPayload = toRecord(sanitizedPayload)
  const agentData = toRecord(agentPayload?.data)
  const agentTitle = firstString([
    agentPayload?.sessionKey,
    agentPayload?.label,
    agentData?.sessionKey,
    agentData?.label,
  ])

  return createActivityEvent({
    type: 'session',
    title: agentTitle ? `Agent: ${agentTitle}` : 'Agent activity',
    detail: formatDetail(sanitizedPayload),
    level: 'info',
  })
}

function normalizeChatEvent(payload: unknown): ActivityEvent {
  const sanitizedPayload = sanitizeValue(payload)
  const payloadRecord = toRecord(sanitizedPayload)

  const errorMessage = extractErrorMessage(sanitizedPayload)
  if (errorMessage) {
    return createActivityEvent({
      type: 'error',
      title: errorMessage,
      detail: formatDetail(sanitizedPayload),
      level: 'error',
    })
  }

  if (isCronPayload(sanitizedPayload)) {
    return createActivityEvent({
      type: 'cron',
      title: 'Cron activity',
      detail: formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  const state = readString(payloadRecord?.state)
  const level =
    state === 'aborted' ? 'warn' : state === 'error' ? 'error' : 'info'

  const chatPayload = toRecord(sanitizedPayload)
  const chatData = toRecord(chatPayload?.data)
  const sessionLabel = firstString([
    chatPayload?.sessionKey,
    chatPayload?.label,
    chatData?.sessionKey,
    chatData?.label,
  ])
  let sessionTitle = sessionLabel
    ? `Session: ${sessionLabel}`
    : 'Session activity'
  if (state.length > 0) {
    sessionTitle = sessionLabel
      ? `${sessionLabel} → ${state}`
      : `Session ${state}`
  }

  return createActivityEvent({
    type: 'session',
    title: sessionTitle,
    detail: formatDetail(sanitizedPayload),
    level,
  })
}

function normalizeOtherEvent(
  eventName: string,
  payload: unknown,
): ActivityEvent {
  const sanitizedPayload = sanitizeValue(payload)
  const normalizedEventName = readString(eventName).toLowerCase()

  if (normalizedEventName.includes('error')) {
    return createActivityEvent({
      type: 'error',
      title:
        extractErrorMessage(sanitizedPayload) ??
        `Gateway event failed: ${eventName}`,
      detail: formatDetail(sanitizedPayload),
      level: 'error',
    })
  }

  if (normalizedEventName.includes('cron') || isCronPayload(sanitizedPayload)) {
    return createActivityEvent({
      type: 'cron',
      title: `Cron event: ${eventName}`,
      detail: formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  if (
    normalizedEventName.includes('tool') ||
    extractToolName(sanitizedPayload)
  ) {
    return createActivityEvent({
      type: 'tool',
      title: `Tool event: ${eventName}`,
      detail: formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  if (
    normalizedEventName.includes('usage') ||
    hasUsageFields(sanitizedPayload)
  ) {
    return createActivityEvent({
      type: 'usage',
      title: 'Usage updated',
      detail:
        formatUsageDetail(sanitizedPayload) ?? formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  const modelName = extractModelName(sanitizedPayload)
  if (normalizedEventName.includes('model') && modelName) {
    return createActivityEvent({
      type: 'model',
      title: `Model switched to ${modelName}`,
      detail: formatDetail(sanitizedPayload),
      level: 'info',
    })
  }

  return createActivityEvent({
    type: 'gateway',
    title: `Gateway event: ${eventName}`,
    detail: formatDetail(sanitizedPayload),
    level: normalizedEventName.includes('warn') ? 'warn' : 'info',
  })
}

function normalizeErrorEvent(error: unknown): ActivityEvent {
  const title = extractErrorMessage(error) ?? 'Gateway error'
  return createActivityEvent({
    type: 'error',
    title,
    level: 'error',
  })
}

function createActivityEvent(input: {
  type: ActivityEvent['type']
  title: string
  detail?: string
  level: ActivityEvent['level']
}): ActivityEvent {
  return {
    id: randomUUID(),
    timestamp: Date.now(),
    type: input.type,
    title: sanitizeText(input.title),
    detail: input.detail ? sanitizeText(input.detail) : undefined,
    level: input.level,
  }
}

function toRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object') return null
  if (Array.isArray(value)) return null
  return value as Record<string, unknown>
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function readNumber(value: unknown): number | null {
  if (typeof value !== 'number') return null
  if (!Number.isFinite(value)) return null
  return value
}

function firstString(values: Array<unknown>): string {
  for (const value of values) {
    const candidate = readString(value)
    if (candidate.length > 0) return candidate
  }
  return ''
}

function extractModelName(payload: unknown): string {
  const payloadRecord = toRecord(payload)
  const dataRecord = toRecord(payloadRecord?.data)
  const contextRecord = toRecord(payloadRecord?.context)

  return firstString([
    payloadRecord?.model,
    payloadRecord?.modelName,
    payloadRecord?.selectedModel,
    payloadRecord?.currentModel,
    dataRecord?.model,
    dataRecord?.modelName,
    dataRecord?.providerModel,
    contextRecord?.model,
  ])
}

function extractToolName(payload: unknown): string {
  const payloadRecord = toRecord(payload)
  const dataRecord = toRecord(payloadRecord?.data)

  if (readString(payloadRecord?.stream).toLowerCase() === 'tool') {
    return firstString([
      dataRecord?.name,
      payloadRecord?.toolName,
      payloadRecord?.tool,
      'Tool call',
    ])
  }

  return firstString([
    payloadRecord?.toolName,
    payloadRecord?.tool,
    payloadRecord?.name,
    dataRecord?.name,
  ])
}

function hasUsageFields(payload: unknown): boolean {
  const payloadRecord = toRecord(payload)
  const dataRecord = toRecord(payloadRecord?.data)

  const usageValues = [
    payloadRecord?.promptTokens,
    payloadRecord?.completionTokens,
    payloadRecord?.totalTokens,
    payloadRecord?.costUsd,
    dataRecord?.promptTokens,
    dataRecord?.completionTokens,
    dataRecord?.totalTokens,
    dataRecord?.costUsd,
  ]

  return usageValues.some(function hasNumber(value) {
    return readNumber(value) !== null
  })
}

function formatUsageDetail(payload: unknown): string | undefined {
  const payloadRecord = toRecord(payload)
  const dataRecord = toRecord(payloadRecord?.data)

  const promptTokens =
    readNumber(payloadRecord?.promptTokens) ??
    readNumber(dataRecord?.promptTokens)
  const completionTokens =
    readNumber(payloadRecord?.completionTokens) ??
    readNumber(dataRecord?.completionTokens)
  const totalTokens =
    readNumber(payloadRecord?.totalTokens) ??
    readNumber(dataRecord?.totalTokens)
  const costUsd =
    readNumber(payloadRecord?.costUsd) ?? readNumber(dataRecord?.costUsd)

  const parts: Array<string> = []
  if (promptTokens !== null) parts.push(`Prompt: ${promptTokens}`)
  if (completionTokens !== null) parts.push(`Completion: ${completionTokens}`)
  if (totalTokens !== null) parts.push(`Total: ${totalTokens}`)
  if (costUsd !== null) parts.push(`Cost: $${costUsd.toFixed(4)}`)

  return parts.length > 0 ? parts.join(' • ') : undefined
}

function isCronPayload(payload: unknown): boolean {
  const payloadRecord = toRecord(payload)
  const dataRecord = toRecord(payloadRecord?.data)

  const fields = [
    payloadRecord?.source,
    payloadRecord?.kind,
    payloadRecord?.task,
    payloadRecord?.type,
    dataRecord?.source,
    dataRecord?.kind,
    dataRecord?.task,
    dataRecord?.type,
  ]

  return fields.some(function hasCronField(value) {
    return readString(value).toLowerCase().includes('cron')
  })
}

function extractErrorMessage(value: unknown): string | null {
  if (value instanceof Error) {
    const message = readString(value.message)
    return message.length > 0 ? sanitizeText(message) : null
  }

  const asString = readString(value)
  if (asString.length > 0) {
    return sanitizeText(asString)
  }

  const record = toRecord(value)
  if (!record) return null

  const nestedError = toRecord(record.error)
  const nestedData = toRecord(record.data)

  const message = firstString([
    record.message,
    record.errorMessage,
    nestedError?.message,
    nestedData?.error,
    nestedData?.message,
  ])

  return message.length > 0 ? sanitizeText(message) : null
}

function formatDetail(payload: unknown): string | undefined {
  if (payload === null || payload === undefined) return undefined

  const asString = readString(payload)
  if (asString.length > 0) {
    return sanitizeText(asString)
  }

  try {
    const serialized = JSON.stringify(payload)
    if (!serialized || serialized === '{}' || serialized === '[]') {
      return undefined
    }

    if (serialized.length <= 220) {
      return serialized
    }

    return `${serialized.slice(0, 217)}...`
  } catch {
    return undefined
  }
}

function sanitizeValue(value: unknown, depth = 0): unknown {
  if (depth > 8) return '[truncated]'

  if (Array.isArray(value)) {
    return value.map(function mapItem(item) {
      return sanitizeValue(item, depth + 1)
    })
  }

  const asRecord = toRecord(value)
  if (asRecord) {
    const sanitized: Record<string, unknown> = {}

    for (const [key, fieldValue] of Object.entries(asRecord)) {
      if (containsSensitiveKeyword(key)) continue
      sanitized[key] = sanitizeValue(fieldValue, depth + 1)
    }

    return sanitized
  }

  if (typeof value === 'string') {
    return sanitizeText(value)
  }

  return value
}

function containsSensitiveKeyword(key: string): boolean {
  const normalized = key.toLowerCase()

  return SENSITIVE_FIELD_KEYWORDS.some(function hasKeyword(keyword) {
    return normalized.includes(keyword)
  })
}

export function sanitizeText(value: string): string {
  return value.replace(
    /(api[_-]?key|token|secret|password|refresh)(\s*[:=]\s*)([^\s,;]+)/gi,
    '$1$2[redacted]',
  )
}
