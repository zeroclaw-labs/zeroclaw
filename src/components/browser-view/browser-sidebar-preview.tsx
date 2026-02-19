import { GlobeIcon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useState } from 'react'
import { cn } from '@/lib/utils'

type BrowserStatusResponse = {
  active: boolean
  url: string
  screenshotUrl: string
  message: string
}

const PLACEHOLDER_TEXT = 'Browser available when agents browse'
const NO_SESSION_TEXT = 'No active browser session'

const FALLBACK_STATUS: BrowserStatusResponse = {
  active: false,
  url: '',
  screenshotUrl: '',
  message: PLACEHOLDER_TEXT,
}

function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : ''
}

async function fetchBrowserStatus(): Promise<BrowserStatusResponse> {
  try {
    // Check local stream server status (same server the browser UI uses)
    const res = await fetch('http://localhost:9223', {
      signal: AbortSignal.timeout(2000),
    })
    if (res.ok) {
      const data = (await res.json()) as Record<string, unknown>
      if (data.running) {
        // Get a fresh screenshot for the preview
        const ssRes = await fetch('http://localhost:9223', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ action: 'screenshot' }),
          signal: AbortSignal.timeout(3000),
        })
        const ssData = ssRes.ok
          ? ((await ssRes.json()) as Record<string, unknown>)
          : {}
        return {
          active: true,
          url: readString(data.url),
          screenshotUrl: readString(ssData.screenshot) || '',
          message: readString(data.title) || 'Browser active',
        }
      }
    }
    return FALLBACK_STATUS
  } catch {
    return FALLBACK_STATUS
  }
}

function BrowserSidebarPreview() {
  const [failedImageUrl, setFailedImageUrl] = useState('')

  const browserStatusQuery = useQuery({
    queryKey: ['browser', 'sidebar-preview', 'status'],
    queryFn: fetchBrowserStatus,
    refetchInterval: (query) => {
      // Poll faster when browser is active
      const data = query.state.data
      return data?.active ? 2000 : 10_000
    },
    refetchIntervalInBackground: false,
    retry: false,
  })

  const status = browserStatusQuery.data ?? FALLBACK_STATUS
  const hasLivePreview =
    status.active &&
    Boolean(status.screenshotUrl) &&
    failedImageUrl !== status.screenshotUrl

  const urlLabel = status.active && status.url ? status.url : NO_SESSION_TEXT
  const placeholderText = status.active
    ? 'Waiting for browser screenshot'
    : PLACEHOLDER_TEXT

  return (
    <Link to="/browser" className="block">
      <div className="flex h-[200px] flex-col overflow-hidden rounded-xl border border-primary-200 bg-primary-100/45 shadow-sm transition-colors hover:border-primary-300 hover:bg-primary-100/65">
        <div className="flex items-center gap-2 border-b border-primary-200/70 bg-primary-50/80 px-2.5 py-2">
          <HugeiconsIcon
            icon={GlobeIcon}
            size={20}
            strokeWidth={1.5}
            className={cn(
              'shrink-0',
              status.active ? 'text-primary-700' : 'text-primary-500',
            )}
          />
          <span
            className={cn(
              'min-w-0 flex-1 truncate text-[11px] tabular-nums',
              status.active ? 'text-primary-700' : 'text-primary-500',
            )}
          >
            {urlLabel}
          </span>
        </div>

        <div className="relative min-h-0 flex-1 bg-primary-100/30">
          {hasLivePreview ? (
            <img
              src={status.screenshotUrl}
              alt="Browser preview screenshot"
              className="h-full w-full object-cover"
              onError={function onImageError() {
                setFailedImageUrl(status.screenshotUrl)
              }}
            />
          ) : (
            <div className="flex h-full flex-col items-center justify-center gap-2 px-4 text-center">
              <div className="flex size-10 items-center justify-center rounded-full border border-primary-200 bg-primary-50/80 text-primary-500">
                <HugeiconsIcon icon={GlobeIcon} size={20} strokeWidth={1.5} />
              </div>
              <p className="text-[11px] text-primary-600 text-pretty">
                {placeholderText}
              </p>
            </div>
          )}
        </div>
      </div>
    </Link>
  )
}

export { BrowserSidebarPreview }
