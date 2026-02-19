import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../../server/gateway'

type UnknownRecord = Record<string, unknown>

type BrowserStatusResponse = {
  active: boolean
  url?: string
  screenshotUrl?: string
  message?: string
}

const UNSUPPORTED_MESSAGE =
  'Browser control available when gateway supports browser RPC'
const NO_ACTIVE_SESSION_MESSAGE = 'No active browser session'

function isRecord(value: unknown): value is UnknownRecord {
  return typeof value === 'object' && value !== null
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function readBoolean(value: unknown): boolean {
  return typeof value === 'boolean' ? value : false
}

function coerceScreenshotUrl(payload: UnknownRecord): string {
  const screenshotUrl =
    readString(payload.screenshotUrl) ||
    readString(payload.imageDataUrl) ||
    readString(payload.dataUrl)
  if (screenshotUrl) return screenshotUrl

  const screenshot = readString(payload.screenshot)
  if (
    screenshot.startsWith('data:image/') ||
    screenshot.startsWith('http://') ||
    screenshot.startsWith('https://')
  ) {
    return screenshot
  }

  const image = readString(payload.image)
  if (image.startsWith('data:image/')) return image
  if (image) {
    const mimeType = readString(payload.mimeType) || 'image/png'
    return `data:${mimeType};base64,${image}`
  }

  const base64 = readString(payload.base64)
  if (base64) {
    const mimeType = readString(payload.mimeType) || 'image/png'
    return `data:${mimeType};base64,${base64}`
  }

  return ''
}

function resolveStatusRecord(payload: unknown): UnknownRecord | null {
  if (!isRecord(payload)) return null
  if (isRecord(payload.status)) return payload.status
  if (isRecord(payload.data)) return payload.data
  return payload
}

function normalizeStatusPayload(payload: unknown): BrowserStatusResponse {
  const statusRecord = resolveStatusRecord(payload)
  if (!statusRecord) {
    return {
      active: false,
      message: NO_ACTIVE_SESSION_MESSAGE,
    }
  }

  const url =
    readString(statusRecord.url) ||
    readString(statusRecord.currentUrl) ||
    readString(statusRecord.href)
  const screenshotUrl = coerceScreenshotUrl(statusRecord)
  const active =
    readBoolean(statusRecord.active) ||
    readBoolean(statusRecord.isActive) ||
    readBoolean(statusRecord.hasActiveSession) ||
    readBoolean(statusRecord.connected) ||
    Boolean(url) ||
    Boolean(screenshotUrl)

  if (!active) {
    return {
      active: false,
      message: NO_ACTIVE_SESSION_MESSAGE,
    }
  }

  return {
    active: true,
    url: url || 'about:blank',
    screenshotUrl,
  }
}

export const Route = createFileRoute('/api/browser/status')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const payload = await gatewayRpc('browser.status')
          return json(normalizeStatusPayload(payload))
        } catch {
          return json({
            active: false,
            message: UNSUPPORTED_MESSAGE,
          } satisfies BrowserStatusResponse)
        }
      },
    },
  },
})
