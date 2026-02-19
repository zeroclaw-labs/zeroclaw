import { HugeiconsIcon } from '@hugeicons/react'
import {
  AiBrain01Icon,
  CloudIcon,
  ComputerIcon,
  FlashIcon,
  GlobeIcon,
  LanguageSkillIcon,
  SourceCodeSquareIcon,
} from '@hugeicons/core-free-icons'
import type * as React from 'react'
import { normalizeProviderId } from '@/lib/provider-catalog'
import { cn } from '@/lib/utils'

type ProviderIconProps = {
  providerId: string
  className?: string
}

type ProviderIconName = React.ComponentProps<typeof HugeiconsIcon>['icon']

function getIcon(providerId: string): ProviderIconName {
  const normalized = normalizeProviderId(providerId)

  if (normalized === 'anthropic') return AiBrain01Icon
  if (normalized === 'openai') return SourceCodeSquareIcon
  if (normalized === 'google') return LanguageSkillIcon
  if (normalized === 'openrouter') return GlobeIcon
  if (normalized === 'minimax') return FlashIcon
  if (normalized === 'ollama') return ComputerIcon
  return CloudIcon
}

export function ProviderIcon({ providerId, className }: ProviderIconProps) {
  return (
    <HugeiconsIcon
      icon={getIcon(providerId)}
      size={20}
      strokeWidth={1.5}
      className={cn('shrink-0', className)}
    />
  )
}
