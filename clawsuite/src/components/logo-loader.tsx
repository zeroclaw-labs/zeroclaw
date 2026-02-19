'use client'

import { cn } from '@/lib/utils'
import { OpenClawStudioIcon } from '@/components/icons/clawsuite'

export type LogoLoaderProps = {
  className?: string
}

function LogoLoader({ className }: LogoLoaderProps) {
  return (
    <span className="logo-loader-track" aria-hidden="true">
      <OpenClawStudioIcon
        className={cn('logo-loader-icon size-4', className)}
      />
    </span>
  )
}

export { LogoLoader }
