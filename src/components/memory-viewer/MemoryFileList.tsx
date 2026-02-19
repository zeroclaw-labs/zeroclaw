import { HugeiconsIcon } from '@hugeicons/react'
import {
  ArrowRight01Icon,
  BrainIcon,
  File01Icon,
  Folder01Icon,
  RefreshIcon,
} from '@hugeicons/core-free-icons'
import { AnimatePresence as _AnimatePresence, motion } from 'motion/react'
import type { MemoryFileGroup, MemoryViewerFile } from './memory-types'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import {
  ScrollAreaCorner,
  ScrollAreaRoot,
  ScrollAreaScrollbar,
  ScrollAreaThumb,
  ScrollAreaViewport,
} from '@/components/ui/scroll-area'

type MemoryFileListProps = {
  rootFile: MemoryViewerFile | null
  groups: Array<MemoryFileGroup>
  selectedPath: string | null
  loading: boolean
  error: string | null
  isDemo: boolean
  collapsed: boolean
  onToggleCollapse: () => void
  onRefresh: () => void
  onSelectPath: (path: string) => void
}

function formatSize(size: number): string {
  if (size < 1024) return `${size} B`
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`
  return `${(size / (1024 * 1024)).toFixed(1)} MB`
}

function formatModifiedAt(value: string): string {
  const date = new Date(value)
  return new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  }).format(date)
}

function MemoryFileList({
  rootFile,
  groups,
  selectedPath,
  loading,
  error,
  isDemo,
  collapsed,
  onToggleCollapse,
  onRefresh,
  onSelectPath,
}: MemoryFileListProps) {
  return (
    <motion.aside
      layout
      initial={false}
      animate={{ width: collapsed ? 0 : 320, opacity: collapsed ? 0 : 1 }}
      transition={{ duration: 0.18, ease: 'easeOut' }}
      className={cn(
        'border-primary-200 bg-primary-100/50 lg:border-r',
        collapsed ? 'pointer-events-none overflow-hidden' : 'overflow-hidden',
      )}
    >
      <div className="border-b border-primary-200 px-3 py-3">
        <div className="mb-2 flex items-center justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2">
            <HugeiconsIcon icon={BrainIcon} size={20} strokeWidth={1.5} />
            <h2 className="truncate text-sm font-medium text-balance">
              Memory Files
            </h2>
          </div>
          <div className="flex items-center gap-1">
            <Button
              size="icon-sm"
              variant="ghost"
              onClick={onRefresh}
              aria-label="Refresh memory files"
            >
              <HugeiconsIcon icon={RefreshIcon} size={20} strokeWidth={1.5} />
            </Button>
            <Button
              size="icon-sm"
              variant="ghost"
              onClick={onToggleCollapse}
              aria-label="Collapse memory file list"
              className="hidden lg:inline-flex"
            >
              <HugeiconsIcon
                icon={ArrowRight01Icon}
                size={20}
                strokeWidth={1.5}
                className="rotate-180"
              />
            </Button>
          </div>
        </div>
        <p className="text-xs text-primary-600 text-pretty">
          {isDemo
            ? 'Demo mode enabled because memory API data is unavailable.'
            : 'Browse MEMORY.md and daily notes in memory/.'}
        </p>
      </div>

      <ScrollAreaRoot className="h-[calc(100%-96px)]">
        <ScrollAreaViewport className="px-2 py-2">
          {loading ? (
            <div className="rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-xs text-primary-600 text-pretty">
              Loading memory files...
            </div>
          ) : null}
          {error ? (
            <div className="rounded-lg border border-red-300 bg-red-50/70 px-3 py-2 text-xs text-red-700 text-pretty">
              {error}
            </div>
          ) : null}
          {!loading && !error ? (
            <div className="space-y-2 pb-3">
              {rootFile ? (
                <MemoryRow
                  file={rootFile}
                  selected={selectedPath === rootFile.path}
                  onSelectPath={onSelectPath}
                  isRoot
                />
              ) : null}
              <div className="rounded-lg border border-primary-200 bg-primary-50/70 p-1">
                <div className="flex items-center gap-2 px-2 py-1.5 text-xs font-medium text-primary-700 tabular-nums">
                  <HugeiconsIcon
                    icon={Folder01Icon}
                    size={20}
                    strokeWidth={1.5}
                  />
                  <span className="truncate">memory/</span>
                </div>
                {groups.length === 0 ? (
                  <div className="px-2 py-1.5 text-xs text-primary-500 text-pretty">
                    No daily memory files found.
                  </div>
                ) : (
                  groups.map(function renderGroup(group) {
                    return (
                      <section key={group.id} className="mt-1">
                        <h3 className="px-2 py-1 text-[11px] font-medium text-primary-500 tabular-nums">
                          {group.label}
                        </h3>
                        <div className="space-y-1">
                          {group.files.map(function renderRow(file) {
                            return (
                              <MemoryRow
                                key={file.path}
                                file={file}
                                selected={selectedPath === file.path}
                                onSelectPath={onSelectPath}
                              />
                            )
                          })}
                        </div>
                      </section>
                    )
                  })
                )}
              </div>
            </div>
          ) : null}
        </ScrollAreaViewport>
        <ScrollAreaScrollbar orientation="vertical">
          <ScrollAreaThumb />
        </ScrollAreaScrollbar>
        <ScrollAreaCorner />
      </ScrollAreaRoot>
    </motion.aside>
  )
}

type MemoryRowProps = {
  file: MemoryViewerFile
  selected: boolean
  onSelectPath: (path: string) => void
  isRoot?: boolean
}

function MemoryRow({
  file,
  selected,
  onSelectPath,
  isRoot = false,
}: MemoryRowProps) {
  return (
    <button
      type="button"
      onClick={function onClickRow() {
        onSelectPath(file.path)
      }}
      className={cn(
        'w-full rounded-md border px-2 py-1.5 text-left transition-colors',
        selected
          ? 'border-accent-500/40 bg-accent-500/10'
          : 'border-primary-200 bg-primary-50 hover:bg-primary-100',
      )}
    >
      <div className="flex items-center gap-2">
        <HugeiconsIcon icon={File01Icon} size={20} strokeWidth={1.5} />
        <span className="truncate text-sm font-medium text-primary-900 tabular-nums">
          {isRoot ? 'MEMORY.md' : file.name}
        </span>
      </div>
      <div className="mt-1 grid grid-cols-2 gap-x-2 text-[11px] text-primary-600 tabular-nums">
        <span className="truncate">{formatSize(file.size)}</span>
        <span className="truncate text-right">
          {formatModifiedAt(file.modifiedAt)}
        </span>
      </div>
    </button>
  )
}

export { MemoryFileList }
