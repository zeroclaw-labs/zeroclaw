'use client'

import { Fragment, useMemo, useState } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import { ArrowDown01Icon, ArrowUp01Icon } from '@hugeicons/core-free-icons'
import {
  Command,
  CommandCollection,
  CommandDialog,
  CommandDialogPopup,
  CommandFooter,
  CommandGroup,
  CommandGroupLabel,
  CommandInput,
  CommandItem,
  CommandList,
  CommandPanel,
  CommandSeparator,
} from '@/components/ui/command'
import { useAutocompleteFilter } from '@/components/ui/autocomplete'

type CommandSession = {
  key: string
  friendlyId: string
  label?: string
  title?: string
  derivedTitle?: string
}

type CommandSessionItem = {
  value: string
  label: string
  friendlyId: string
  session: CommandSession
}

type CommandSessionGroup = {
  value: string
  items: Array<CommandSessionItem>
}

type CommandSessionProps = {
  sessions: Array<CommandSession>
  open: boolean
  onOpenChange: (open: boolean) => void
  onSelect: (session: CommandSession) => void
}

function getSessionLabel(session: CommandSession) {
  return (
    session.label || session.title || session.derivedTitle || session.friendlyId
  )
}

function CommandSessionDialog({
  sessions,
  open,
  onOpenChange,
  onSelect,
}: CommandSessionProps) {
  const [value, setValue] = useState('')
  const filter = useAutocompleteFilter({ sensitivity: 'base' })

  const groupedItems = useMemo<Array<CommandSessionGroup>>(() => {
    return [
      {
        value: 'Sessions',
        items: sessions.map((session) => ({
          value: session.key,
          label: getSessionLabel(session),
          friendlyId: session.friendlyId,
          session,
        })),
      },
    ]
  }, [sessions])

  const filteredGroups = useMemo(() => {
    const query = value.trim()
    if (!query) return groupedItems

    return groupedItems
      .map((group) => ({
        ...group,
        items: group.items.filter((item) =>
          filter.contains(item, query, (target) => target.label),
        ),
      }))
      .filter((group) => group.items.length > 0)
  }, [filter, groupedItems, value])

  const isEmpty = filteredGroups.length === 0

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <CommandDialogPopup className="mx-auto self-center">
        <Command
          items={groupedItems}
          value={value}
          onValueChange={setValue}
          mode="none"
        >
          <CommandInput placeholder="Search sessions" />
          <CommandPanel className="flex min-h-0 flex-1 flex-col">
            {isEmpty ? (
              <div className="h-72 min-h-0 flex items-center justify-center text-sm text-primary-600">
                No sessions found.
              </div>
            ) : (
              <CommandList className="h-72 min-h-0">
                {filteredGroups.map((group, index) => (
                  <Fragment key={`${group.value}-${index}`}>
                    <CommandGroup items={group.items}>
                      <CommandGroupLabel>{group.value}</CommandGroupLabel>
                      <CommandCollection>
                        {(item) => (
                          <CommandItem
                            key={item.value}
                            value={item.label}
                            onClick={() => onSelect(item.session)}
                            className="gap-2"
                          >
                            <span className="text-sm font-[450] line-clamp-1">
                              {item.label}
                            </span>
                          </CommandItem>
                        )}
                      </CommandCollection>
                    </CommandGroup>
                    {index < filteredGroups.length - 1 ? (
                      <CommandSeparator />
                    ) : null}
                  </Fragment>
                ))}
              </CommandList>
            )}
          </CommandPanel>
          <CommandFooter>
            <div className="flex items-center gap-4 text-primary-700">
              <div className="flex items-center gap-2">
                <span className="inline-flex items-center gap-1 rounded-md border border-primary-200 bg-surface px-2 py-1 text-[11px] font-medium text-primary-700">
                  <HugeiconsIcon
                    icon={ArrowUp01Icon}
                    size={14}
                    strokeWidth={1.5}
                  />
                  <HugeiconsIcon
                    icon={ArrowDown01Icon}
                    size={14}
                    strokeWidth={1.5}
                  />
                </span>
                <span>Navigate</span>
              </div>
              <div className="flex items-center gap-2">
                <span className="rounded-md border border-primary-200 bg-surface px-2 py-1 text-[11px] font-medium text-primary-700">
                  Enter
                </span>
                <span>Open</span>
              </div>
            </div>
            <div className="flex items-center gap-2 text-primary-700">
              <span className="rounded-md border border-primary-200 bg-surface px-2 py-1 text-[11px] font-medium text-primary-700">
                Esc
              </span>
              <span>Close</span>
            </div>
          </CommandFooter>
        </Command>
      </CommandDialogPopup>
    </CommandDialog>
  )
}

export { CommandSessionDialog }
