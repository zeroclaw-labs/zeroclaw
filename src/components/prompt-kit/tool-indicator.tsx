'use client'

import { HugeiconsIcon } from '@hugeicons/react'
import { ArrowDown01Icon, Wrench01Icon } from '@hugeicons/core-free-icons'
import { useLayoutEffect, useState } from 'react'
import { Tool } from './tool'
import type { ToolPart } from './tool'
import { Button } from '@/components/ui/button'
import {
  Collapsible,
  CollapsiblePanel,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'

export type ToolIndicatorProps = {
  tools: Array<ToolPart>
  defaultOpen?: boolean
}

function ToolIndicator({ tools, defaultOpen = false }: ToolIndicatorProps) {
  if (tools.length === 0) return null

  const [isOpen, setIsOpen] = useState(defaultOpen)

  useLayoutEffect(() => {
    if (defaultOpen) {
      setIsOpen(true)
    }
  }, [defaultOpen])

  const toolCount = tools.length
  const toolLabel = toolCount === 1 ? '1 tool used' : `${toolCount} tools used`

  return (
    <div className="inline-flex flex-col">
      <Collapsible open={isOpen} onOpenChange={setIsOpen}>
        <CollapsibleTrigger
          render={
            <Button
              variant="ghost"
              className="h-auto gap-1.5 px-2 py-1 -mx-2 text-primary-500 hover:text-primary-700 hover:bg-primary-50"
            />
          }
        >
          <HugeiconsIcon
            icon={Wrench01Icon}
            size={14}
            strokeWidth={1.5}
            className="opacity-60"
          />
          <span className="text-xs font-medium">{toolLabel}</span>
          <HugeiconsIcon
            icon={ArrowDown01Icon}
            size={12}
            strokeWidth={1.5}
            className="opacity-60 transition-transform duration-150 group-data-panel-open:rotate-180"
          />
        </CollapsibleTrigger>
        <CollapsiblePanel className="mt-2">
          <div className="flex flex-col gap-2 pl-2 border-l-2 border-primary-200">
            {tools.map((toolPart) => (
              <Tool
                key={toolPart.toolCallId || toolPart.type}
                toolPart={toolPart}
                defaultOpen={false}
              />
            ))}
          </div>
        </CollapsiblePanel>
      </Collapsible>
    </div>
  )
}

export { ToolIndicator }
