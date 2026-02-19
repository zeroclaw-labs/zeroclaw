import { cn } from '@/lib/utils'

export type SearchResultItemData = {
  id: string
  title: string
  snippet: string
  meta: string
  scope: 'chats' | 'files' | 'agents' | 'skills' | 'actions'
  icon: React.ReactNode
  badge?: string
  onSelect: () => void
}

type SearchResultItemProps = {
  item: SearchResultItemData
  selected: boolean
  query: string
  shortcut?: number
  onHover: () => void
  onSelect: () => void
}

function highlightMatches(text: string, query: string) {
  const normalized = query.trim()
  if (!normalized) return text
  const escaped = normalized.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const regex = new RegExp(`(${escaped})`, 'ig')
  const parts = text.split(regex)

  return parts.map((part, index) => {
    if (part.toLowerCase() === normalized.toLowerCase()) {
      return (
        <mark
          key={`${part}-${index}`}
          className="rounded-sm bg-accent-500/25 px-0.5 text-accent-200"
        >
          {part}
        </mark>
      )
    }
    return <span key={`${part}-${index}`}>{part}</span>
  })
}

export function SearchResultItem({
  item,
  selected,
  query,
  shortcut,
  onHover,
  onSelect,
}: SearchResultItemProps) {
  return (
    <button
      type="button"
      onMouseEnter={onHover}
      onClick={onSelect}
      className={cn(
        'group flex w-full items-start gap-3 rounded-lg border border-transparent px-3 py-2 text-left transition-colors',
        selected
          ? 'border-accent-500/35 bg-accent-500/15'
          : 'hover:border-border hover:bg-muted/70',
      )}
    >
      <div className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-md border border-border bg-muted/60 text-muted-foreground">
        {item.icon}
      </div>
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm font-medium text-foreground text-balance">
          {highlightMatches(item.title, query)}
        </div>
        <div className="mt-1 line-clamp-2 text-xs text-muted-foreground text-pretty">
          {highlightMatches(item.snippet, query)}
        </div>
      </div>
      <div className="ml-2 flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground tabular-nums">
        {item.badge ? (
          <span className="rounded-md border border-border bg-muted/50 px-1.5 py-0.5">
            {item.badge}
          </span>
        ) : null}
        {shortcut ? (
          <span className="rounded-md border border-border bg-muted/50 px-1.5 py-0.5">
            {shortcut}
          </span>
        ) : null}
        <span className="max-w-32 truncate">{item.meta}</span>
        <span className="rounded-md border border-border bg-muted/50 px-1.5 py-0.5">
          ↵
        </span>
      </div>
    </button>
  )
}
