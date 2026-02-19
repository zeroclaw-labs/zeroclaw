import { HugeiconsIcon } from '@hugeicons/react'
import { BrowserIcon, Loading03Icon } from '@hugeicons/core-free-icons'
import { AnimatePresence, motion } from 'motion/react'
import { cn } from '@/lib/utils'

type BrowserTab = {
  id: string
  title: string
  url: string
  isActive: boolean
}

type BrowserTabsProps = {
  tabs: Array<BrowserTab>
  activeTabId: string | null
  loading: boolean
  onSelect: (tabId: string) => void
}

function BrowserTabs({
  tabs,
  activeTabId,
  loading,
  onSelect,
}: BrowserTabsProps) {
  if (loading && tabs.length === 0) {
    return (
      <section className="rounded-2xl border border-primary-200 bg-primary-100/45 p-3 shadow-sm backdrop-blur-xl">
        <div className="mb-2 flex items-center justify-between">
          <h2 className="text-sm font-medium text-primary-900 text-balance">
            Open Tabs
          </h2>
          <HugeiconsIcon
            icon={Loading03Icon}
            size={20}
            strokeWidth={1.5}
            className="animate-spin text-primary-500"
          />
        </div>
        <div className="space-y-2">
          {Array.from({ length: 3 }).map(function mapPlaceholder(_, index) {
            return (
              <div
                key={index}
                className="h-14 animate-pulse rounded-xl border border-primary-200 bg-primary-200/50"
              />
            )
          })}
        </div>
      </section>
    )
  }

  return (
    <section className="rounded-2xl border border-primary-200 bg-primary-100/45 p-3 shadow-sm backdrop-blur-xl">
      <div className="mb-2 flex items-center justify-between">
        <h2 className="text-sm font-medium text-primary-900 text-balance">
          Open Tabs
        </h2>
        <span className="text-xs text-primary-500 tabular-nums">
          {tabs.length}
        </span>
      </div>

      {tabs.length === 0 ? (
        <div className="rounded-xl border border-primary-200 bg-primary-50/70 px-3 py-4 text-sm text-primary-500 text-pretty">
          No tabs are available in the current browser session.
        </div>
      ) : (
        <ul className="space-y-2">
          <AnimatePresence initial={false}>
            {tabs.map(function mapTab(tab) {
              const isActive = activeTabId
                ? tab.id === activeTabId
                : tab.isActive
              return (
                <motion.li
                  key={tab.id}
                  layout
                  initial={{ opacity: 0, y: 6 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, y: -6 }}
                  transition={{ duration: 0.16 }}
                >
                  <button
                    type="button"
                    onClick={function onClickTab() {
                      onSelect(tab.id)
                    }}
                    className={cn(
                      'w-full rounded-xl border px-3 py-2.5 text-left transition-colors',
                      'bg-primary-50/70 border-primary-200 hover:bg-primary-100',
                      isActive && 'border-accent-500/45 bg-accent-500/12',
                    )}
                  >
                    <div className="flex items-start gap-2.5">
                      <HugeiconsIcon
                        icon={BrowserIcon}
                        size={20}
                        strokeWidth={1.5}
                        className={cn(
                          'shrink-0 text-primary-500',
                          isActive && 'text-accent-500',
                        )}
                      />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <span className="truncate text-sm font-medium text-primary-900">
                            {tab.title || 'Untitled Tab'}
                          </span>
                          {isActive ? (
                            <span className="size-2 rounded-full bg-accent-500" />
                          ) : null}
                        </div>
                        <p className="truncate text-xs text-primary-500 tabular-nums">
                          {tab.url}
                        </p>
                      </div>
                    </div>
                  </button>
                </motion.li>
              )
            })}
          </AnimatePresence>
        </ul>
      )}
    </section>
  )
}

export { BrowserTabs }
