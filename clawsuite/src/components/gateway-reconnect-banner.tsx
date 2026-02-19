'use client'

import { useEffect, useRef, useState } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import { Alert02Icon, Cancel01Icon, Settings02Icon } from '@hugeicons/core-free-icons'
import { Button } from '@/components/ui/button'
import { useGatewaySetupStore } from '@/hooks/use-gateway-setup'
import { cn } from '@/lib/utils'

const BANNER_STORAGE_KEY = 'clawsuite-gateway-banner-dismissed'
const CHECK_INTERVAL_MS = 5_000 // Check every 5 seconds
const DISMISS_ANIMATION_MS = 220

async function checkGatewayHealth(): Promise<boolean> {
  try {
    const response = await fetch('/api/ping', {
      signal: AbortSignal.timeout(5000),
    })
    if (!response.ok) return false
    const data = (await response.json()) as { ok?: boolean }
    return Boolean(data.ok)
  } catch {
    return false
  }
}

/**
 * Shows a persistent banner when the gateway is unreachable (but was previously configured).
 * This is different from the setup wizard — it's for temporary connection issues.
 */
export function GatewayReconnectBanner() {
  const [isVisible, setIsVisible] = useState(false)
  const [isDismissed, setIsDismissed] = useState(false)
  const [isFadingOut, setIsFadingOut] = useState(false)
  const { open: openSetupWizard } = useGatewaySetupStore()
  const isVisibleRef = useRef(isVisible)

  useEffect(() => {
    isVisibleRef.current = isVisible
  }, [isVisible])

  useEffect(() => {
    // Only run on client
    if (typeof window === 'undefined') return

    // Check if user previously dismissed the banner in this session
    const dismissed = sessionStorage.getItem(BANNER_STORAGE_KEY) === 'true'
    if (dismissed) {
      setIsDismissed(true)
      return
    }

    let mounted = true

    let failCount = 0
    let wasUnhealthy = false
    let fadeTimer: number | null = null

    function clearFadeTimer() {
      if (fadeTimer !== null) {
        window.clearTimeout(fadeTimer)
        fadeTimer = null
      }
    }

    function hideBannerWithFade() {
      if (!isVisibleRef.current) {
        setIsVisible(false)
        setIsFadingOut(false)
        return
      }

      clearFadeTimer()
      setIsFadingOut(true)
      fadeTimer = window.setTimeout(() => {
        if (!mounted) return
        setIsVisible(false)
        setIsFadingOut(false)
      }, DISMISS_ANIMATION_MS)
    }

    async function checkHealth() {
      if (!mounted) return
      const healthy = await checkGatewayHealth()
      if (!mounted) return

      if (healthy) {
        const shouldNotifyRestored = wasUnhealthy
        failCount = 0
        wasUnhealthy = false
        hideBannerWithFade()
        if (shouldNotifyRestored && typeof window !== 'undefined') {
          window.dispatchEvent(new CustomEvent('gateway:health-restored'))
        }
      } else {
        failCount++
        // Only show banner after 2 consecutive failures (avoids flash on slow initial connect)
        if (failCount >= 2 && !isDismissed) {
          wasUnhealthy = true
          clearFadeTimer()
          setIsFadingOut(false)
          setIsVisible(true)
        }
      }
    }

    // Initial check (delayed to let SSR gateway client connect)
    const initialTimer = setTimeout(() => void checkHealth(), 3000)

    // Periodic checks
    const interval = setInterval(() => {
      void checkHealth()
    }, CHECK_INTERVAL_MS)

    return () => {
      mounted = false
      clearTimeout(initialTimer)
      clearInterval(interval)
      clearFadeTimer()
    }
  }, [isDismissed])

  const handleDismiss = () => {
    sessionStorage.setItem(BANNER_STORAGE_KEY, 'true')
    setIsDismissed(true)
    setIsVisible(false)
  }

  const handleOpenSettings = () => {
    openSetupWizard()
    handleDismiss()
  }

  if ((!isVisible && !isFadingOut) || isDismissed) return null

  return (
    <div
      className={cn(
        'fixed bottom-4 left-4 right-4 sm:left-auto sm:w-80 z-40 rounded-xl border border-red-200 bg-red-50 px-3 py-2.5 shadow-lg',
        'transition-all duration-200 ease-out',
        (isVisible && !isFadingOut
          ? 'translate-y-0 opacity-100'
          : 'translate-y-2 opacity-0 pointer-events-none'),
      )}
      role="alert"
      aria-live="polite"
    >
      <div className="flex items-start gap-2">
        <HugeiconsIcon
          icon={Alert02Icon}
          size={20}
          className="shrink-0 text-red-600"
          strokeWidth={1.5}
        />
        <p className="text-xs font-medium text-red-900">
          Gateway connection lost.{' '}
          <span className="font-normal text-red-700 text-pretty">
            Check your connection or reconfigure in settings.
          </span>
        </p>
      </div>
      <div className="mt-2 flex items-center gap-2">
        <Button
          variant="secondary"
          size="sm"
          onClick={handleOpenSettings}
          className="h-7 shrink-0 border-red-300 bg-red-100 px-2 text-xs text-red-700 hover:bg-red-200"
        >
          <HugeiconsIcon icon={Settings02Icon} size={20} strokeWidth={1.5} />
          Reconfigure
        </Button>
        <button
          onClick={handleDismiss}
          className="shrink-0 rounded p-1 text-red-600 transition-colors hover:bg-red-200/70"
          aria-label="Dismiss banner"
        >
          <HugeiconsIcon icon={Cancel01Icon} size={20} strokeWidth={1.5} />
        </button>
      </div>
    </div>
  )
}
