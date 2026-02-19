'use client'

import { BrailleSpinner } from './ui/braille-spinner'
import { ThreeDotsSpinner } from './ui/three-dots-spinner'
import { LogoLoader } from './logo-loader'
import type { BrailleSpinnerPreset } from './ui/braille-spinner'
import type { LoaderStyle } from '@/hooks/use-chat-settings'
import { useChatSettingsStore } from '@/hooks/use-chat-settings'
import { cn } from '@/lib/utils'

export type LoaderPreference = LoaderStyle

export type LoadingIndicatorProps = {
  className?: string
  iconClassName?: string
  ariaLabel?: string
}

function getBraillePreset(
  loaderStyle: LoaderStyle,
): BrailleSpinnerPreset | null {
  if (loaderStyle === 'braille-claw') return 'claw'
  if (loaderStyle === 'braille-orbit') return 'orbit'
  if (loaderStyle === 'braille-breathe') return 'breathe'
  if (loaderStyle === 'braille-pulse') return 'pulse'
  if (loaderStyle === 'braille-wave') return 'wave'
  return null
}

function renderLoader(loaderStyle: LoaderStyle, iconClassName?: string) {
  if (loaderStyle === 'dots') {
    return <ThreeDotsSpinner className={iconClassName} />
  }

  if (loaderStyle === 'lobster') {
    return (
      <span
        className={cn(
          'inline-block leading-none text-sm animate-pulse',
          iconClassName,
        )}
        aria-hidden="true"
      >
        🦞
      </span>
    )
  }

  if (loaderStyle === 'logo') {
    return <LogoLoader className={iconClassName} />
  }

  const preset = getBraillePreset(loaderStyle)
  if (preset) {
    return (
      <span aria-hidden="true">
        <BrailleSpinner
          preset={preset}
          size={18}
          speed={120}
          className={cn('text-primary-500', iconClassName)}
        />
      </span>
    )
  }

  return <ThreeDotsSpinner className={iconClassName} />
}

function LoadingIndicator({
  className,
  iconClassName,
  ariaLabel = 'Assistant is streaming',
}: LoadingIndicatorProps) {
  const loaderStyle = useChatSettingsStore(
    (state) => state.settings.loaderStyle,
  )

  return (
    <span
      role="status"
      aria-live="polite"
      className={cn(
        'chat-streaming-loader inline-flex items-center justify-center bg-transparent p-0 text-current',
        className,
      )}
      aria-label={ariaLabel}
    >
      {renderLoader(loaderStyle, iconClassName)}
    </span>
  )
}

export { LoadingIndicator }
