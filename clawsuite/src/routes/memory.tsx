import { useCallback, useEffect, useMemo, useState } from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { useQuery } from '@tanstack/react-query'
import { HugeiconsIcon } from '@hugeicons/react'
import {
  BrainIcon,
  SidebarLeft01Icon,
  ViewIcon,
} from '@hugeicons/core-free-icons'
import { AnimatePresence, motion } from 'motion/react'
import type {
  MemoryFileGroup,
  MemorySearchResult,
  MemoryViewerFile,
} from '@/components/memory-viewer'
import { usePageTitle } from '@/hooks/use-page-title'
import {
  MemoryEditor,
  MemoryFileList,
  MemoryPreview,
  MemorySearch,
} from '@/components/memory-viewer'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { resolveTheme, useSettings } from '@/hooks/use-settings'

type ApiFileEntry = {
  name: string
  path: string
  type: 'file' | 'folder'
  size?: number
  modifiedAt?: string
}

type ApiFilesResponse = {
  entries?: Array<ApiFileEntry>
}

type SaveState = 'saved' | 'saving' | 'unsaved' | 'error'

type MemoryIndexData = {
  files: Array<MemoryViewerFile>
}

function isMarkdownFile(pathValue: string): boolean {
  return pathValue.toLowerCase().endsWith('.md')
}

function isDailyMemoryFile(fileName: string): boolean {
  return /^\d{4}-\d{2}-\d{2}\.md$/.test(fileName)
}

function toDateGroup(fileName: string): string | null {
  if (!isDailyMemoryFile(fileName)) return null
  return fileName.slice(0, 7)
}

function compareMemoryFiles(a: MemoryViewerFile, b: MemoryViewerFile): number {
  if (a.isRootMemory !== b.isRootMemory) return a.isRootMemory ? -1 : 1
  if (a.isDaily && b.isDaily) return b.name.localeCompare(a.name)
  return a.name.localeCompare(b.name)
}

function labelForDateGroup(value: string): string {
  const [year, month] = value.split('-')
  const date = new Date(Number(year), Number(month) - 1, 1)
  return new Intl.DateTimeFormat(undefined, {
    month: 'long',
    year: 'numeric',
  }).format(date)
}

function buildMemoryGroups(
  files: Array<MemoryViewerFile>,
): Array<MemoryFileGroup> {
  const groups = new Map<string, Array<MemoryViewerFile>>()
  const otherFiles: Array<MemoryViewerFile> = []

  for (const file of files) {
    if (file.isRootMemory) continue
    if (file.dateGroup) {
      const groupFiles = groups.get(file.dateGroup) || []
      groupFiles.push(file)
      groups.set(file.dateGroup, groupFiles)
      continue
    }
    otherFiles.push(file)
  }

  const ordered: Array<MemoryFileGroup> = []
  const dateGroups = Array.from(groups.keys()).sort(function sortDesc(a, b) {
    return b.localeCompare(a)
  })

  for (const key of dateGroups) {
    const filesForGroup = groups.get(key) || []
    filesForGroup.sort(compareMemoryFiles)
    ordered.push({
      id: key,
      label: labelForDateGroup(key),
      files: filesForGroup,
    })
  }

  if (otherFiles.length > 0) {
    otherFiles.sort(compareMemoryFiles)
    ordered.push({
      id: 'other',
      label: 'Other Notes',
      files: otherFiles,
    })
  }

  return ordered
}

async function fetchJson(url: string): Promise<ApiFilesResponse> {
  const response = await fetch(url)
  if (!response.ok) {
    const text = await response.text()
    throw new Error(text || `Request failed: ${response.status}`)
  }
  return (await response.json()) as ApiFilesResponse
}

function mapApiFiles(entries: Array<ApiFileEntry>): Array<MemoryViewerFile> {
  const mapped: Array<MemoryViewerFile> = []
  for (const entry of entries) {
    if (entry.type !== 'file') continue
    if (!isMarkdownFile(entry.path)) continue
    const name = entry.name
    mapped.push({
      name,
      path: entry.path,
      size: entry.size || 0,
      modifiedAt: entry.modifiedAt || new Date().toISOString(),
      source: 'api',
      isRootMemory: entry.path === 'MEMORY.md',
      isDaily: entry.path.startsWith('memory/') && isDailyMemoryFile(name),
      dateGroup: entry.path.startsWith('memory/') ? toDateGroup(name) : null,
    })
  }
  mapped.sort(compareMemoryFiles)
  return mapped
}

async function loadMemoryIndex(): Promise<MemoryIndexData> {
  const memoryResponse = await fetchJson('/api/files?path=memory/*.md')
  const rootResponse = await fetchJson('/api/files?path=MEMORY.*')
  const entries = [
    ...(memoryResponse.entries || []),
    ...(rootResponse.entries || []),
  ]
  const files = mapApiFiles(entries).filter(function filterMemory(file) {
    return file.path === 'MEMORY.md' || file.path.startsWith('memory/')
  })
  if (files.length === 0) {
    throw new Error('No memory files found')
  }
  return { files }
}

type MemoryContentMap = Record<string, string>

async function readFileContent(pathValue: string): Promise<string> {
  const response = await fetch(
    `/api/files?action=read&path=${encodeURIComponent(pathValue)}`,
  )
  if (!response.ok) {
    throw new Error(`Failed to read ${pathValue}`)
  }
  const body = (await response.json()) as { content?: string; type?: string }
  if (body.type !== 'text') return ''
  return body.content || ''
}

function buildSearchResults(
  query: string,
  contents: MemoryContentMap,
): Array<MemorySearchResult> {
  const trimmed = query.trim().toLowerCase()
  if (!trimmed) return []
  const results: Array<MemorySearchResult> = []

  for (const [pathValue, text] of Object.entries(contents)) {
    const lines = text.split('\n')
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index]
      if (!line.toLowerCase().includes(trimmed)) continue
      results.push({
        path: pathValue,
        line: index + 1,
        snippet: line.trim() || '(empty line)',
      })
      if (results.length >= 100) return results
    }
  }
  return results
}

export const Route = createFileRoute('/memory')({
  component: MemoryRoute,
  errorComponent: function MemoryError({ error }) {
    return (
      <div className="flex flex-col items-center justify-center h-full p-6 text-center bg-primary-50">
        <h2 className="text-xl font-semibold text-primary-900 mb-3">
          Failed to Load Memory
        </h2>
        <p className="text-sm text-primary-600 mb-4 max-w-md">
          {error instanceof Error
            ? error.message
            : 'An unexpected error occurred'}
        </p>
        <button
          onClick={() => window.location.reload()}
          className="px-4 py-2 bg-accent-500 text-white rounded-lg hover:bg-accent-600 transition-colors"
        >
          Reload Page
        </button>
      </div>
    )
  },
  pendingComponent: function MemoryPending() {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="text-center">
          <div className="inline-block h-8 w-8 animate-spin rounded-full border-4 border-accent-500 border-r-transparent mb-3" />
          <p className="text-sm text-primary-500">Loading memory files...</p>
        </div>
      </div>
    )
  },
})

function MemoryRoute() {
  usePageTitle('Memory')
  const { settings } = useSettings()
  const resolvedTheme = resolveTheme(settings.theme)

  const [collapsedList, setCollapsedList] = useState(false)
  const [previewVisible, setPreviewVisible] = useState(true)
  const [searchQuery, setSearchQuery] = useState('')
  const [selectedPath, setSelectedPath] = useState<string | null>(null)
  const [readOnly, setReadOnly] = useState(false)
  const [drafts, setDrafts] = useState<MemoryContentMap>({})
  const [savedOverrides, setSavedOverrides] = useState<MemoryContentMap>({})
  const [saveState, setSaveState] = useState<SaveState>('saved')
  const [lastSavedAt, setLastSavedAt] = useState<string | null>(null)

  const memoryIndexQuery = useQuery({
    queryKey: ['memory-index'],
    queryFn: loadMemoryIndex,
  })

  const files = memoryIndexQuery.data?.files || []
  const rootFile =
    files.find(function findRoot(file) {
      return file.path === 'MEMORY.md'
    }) || null
  const groups = useMemo(
    function memoGroups() {
      return buildMemoryGroups(files)
    },
    [files],
  )

  const contentQueryKey = useMemo(
    function memoKey() {
      const joinedPaths = files
        .map(function mapPath(file) {
          return file.path
        })
        .join('|')
      return ['memory-contents', joinedPaths]
    },
    [files],
  )

  const memoryContentsQuery = useQuery({
    enabled: files.length > 0,
    queryKey: contentQueryKey,
    queryFn: async function queryMemoryContents() {
      const entries = await Promise.all(
        files.map(async function mapFile(file) {
          const content = await readFileContent(file.path)
          return [file.path, content] as const
        }),
      )
      return Object.fromEntries(entries) as MemoryContentMap
    },
  })

  const fallbackPath = rootFile?.path || files[0]?.path || null

  const activePath = useMemo(
    function memoActivePath() {
      if (!fallbackPath) return null
      if (
        selectedPath &&
        files.some(function hasSelected(file) {
          return file.path === selectedPath
        })
      ) {
        return selectedPath
      }
      return fallbackPath
    },
    [fallbackPath, files, selectedPath],
  )

  const savedContentMap = useMemo(
    function memoSavedContentMap() {
      return {
        ...(memoryContentsQuery.data || {}),
        ...savedOverrides,
      }
    },
    [memoryContentsQuery.data, savedOverrides],
  )

  const mergedContentMap = useMemo(
    function memoMergedContentMap() {
      return {
        ...savedContentMap,
        ...drafts,
      }
    },
    [drafts, savedContentMap],
  )

  const activeSavedContent = activePath ? savedContentMap[activePath] || '' : ''
  const activeDraftContent = activePath
    ? mergedContentMap[activePath] || ''
    : ''
  const activeDirty = activeDraftContent !== activeSavedContent

  const searchResults = useMemo(
    function memoResults() {
      return buildSearchResults(searchQuery, mergedContentMap)
    },
    [mergedContentMap, searchQuery],
  )

  const saveFile = useCallback(
    async function saveFile(pathValue: string) {
      const nextContent = mergedContentMap[pathValue] || ''
      setSaveState('saving')
      try {
        const response = await fetch('/api/files', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            action: 'write',
            path: pathValue,
            content: nextContent,
          }),
        })
        if (!response.ok) {
          throw new Error(`Failed to save ${pathValue}`)
        }
        setSavedOverrides(function setOverride(previous) {
          return {
            ...previous,
            [pathValue]: nextContent,
          }
        })
        setLastSavedAt(new Date().toISOString())
        setSaveState('saved')
      } catch {
        setSaveState('error')
      }
    },
    [mergedContentMap],
  )

  useEffect(
    function syncAutoSave() {
      if (!activePath) return
      if (readOnly) return
      if (!activeDirty) return
      setSaveState('unsaved')
      const timerId = window.setTimeout(function autosaveAfterDelay() {
        void saveFile(activePath)
      }, 1200)
      return function cleanupTimer() {
        window.clearTimeout(timerId)
      }
    },
    [activeDirty, activePath, readOnly, saveFile],
  )

  const isLoading = memoryIndexQuery.isLoading || memoryContentsQuery.isLoading
  const loadError =
    memoryContentsQuery.error instanceof Error
      ? memoryContentsQuery.error.message
      : null

  return (
    <div className="h-screen bg-surface text-primary-900">
      <div className="flex h-full min-h-0 flex-col lg:flex-row">
        <AnimatePresence initial={false}>
          {!collapsedList ? (
            <MemoryFileList
              rootFile={rootFile}
              groups={groups}
              selectedPath={activePath}
              loading={isLoading}
              error={loadError}
              isDemo={false}
              collapsed={collapsedList}
              onToggleCollapse={function onToggleCollapse() {
                setCollapsedList(true)
              }}
              onRefresh={function onRefresh() {
                void memoryIndexQuery.refetch()
                void memoryContentsQuery.refetch()
              }}
              onSelectPath={function onSelectPath(pathValue) {
                setSelectedPath(pathValue)
              }}
            />
          ) : null}
        </AnimatePresence>

        <main className="flex min-h-0 min-w-0 flex-1 flex-col">
          <header className="border-b border-primary-200 bg-primary-50/85 px-3 py-3 backdrop-blur-sm">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <HugeiconsIcon icon={BrainIcon} size={20} strokeWidth={1.5} />
                  <h1 className="truncate text-base font-medium text-primary-900 text-balance">
                    Memory Viewer
                  </h1>
                </div>
                <p className="text-xs text-primary-600 text-pretty">
                  Browse, search, and edit agent memory files.
                </p>
              </div>
              <div className="flex items-center gap-1.5">
                <Button
                  size="sm"
                  variant="outline"
                  onClick={function onToggleList() {
                    setCollapsedList(function toggle(previous) {
                      return !previous
                    })
                  }}
                  className="tabular-nums"
                >
                  <HugeiconsIcon
                    icon={SidebarLeft01Icon}
                    size={20}
                    strokeWidth={1.5}
                  />
                  Files
                </Button>
                <Button
                  size="sm"
                  variant={previewVisible ? 'secondary' : 'outline'}
                  onClick={function onTogglePreview() {
                    setPreviewVisible(function toggle(previous) {
                      return !previous
                    })
                  }}
                  className="tabular-nums"
                >
                  <HugeiconsIcon icon={ViewIcon} size={20} strokeWidth={1.5} />
                  Preview
                </Button>
              </div>
            </div>
          </header>

          <MemorySearch
            query={searchQuery}
            searching={false}
            results={searchResults}
            onQueryChange={function onQueryChange(nextQuery) {
              setSearchQuery(nextQuery)
            }}
            onSelectResult={function onSelectResult(result) {
              setSelectedPath(result.path)
            }}
          />

          <div
            className={cn(
              'flex min-h-0 flex-1 flex-col',
              previewVisible ? 'lg:flex-row' : '',
            )}
          >
            <motion.div layout className="min-h-0 flex-1">
              <MemoryEditor
                path={activePath}
                content={activeDraftContent}
                loading={isLoading}
                error={loadError}
                readOnly={readOnly}
                saveState={saveState}
                lastSavedAt={lastSavedAt}
                theme={resolvedTheme}
                editorFontSize={settings.editorFontSize}
                editorWordWrap={settings.editorWordWrap}
                editorMinimap={settings.editorMinimap}
                onChangeContent={function onChangeContent(nextValue) {
                  if (!activePath) return
                  setDrafts(function setDraft(previous) {
                    return {
                      ...previous,
                      [activePath]: nextValue,
                    }
                  })
                  setSaveState('unsaved')
                }}
                onSave={function onSave() {
                  if (!activePath || readOnly) return
                  void saveFile(activePath)
                }}
                onToggleReadOnly={function onToggleReadOnly(next) {
                  setReadOnly(next)
                }}
              />
            </motion.div>
            <AnimatePresence initial={false}>
              {previewVisible ? (
                <motion.div
                  key="preview"
                  layout
                  initial={{ width: 0, opacity: 0 }}
                  animate={{ width: '100%', opacity: 1 }}
                  exit={{ width: 0, opacity: 0 }}
                  transition={{ duration: 0.2, ease: 'easeOut' }}
                  className="min-h-[240px] border-t border-primary-200 lg:min-h-0 lg:w-[38%] lg:border-t-0"
                >
                  <MemoryPreview
                    path={activePath}
                    content={activeDraftContent}
                  />
                </motion.div>
              ) : null}
            </AnimatePresence>
          </div>
        </main>
      </div>
    </div>
  )
}
