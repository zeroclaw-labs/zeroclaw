import { HugeiconsIcon } from '@hugeicons/react'
import {
  ArrowLeft01Icon,
  ArrowRight01Icon,
  RefreshIcon,
} from '@hugeicons/core-free-icons'
import { motion } from 'motion/react'
import { Button } from '@/components/ui/button'

type BrowserControlsProps = {
  url: string
  loading: boolean
  refreshing: boolean
  demoMode: boolean
  onRefresh: () => void
}

function BrowserControls({
  url,
  loading,
  refreshing,
  demoMode,
  onRefresh,
}: BrowserControlsProps) {
  const navigationHint = demoMode
    ? 'Browser navigation requires gateway browser RPC support.'
    : 'Browser navigation RPC is not available yet.'

  return (
    <motion.header
      initial={{ opacity: 0, y: -8 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.2 }}
      className="rounded-2xl border border-primary-200 bg-primary-100/40 p-3 shadow-sm backdrop-blur-xl"
    >
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          size="icon-sm"
          disabled
          aria-label="Navigate back"
          title={navigationHint}
        >
          <HugeiconsIcon icon={ArrowLeft01Icon} size={20} strokeWidth={1.5} />
        </Button>
        <Button
          variant="outline"
          size="icon-sm"
          disabled
          aria-label="Navigate forward"
          title={navigationHint}
        >
          <HugeiconsIcon icon={ArrowRight01Icon} size={20} strokeWidth={1.5} />
        </Button>
        <Button
          variant="secondary"
          size="sm"
          onClick={onRefresh}
          disabled={loading || refreshing}
          aria-label="Refresh browser screenshot and tabs"
          className="tabular-nums"
        >
          <HugeiconsIcon icon={RefreshIcon} size={20} strokeWidth={1.5} />
          {refreshing ? 'Refreshing' : 'Refresh'}
        </Button>
        <div className="min-w-[240px] flex-1 rounded-xl border border-primary-200 bg-primary-50/75 px-3 py-2 text-sm text-primary-700">
          <span className="block truncate tabular-nums">
            {url || 'about:blank'}
          </span>
        </div>
        {demoMode ? (
          <span className="inline-flex items-center rounded-full border border-accent-500/40 bg-accent-500/15 px-2.5 py-1 text-xs text-accent-500 tabular-nums">
            Demo Mode
          </span>
        ) : null}
      </div>
    </motion.header>
  )
}

export { BrowserControls }
