'use client'

import { useSyncExternalStore } from 'react'
import { OpenClawStudioIcon } from '@/components/icons/clawsuite'
import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

export type AgentAvatarPreference = 'lobster' | 'logo'
export type AgentAvatarSize = 'sm' | 'md' | 'lg'

export const AGENT_AVATAR_STORAGE_KEY = 'clawsuite-loader-preference'
const AGENT_AVATAR_EVENT = 'clawsuite-loader-preference-change'

type AgentAvatarProps = {
  size?: AgentAvatarSize
  className?: string
  iconClassName?: string
}

function getContainerSizeClassName(size: AgentAvatarSize): string {
  if (size === 'sm') return 'size-6'
  if (size === 'lg') return 'size-10'
  return 'size-8'
}

function getEmojiSizeClassName(size: AgentAvatarSize): string {
  if (size === 'sm') return 'text-base'
  if (size === 'lg') return 'text-2xl'
  return 'text-xl'
}

function getLogoSizeClassName(size: AgentAvatarSize): string {
  if (size === 'sm') return 'size-4'
  if (size === 'lg') return 'size-6'
  return 'size-5'
}

export function readAgentAvatarPreference(): AgentAvatarPreference {
  if (typeof window === 'undefined') return 'lobster'

  try {
    const stored = window.localStorage.getItem(AGENT_AVATAR_STORAGE_KEY)
    if (stored === 'lobster' || stored === 'logo') {
      return stored
    }
  } catch {
    // Ignore storage errors
  }

  return 'lobster'
}

export function writeAgentAvatarPreference(
  preference: AgentAvatarPreference,
): void {
  if (typeof window === 'undefined') return

  try {
    window.localStorage.setItem(AGENT_AVATAR_STORAGE_KEY, preference)
  } catch {
    // Ignore storage errors
  }

  window.dispatchEvent(new Event(AGENT_AVATAR_EVENT))
}

export function toggleAgentAvatarPreference(
  currentPreference: AgentAvatarPreference,
): AgentAvatarPreference {
  const nextPreference = currentPreference === 'lobster' ? 'logo' : 'lobster'
  writeAgentAvatarPreference(nextPreference)
  return nextPreference
}

export function subscribeToAgentAvatarPreference(
  onStoreChange: () => void,
): () => void {
  if (typeof window === 'undefined') {
    return function noop() {
      return undefined
    }
  }

  function handleStorage(event: StorageEvent) {
    if (event.key === AGENT_AVATAR_STORAGE_KEY) {
      onStoreChange()
    }
  }

  function handlePreferenceChange() {
    onStoreChange()
  }

  window.addEventListener('storage', handleStorage)
  window.addEventListener(AGENT_AVATAR_EVENT, handlePreferenceChange)

  return function unsubscribe() {
    window.removeEventListener('storage', handleStorage)
    window.removeEventListener(AGENT_AVATAR_EVENT, handlePreferenceChange)
  }
}

function AgentAvatar({
  size = 'md',
  className,
  iconClassName,
}: AgentAvatarProps) {
  const preference = useSyncExternalStore(
    subscribeToAgentAvatarPreference,
    readAgentAvatarPreference,
    function getServerSnapshot() {
      return 'lobster'
    },
  )

  return (
    <TooltipProvider>
      <TooltipRoot>
        <TooltipTrigger
          type="button"
          className={cn(
            'inline-flex cursor-pointer items-center justify-center rounded-full border border-primary-300/70 bg-primary-200/70 text-primary-900 transition-transform duration-150 hover:scale-105 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent-500/45',
            getContainerSizeClassName(size),
            className,
          )}
          aria-label="Toggle agent avatar"
          onClick={function handleToggleAvatar(event) {
            event.stopPropagation()
            toggleAgentAvatarPreference(preference as AgentAvatarPreference)
          }}
        >
          {preference === 'lobster' ? (
            <span
              className={cn('leading-none', getEmojiSizeClassName(size))}
              aria-hidden="true"
            >
              🦞
            </span>
          ) : (
            <OpenClawStudioIcon
              className={cn(getLogoSizeClassName(size), iconClassName)}
            />
          )}
        </TooltipTrigger>
        <TooltipContent side="top">Click to switch avatar</TooltipContent>
      </TooltipRoot>
    </TooltipProvider>
  )
}

export { AgentAvatar }
