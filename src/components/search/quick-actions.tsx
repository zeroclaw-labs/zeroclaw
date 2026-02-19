import { cn } from '@/lib/utils'

type QuickAction = {
  id: string
  label: string
  description: string
  emoji: string
  onSelect: () => void
}

type QuickActionsProps = {
  recentSearches: Array<string>
  actions: Array<QuickAction>
  onSelectRecent: (value: string) => void
}

export function QuickActions({
  recentSearches,
  actions,
  onSelectRecent,
}: QuickActionsProps) {
  return (
    <div className="space-y-4">
      <div>
        <div className="mb-2 text-xs font-medium text-muted-foreground">
          Recent Searches
        </div>
        <div className="flex flex-wrap gap-2">
          {recentSearches.map((entry) => (
            <button
              key={entry}
              type="button"
              onClick={() => onSelectRecent(entry)}
              className={cn(
                'rounded-md border border-border bg-muted/60 px-2.5 py-1 text-xs text-foreground transition-colors',
                'hover:bg-muted',
              )}
            >
              {entry}
            </button>
          ))}
        </div>
      </div>

      <div>
        <div className="mb-2 text-xs font-medium text-muted-foreground">
          Quick Actions
        </div>
        <div className="grid grid-cols-2 gap-2 md:grid-cols-4">
          {actions.map((action) => (
            <button
              key={action.id}
              type="button"
              onClick={action.onSelect}
              className={cn(
                'flex min-h-20 flex-col items-start rounded-lg border border-border bg-card/80 p-3 text-left transition-colors backdrop-blur-sm',
                'hover:border-accent-500/35 hover:bg-accent-500/15',
              )}
            >
              <span className="text-base leading-none">{action.emoji}</span>
              <span className="mt-2 text-sm font-medium text-foreground text-balance">
                {action.label}
              </span>
              <span className="mt-1 line-clamp-2 text-xs text-muted-foreground text-pretty">
                {action.description}
              </span>
            </button>
          ))}
        </div>
      </div>
    </div>
  )
}

export type { QuickAction }
