import { gatewayRpc } from './gateway'

type UnknownRecord = Record<string, unknown>

export type BrowserTab = {
  id: string
  title: string
  url: string
  isActive: boolean
}

export type BrowserTabsResponse = {
  ok: boolean
  tabs: Array<BrowserTab>
  activeTabId: string | null
  updatedAt: string
  demoMode: boolean
  error?: string
  gatewaySupportRequired?: boolean
}

export type BrowserScreenshotResponse = {
  ok: boolean
  imageDataUrl: string
  currentUrl: string
  activeTabId: string | null
  capturedAt: string
  demoMode: boolean
  error?: string
  gatewaySupportRequired?: boolean
}

const BROWSER_TABS_METHODS = [
  'browser.tabs',
  'browser.list_tabs',
  'browser.get_tabs',
]

const BROWSER_SCREENSHOT_METHODS = [
  'browser.screenshot',
  'browser.capture',
  'browser.take_screenshot',
]

function isRecord(value: unknown): value is UnknownRecord {
  return typeof value === 'object' && value !== null
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

function readBoolean(value: unknown): boolean {
  return typeof value === 'boolean' ? value : false
}

function readErrorReason(error: unknown): string | undefined {
  if (error instanceof Error) return error.message
  if (typeof error === 'string') return error
  return undefined
}

const GATEWAY_SUPPORT_ERROR_PATTERNS = [
  'missing gateway auth',
  'gateway connection closed',
  'connect econnrefused',
  'method not found',
  'unknown method',
  'not implemented',
  'unsupported',
  'browser api unavailable',
  'browser tool request failed',
]

function isGatewaySupportRequired(error: unknown): boolean {
  const reason = readErrorReason(error)
  if (!reason) return false

  const normalizedReason = reason.toLowerCase()
  return GATEWAY_SUPPORT_ERROR_PATTERNS.some(function hasPattern(pattern) {
    return normalizedReason.includes(pattern)
  })
}

function escapeHtml(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
}

function getDemoTabs(): Array<BrowserTab> {
  return [
    {
      id: 'demo-tab-1',
      title: 'ClawSuite',
      url: 'https://openclaw.local/studio',
      isActive: true,
    },
    {
      id: 'demo-tab-2',
      title: 'Gateway Status',
      url: 'https://openclaw.local/gateway/status',
      isActive: false,
    },
    {
      id: 'demo-tab-3',
      title: 'Agent Documentation',
      url: 'https://docs.openclaw.local/agents',
      isActive: false,
    },
  ]
}

function buildDemoScreenshotUrl(
  url: string,
  title: string,
  timestampIso: string,
): string {
  const escapedUrl = escapeHtml(url)
  const escapedTitle = escapeHtml(title)
  const escapedTime = escapeHtml(
    new Date(timestampIso).toLocaleTimeString(undefined, {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
    }),
  )

  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="900" viewBox="0 0 1600 900">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#0f172a" />
      <stop offset="100%" stop-color="#1e293b" />
    </linearGradient>
  </defs>
  <rect width="1600" height="900" fill="url(#bg)" />
  <rect x="96" y="96" width="1408" height="708" rx="24" fill="#0b1220" stroke="#334155" stroke-width="2" />
  <rect x="96" y="96" width="1408" height="58" rx="24" fill="#111827" />
  <circle cx="130" cy="125" r="8" fill="#ef4444" />
  <circle cx="156" cy="125" r="8" fill="#f59e0b" />
  <circle cx="182" cy="125" r="8" fill="#10b981" />
  <rect x="220" y="110" width="1180" height="30" rx="10" fill="#1f2937" />
  <text x="238" y="131" fill="#cbd5e1" font-family="ui-monospace, SFMono-Regular, Menlo, monospace" font-size="14">${escapedUrl}</text>
  <text x="140" y="250" fill="#f8fafc" font-family="ui-sans-serif, system-ui, sans-serif" font-size="46">Demo Browser Feed</text>
  <text x="140" y="306" fill="#94a3b8" font-family="ui-sans-serif, system-ui, sans-serif" font-size="24">${escapedTitle}</text>
  <rect x="140" y="350" width="480" height="132" rx="16" fill="#111827" stroke="#334155" />
  <text x="170" y="398" fill="#e2e8f0" font-family="ui-sans-serif, system-ui, sans-serif" font-size="24">Gateway browser API unavailable</text>
  <text x="170" y="434" fill="#94a3b8" font-family="ui-sans-serif, system-ui, sans-serif" font-size="20">Showing demo screenshot fallback</text>
  <text x="170" y="470" fill="#94a3b8" font-family="ui-sans-serif, system-ui, sans-serif" font-size="20">Captured ${escapedTime}</text>
</svg>`

  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`
}

function normalizeTab(tab: unknown, index: number): BrowserTab {
  if (!isRecord(tab)) {
    return {
      id: `tab-${index + 1}`,
      title: `Tab ${index + 1}`,
      url: 'about:blank',
      isActive: false,
    }
  }

  const id =
    readString(tab.id) ||
    readString(tab.tabId) ||
    readString(tab.key) ||
    `tab-${index + 1}`

  return {
    id,
    title: readString(tab.title) || `Tab ${index + 1}`,
    url: readString(tab.url) || readString(tab.href) || 'about:blank',
    isActive: readBoolean(tab.active) || readBoolean(tab.isActive),
  }
}

async function callGatewayRpcWithFallbackMethods(
  methods: Array<string>,
  params?: unknown,
): Promise<unknown> {
  let lastError: unknown = null
  for (const method of methods) {
    try {
      return await gatewayRpc(method, params)
    } catch (error) {
      lastError = error
    }
  }

  if (lastError instanceof Error) throw lastError
  throw new Error('Gateway browser tool request failed')
}

function coerceImageDataUrl(payload: UnknownRecord): string {
  const imageDataUrl = readString(payload.imageDataUrl)
  if (imageDataUrl) return imageDataUrl

  const dataUrl = readString(payload.dataUrl)
  if (dataUrl) return dataUrl

  const screenshot = readString(payload.screenshot)
  if (screenshot.startsWith('data:image/')) return screenshot
  if (screenshot.startsWith('http://') || screenshot.startsWith('https://')) {
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

export function buildDemoTabsResponse(error?: unknown): BrowserTabsResponse {
  const tabs = getDemoTabs()
  const nowIso = new Date().toISOString()
  const reason = readErrorReason(error)

  return {
    ok: true,
    tabs,
    activeTabId: tabs[0]?.id ?? null,
    updatedAt: nowIso,
    demoMode: true,
    error: reason,
    gatewaySupportRequired: isGatewaySupportRequired(error),
  }
}

export function buildDemoScreenshotResponse(params?: {
  activeTabId?: string | null
  currentUrl?: string
  error?: unknown
}): BrowserScreenshotResponse {
  const tabs = getDemoTabs()
  const nowIso = new Date().toISOString()
  const requestedTabId = params?.activeTabId ?? null
  const selectedTab =
    tabs.find((tab) => tab.id === requestedTabId) ??
    tabs.find((tab) => tab.isActive) ??
    tabs[0]

  const currentUrl = params?.currentUrl || selectedTab.url || 'about:blank'
  const title = selectedTab.title || 'OpenClaw Browser Demo'
  const reason = readErrorReason(params?.error)

  return {
    ok: true,
    imageDataUrl: buildDemoScreenshotUrl(currentUrl, title, nowIso),
    currentUrl,
    activeTabId: selectedTab.id,
    capturedAt: nowIso,
    demoMode: true,
    error: reason,
    gatewaySupportRequired: isGatewaySupportRequired(params?.error),
  }
}

export async function getGatewayTabsResponse(): Promise<BrowserTabsResponse> {
  try {
    const payload =
      await callGatewayRpcWithFallbackMethods(BROWSER_TABS_METHODS)

    const payloadRecord = isRecord(payload) ? payload : null
    const rawTabs = Array.isArray(payload)
      ? payload
      : Array.isArray(payloadRecord?.tabs)
        ? payloadRecord.tabs
        : Array.isArray(payloadRecord?.items)
          ? payloadRecord.items
          : []

    if (rawTabs.length === 0) {
      return buildDemoTabsResponse('Gateway returned no browser tabs')
    }

    const tabs = rawTabs.map(normalizeTab)
    let activeTabId =
      (payloadRecord && readString(payloadRecord.activeTabId)) ||
      tabs.find((tab) => tab.isActive)?.id ||
      tabs[0]?.id ||
      null

    const withActive = tabs.map(function mapTab(tab) {
      const active = activeTabId ? tab.id === activeTabId : false
      return { ...tab, isActive: active }
    })

    if (!activeTabId && withActive[0]) {
      activeTabId = withActive[0].id
    }

    return {
      ok: true,
      tabs: withActive,
      activeTabId,
      updatedAt: new Date().toISOString(),
      demoMode: false,
    }
  } catch (error) {
    return buildDemoTabsResponse(error)
  }
}

export async function getGatewayScreenshotResponse(
  activeTabId?: string | null,
): Promise<BrowserScreenshotResponse> {
  try {
    const payload = await callGatewayRpcWithFallbackMethods(
      BROWSER_SCREENSHOT_METHODS,
      activeTabId ? { tabId: activeTabId } : undefined,
    )

    if (!isRecord(payload)) {
      return buildDemoScreenshotResponse({
        activeTabId,
        error: 'Gateway returned an invalid screenshot payload',
      })
    }

    const currentUrl =
      readString(payload.currentUrl) ||
      readString(payload.url) ||
      readString(payload.href) ||
      'about:blank'

    const resolvedTabId =
      readString(payload.activeTabId) ||
      readString(payload.tabId) ||
      readString(payload.id) ||
      activeTabId ||
      null

    const imageDataUrl = coerceImageDataUrl(payload)
    if (!imageDataUrl) {
      return buildDemoScreenshotResponse({
        activeTabId: resolvedTabId,
        currentUrl,
        error: 'Gateway returned a screenshot payload without image data',
      })
    }

    return {
      ok: true,
      imageDataUrl,
      currentUrl,
      activeTabId: resolvedTabId,
      capturedAt: new Date().toISOString(),
      demoMode: false,
    }
  } catch (error) {
    return buildDemoScreenshotResponse({ activeTabId, error })
  }
}
