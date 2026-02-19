'use client'

import { HugeiconsIcon } from '@hugeicons/react'
import { ArrowDown01Icon, Idea01Icon } from '@hugeicons/core-free-icons'
import { useLayoutEffect, useState } from 'react'
import {
  Collapsible,
  CollapsiblePanel,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { Button } from '@/components/ui/button'
import { LoadingIndicator } from '@/components/loading-indicator'

export type ThinkingIndicatorProps = {
  content: string
  defaultOpen?: boolean
  isStreaming?: boolean
}

function ThinkingIndicator({
  content,
  defaultOpen = false,
  isStreaming = false,
}: ThinkingIndicatorProps) {
  if (!content || content.trim().length === 0) return null

  const [isOpen, setIsOpen] = useState(() => isStreaming || defaultOpen)

  useLayoutEffect(() => {
    if (isStreaming || defaultOpen) {
      setIsOpen(true)
    }
  }, [defaultOpen, isStreaming])

  return (
    <div className="inline-flex flex-col">
      <Collapsible open={isOpen} onOpenChange={setIsOpen}>
        <CollapsibleTrigger
          render={
            <Button
              variant="ghost"
              className="h-auto gap-1.5 px-2 py-1 -mx-2 text-primary-500 hover:text-primary-700 hover:bg-primary-100"
            />
          }
        >
          <HugeiconsIcon
            icon={Idea01Icon}
            size={20}
            strokeWidth={1.5}
            className="opacity-60"
          />
          <span className="text-xs font-medium">
            {isStreaming ? 'Thinking live' : 'Thought process'}
          </span>
          {isStreaming ? (
            <LoadingIndicator ariaLabel="Assistant thinking" />
          ) : null}
          <HugeiconsIcon
            icon={ArrowDown01Icon}
            size={20}
            strokeWidth={1.5}
            className="opacity-60 transition-transform duration-150 group-data-panel-open:rotate-180"
          />
        </CollapsibleTrigger>
        <CollapsiblePanel className="mt-1">
          <div className="pl-2 border-l-2 border-primary-200 py-2">
            <p className="text-sm text-primary-600 whitespace-pre-wrap text-pretty">
              {content}
            </p>
          </div>
        </CollapsiblePanel>
      </Collapsible>
    </div>
  )
}

export { ThinkingIndicator }
