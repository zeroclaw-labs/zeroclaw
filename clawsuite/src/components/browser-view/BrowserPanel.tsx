import { HugeiconsIcon } from '@hugeicons/react'
import { GlobeIcon, Loading03Icon } from '@hugeicons/core-free-icons'
import { useQuery } from '@tanstack/react-query'
import { motion } from 'motion/react'
import { useMemo, useState } from 'react'
import { BrowserControls } from './BrowserControls'
import { BrowserScreenshot } from './BrowserScreenshot'
import { BrowserTabs } from './BrowserTabs'
import { LocalBrowser } from './LocalBrowser'

type BrowserTab = {
  id: string
  title: string
  url: string
  isActive: boolean
}

type BrowserTabsResponse = {
  ok: boolean
  tabs: Array<BrowserTab>
  activeTabId: string | null
  updatedAt: string
  demoMode: boolean
  error?: string
  gatewaySupportRequired?: boolean
}

type BrowserScreenshotResponse = {
  ok: boolean
  imageDataUrl: string
  currentUrl: string
  activeTabId: string | null
  capturedAt: string
  demoMode: boolean
  error?: string
  gatewaySupportRequired?: boolean
}

const GATEWAY_SUPPORT_PATTERNS = [
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

function readError(response: Response): Promise<string> {
  return response
    .json()
    .then(function onJson(payload) {
      if (payload && typeof payload.error === 'string') return payload.error
      return response.statusText || 'Request failed'
    })
    .catch(function onError() {
      return response.statusText || 'Request failed'
    })
}

function readErrorMessage(error: unknown, fallbackMessage: string): string {
  if (error instanceof Error) return error.message || fallbackMessage
  if (typeof error === 'string' && error.trim()) return error
  return fallbackMessage
}

function isGatewaySupportError(message: string): boolean {
  const normalizedMessage = message.trim().toLowerCase()
  if (!normalizedMessage) return false

  return GATEWAY_SUPPORT_PATTERNS.some(function hasPattern(pattern) {
    return normalizedMessage.includes(pattern)
  })
}

function createLocalFallbackTabs(error: unknown): BrowserTabsResponse {
  const reason = readErrorMessage(error, 'Browser tabs unavailable')

  return {
    ok: true,
    tabs: [
      {
        id: 'local-demo-tab',
        title: 'ClawSuite Demo',
        url: 'https://openclaw.local/studio',
        isActive: true,
      },
    ],
    activeTabId: 'local-demo-tab',
    updatedAt: new Date().toISOString(),
    demoMode: true,
    error: reason,
    gatewaySupportRequired: true,
  }
}

function createLocalFallbackScreenshot(
  tabId?: string | null,
  error?: unknown,
): BrowserScreenshotResponse {
  const timestamp = new Date().toISOString()
  const reason = readErrorMessage(error, 'Browser screenshot unavailable')
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="720" viewBox="0 0 1200 720">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="#0f172a"/>
      <stop offset="100%" stop-color="#1e293b"/>
    </linearGradient>
  </defs>
  <rect width="1200" height="720" fill="url(#g)"/>
  <rect x="80" y="80" width="1040" height="560" rx="22" fill="#0b1220" stroke="#334155"/>
  <text x="120" y="180" fill="#f8fafc" font-family="ui-sans-serif, system-ui, sans-serif" font-size="38">Demo Browser Mode</text>
  <text x="120" y="226" fill="#94a3b8" font-family="ui-sans-serif, system-ui, sans-serif" font-size="21">Using fallback screenshot stream.</text>
  <text x="120" y="268" fill="#94a3b8" font-family="ui-monospace, SFMono-Regular, Menlo, monospace" font-size="17">tabId=${tabId || 'active'}</text>
</svg>`

  return {
    ok: true,
    imageDataUrl: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`,
    currentUrl: 'https://openclaw.local/demo/browser',
    activeTabId: tabId || 'local-demo-tab',
    capturedAt: timestamp,
    demoMode: true,
    error: reason,
    gatewaySupportRequired: true,
  }
}

async function fetchBrowserTabs(): Promise<BrowserTabsResponse> {
  try {
    const response = await fetch('/api/browser/tabs')
    if (!response.ok) {
      throw new Error(await readError(response))
    }

    return (await response.json()) as BrowserTabsResponse
  } catch (error) {
    return createLocalFallbackTabs(error)
  }
}

async function fetchBrowserScreenshot(
  activeTabId?: string | null,
): Promise<BrowserScreenshotResponse> {
  try {
    const params = new URLSearchParams()
    if (activeTabId) params.set('tabId', activeTabId)

    const response = await fetch(
      `/api/browser/screenshot${params.size ? `?${params.toString()}` : ''}`,
    )
    if (!response.ok) {
      throw new Error(await readError(response))
    }

    return (await response.json()) as BrowserScreenshotResponse
  } catch (error) {
    return createLocalFallbackScreenshot(activeTabId, error)
  }
}

function BrowserPanel() {
  const [selectedTabId, setSelectedTabId] = useState<string | null>(null)

  const tabsQuery = useQuery({
    queryKey: ['browser', 'tabs'],
    queryFn: fetchBrowserTabs,
    refetchInterval: 2_000,
    refetchIntervalInBackground: true,
    retry: false,
  })

  const tabs = tabsQuery.data?.tabs ?? []
  const tabSet = useMemo(
    function buildTabSet() {
      return new Set(tabs.map((tab) => tab.id))
    },
    [tabs],
  )

  const effectiveTabId =
    selectedTabId && tabSet.has(selectedTabId)
      ? selectedTabId
      : (tabsQuery.data?.activeTabId ??
        tabs.find((tab) => tab.isActive)?.id ??
        null)

  const screenshotQuery = useQuery({
    queryKey: ['browser', 'screenshot', effectiveTabId ?? 'active'],
    queryFn: function queryScreenshot() {
      return fetchBrowserScreenshot(effectiveTabId)
    },
    refetchInterval: 2_000,
    refetchIntervalInBackground: true,
    retry: false,
  })

  const activeTab = tabs.find((tab) => tab.id === effectiveTabId)
  const currentUrl =
    screenshotQuery.data?.currentUrl || activeTab?.url || 'about:blank'
  const demoMode =
    Boolean(tabsQuery.data?.demoMode) || Boolean(screenshotQuery.data?.demoMode)
  const screenshotUrl = screenshotQuery.data?.imageDataUrl || ''
  const errorText = tabsQuery.data?.error || screenshotQuery.data?.error || ''
  const gatewaySupportRequired =
    Boolean(tabsQuery.data?.gatewaySupportRequired) ||
    Boolean(screenshotQuery.data?.gatewaySupportRequired) ||
    isGatewaySupportError(errorText)
  const showGatewaySupportPlaceholder = demoMode && gatewaySupportRequired

  // Default to local browser — gateway RPC browser is an advanced/optional mode
  // Show local browser immediately, no waiting for gateway probe
  const gatewayBrowserAvailable =
    tabsQuery.isSuccess &&
    !showGatewaySupportPlaceholder &&
    (tabsQuery.data?.tabs?.length ?? 0) > 0

  if (!gatewayBrowserAvailable) {
    return (
      <motion.main
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        transition={{ duration: 0.22 }}
        className="h-screen bg-surface text-primary-900"
      >
        <LocalBrowser />
      </motion.main>
    )
  }

  function handleSelectTab(tabId: string) {
    setSelectedTabId(tabId)
  }

  function handleRefresh() {
    void Promise.all([tabsQuery.refetch(), screenshotQuery.refetch()])
  }

  return (
    <motion.main
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: 0.22 }}
      className="h-screen bg-surface px-3 py-3 text-primary-900 sm:px-4 sm:py-4"
    >
      <div className="mx-auto flex h-full w-full max-w-[1700px] min-w-0 flex-col gap-3">
        <header className="rounded-2xl border border-primary-200 bg-primary-50/85 p-4 shadow-sm backdrop-blur-xl">
          <div className="inline-flex items-center gap-2 rounded-full border border-primary-200 bg-primary-100/70 px-3 py-1 text-xs text-primary-600 tabular-nums">
            <HugeiconsIcon icon={GlobeIcon} size={20} strokeWidth={1.5} />
            <span>Live Browser Monitor</span>
          </div>
          <h1 className="mt-2 text-xl font-medium text-balance sm:text-2xl">
            Browser View
          </h1>
          <p className="mt-1 text-sm text-primary-600 text-pretty">
            Track agent browser tabs and live screenshots every 2 seconds.
          </p>
        </header>

        <BrowserControls
          url={currentUrl}
          loading={tabsQuery.isPending || screenshotQuery.isPending}
          refreshing={tabsQuery.isRefetching || screenshotQuery.isRefetching}
          demoMode={demoMode}
          onRefresh={handleRefresh}
        />

        {demoMode ? (
          <div className="flex items-center gap-3 rounded-xl border border-amber-500/30 bg-amber-500/5 px-4 py-3">
            <div className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-amber-500/15">
              <HugeiconsIcon
                icon={GlobeIcon}
                size={20}
                strokeWidth={1.5}
                className="text-amber-600"
              />
            </div>
            <div className="flex-1 min-w-0">
              <div className="flex items-center gap-2">
                <span className="inline-flex items-center rounded-full bg-amber-500/20 px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-amber-700">
                  Demo Mode
                </span>
              </div>
              <p className="mt-0.5 text-xs text-amber-700 text-pretty">
                {showGatewaySupportPlaceholder
                  ? 'Browser control requires gateway support. Enable browser RPC in your Gateway configuration.'
                  : 'Connect a browser to use live features. Configure the browser plugin in your Gateway settings.'}
                {errorText ? (
                  <span className="text-amber-600/80"> ({errorText})</span>
                ) : null}
              </p>
            </div>
          </div>
        ) : null}

        <section className="grid min-h-0 flex-1 grid-cols-1 gap-3 lg:grid-cols-[320px_1fr]">
          <BrowserTabs
            tabs={tabs}
            activeTabId={effectiveTabId}
            loading={tabsQuery.isPending}
            onSelect={handleSelectTab}
          />

          {showGatewaySupportPlaceholder ? (
            <div className="flex min-h-[320px] flex-col items-center justify-center gap-3 rounded-2xl border border-primary-200 bg-primary-100/35 p-6 text-center lg:min-h-[560px]">
              <div className="flex size-10 items-center justify-center rounded-full border border-primary-300 bg-primary-50 text-primary-700">
                <HugeiconsIcon icon={GlobeIcon} size={20} strokeWidth={1.5} />
              </div>
              <h3 className="text-base font-medium text-primary-900 text-balance">
                Browser Control Setup
              </h3>
              <p className="max-w-md text-sm text-primary-600 text-pretty">
                Connect a browser so your AI agent can browse the web, fill
                forms, and extract data.
              </p>
              <div className="mt-2 w-full max-w-md rounded-xl border border-primary-200 bg-surface p-4 text-left space-y-3">
                <div className="flex items-start gap-3">
                  <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-accent-500/15 text-xs font-bold text-accent-600">
                    1
                  </span>
                  <div>
                    <p className="text-sm font-medium text-ink">
                      Install the Chrome Extension
                    </p>
                    <p className="text-xs text-primary-500 mt-0.5">
                      Install the{' '}
                      <a
                        href="https://docs.openclaw.ai/browser"
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-accent-500 hover:underline"
                      >
                        OpenClaw Browser Relay
                      </a>{' '}
                      extension from the Chrome Web Store.
                    </p>
                  </div>
                </div>
                <div className="flex items-start gap-3">
                  <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-accent-500/15 text-xs font-bold text-accent-600">
                    2
                  </span>
                  <div>
                    <p className="text-sm font-medium text-ink">
                      Enable Browser RPC
                    </p>
                    <p className="text-xs text-primary-500 mt-0.5">
                      Add{' '}
                      <code className="rounded bg-primary-100 px-1 py-0.5 font-mono text-[11px]">
                        browser: true
                      </code>{' '}
                      to your OpenClaw gateway config.
                    </p>
                  </div>
                </div>
                <div className="flex items-start gap-3">
                  <span className="flex size-6 shrink-0 items-center justify-center rounded-full bg-accent-500/15 text-xs font-bold text-accent-600">
                    3
                  </span>
                  <div>
                    <p className="text-sm font-medium text-ink">Attach a Tab</p>
                    <p className="text-xs text-primary-500 mt-0.5">
                      Click the OpenClaw toolbar icon on any Chrome tab to
                      connect it. The badge turns ON when attached.
                    </p>
                  </div>
                </div>
              </div>
              <a
                href="https://docs.openclaw.ai/browser"
                target="_blank"
                rel="noopener noreferrer"
                className="mt-2 inline-flex items-center gap-1 rounded-lg bg-accent-500/10 px-4 py-2 text-sm font-medium text-accent-600 hover:bg-accent-500/20 transition-colors"
              >
                View Full Setup Guide →
              </a>
            </div>
          ) : screenshotUrl ? (
            <BrowserScreenshot
              imageDataUrl={screenshotUrl}
              loading={screenshotQuery.isPending}
              capturedAt={screenshotQuery.data?.capturedAt || ''}
            />
          ) : screenshotQuery.isPending ? (
            <div className="flex min-h-[320px] items-center justify-center rounded-2xl border border-primary-200 bg-primary-100/35 text-primary-500">
              <HugeiconsIcon
                icon={Loading03Icon}
                size={20}
                strokeWidth={1.5}
                className="animate-spin"
              />
            </div>
          ) : (
            <div className="flex min-h-[320px] flex-col items-center justify-center gap-2 rounded-2xl border border-primary-200 bg-primary-100/35 px-6 text-center lg:min-h-[560px]">
              <h3 className="text-base font-medium text-primary-900 text-balance">
                Screenshot unavailable
              </h3>
              <p className="max-w-md text-sm text-primary-600 text-pretty">
                {errorText ||
                  'No screenshot was returned by the gateway. Use refresh to retry.'}
              </p>
            </div>
          )}
        </section>
      </div>
    </motion.main>
  )
}

export { BrowserPanel }
