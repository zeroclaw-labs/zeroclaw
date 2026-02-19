import { HugeiconsIcon } from '@hugeicons/react'
import { Cancel01Icon, Search01Icon } from '@hugeicons/core-free-icons'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

type SearchInputProps = {
  value: string
  onValueChange: (value: string) => void
  onClear: () => void
  inputRef?: React.RefObject<HTMLInputElement | null>
}

export function SearchInput({
  value,
  onValueChange,
  onClear,
  inputRef,
}: SearchInputProps) {
  const shortcut =
    typeof navigator !== 'undefined' &&
    /Mac|iPhone|iPad|iPod/i.test(navigator.platform)
      ? '⌘K'
      : 'Ctrl K'

  return (
    <div className="relative">
      <HugeiconsIcon
        icon={Search01Icon}
        size={20}
        strokeWidth={1.5}
        className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted-foreground"
      />
      <input
        ref={inputRef}
        value={value}
        onChange={(event) => onValueChange(event.target.value)}
        placeholder="Search chats, files, agents, skills..."
        className={cn(
          'h-12 w-full rounded-xl border border-border bg-muted/60 pl-10 pr-24 text-sm text-foreground outline-none',
          'placeholder:text-muted-foreground focus:border-primary focus:bg-muted',
          'text-pretty',
        )}
      />
      {value.trim().length > 0 ? (
        <Button
          size="icon-sm"
          variant="ghost"
          onClick={onClear}
          className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:bg-muted"
          aria-label="Clear search"
        >
          <HugeiconsIcon icon={Cancel01Icon} size={20} strokeWidth={1.5} />
        </Button>
      ) : (
        <div className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 rounded-md border border-border bg-muted/50 px-2 py-0.5 text-[11px] text-muted-foreground tabular-nums">
          {shortcut}
        </div>
      )}
    </div>
  )
}
