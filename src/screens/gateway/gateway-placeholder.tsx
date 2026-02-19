import { HugeiconsIcon } from '@hugeicons/react'
import { AlertDiamondIcon } from '@hugeicons/core-free-icons'

export function GatewayPlaceholder({ title }: { title: string }) {
  return (
    <div className="flex flex-col items-center justify-center h-full gap-3 text-primary-500">
      <HugeiconsIcon icon={AlertDiamondIcon} size={32} strokeWidth={1.5} />
      <h2 className="text-lg font-medium text-ink">{title}</h2>
      <p className="text-sm text-primary-600 max-w-md text-center">
        This page is coming soon. The gateway endpoint exists but the UI has not
        been built yet.
      </p>
    </div>
  )
}
