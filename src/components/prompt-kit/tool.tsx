'use client'

import { HugeiconsIcon } from '@hugeicons/react'
import { ArrowDown01Icon } from '@hugeicons/core-free-icons'
import {
  Collapsible,
  CollapsiblePanel,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import { Button } from '@/components/ui/button'

export type ToolPart = {
  type: string
  state:
    | 'input-streaming'
    | 'input-available'
    | 'output-available'
    | 'output-error'
  input?: Record<string, unknown>
  output?: Record<string, unknown>
  toolCallId?: string
  errorText?: string
}

export type ToolProps = {
  toolPart: ToolPart
  defaultOpen?: boolean
}

function Tool({ toolPart, defaultOpen = false }: ToolProps) {
  const { state, input, output, toolCallId } = toolPart

  const formatValue = (value: unknown): unknown => {
    if (value === null) return 'null'
    if (value === undefined) return 'undefined'
    if (typeof value === 'string') {
      // Try to parse as JSON for pretty display
      try {
        const parsed = JSON.parse(value)
        return parsed
      } catch {
        return value
      }
    }
    return value
  }

  const renderValue = (value: unknown): React.ReactNode => {
    const formatted = formatValue(value)
    if (typeof formatted === 'object' && formatted !== null) {
      return (
        <pre className="whitespace-pre-wrap break-all font-mono text-xs leading-relaxed">
          {JSON.stringify(formatted, null, 2)}
        </pre>
      )
    }
    return <span className="break-all">{String(formatted)}</span>
  }

  return (
    <div className="inline-flex flex-col">
      <Collapsible defaultOpen={defaultOpen}>
        <CollapsibleTrigger
          render={
            <Button
              variant="ghost"
              className="h-auto gap-1.5 px-1.5 py-0.5 -mx-2"
            />
          }
        >
          <span className="text-sm font-medium text-primary-900">
            {toolPart.type}
          </span>
          <HugeiconsIcon
            icon={ArrowDown01Icon}
            size={14}
            strokeWidth={1.5}
            className="text-primary-900 transition-transform duration-150 group-data-panel-open:rotate-180"
          />
        </CollapsibleTrigger>
        <CollapsiblePanel className="mt-1">
          <div className="space-y-2 bg-primary-100 p-2 border border-primary-200">
            {input && Object.keys(input).length > 0 && (
              <div className="border border-primary-200 bg-primary-50 p-3">
                <h4 className="text-primary-600 mb-2 text-xs font-medium">
                  Input
                </h4>
                <div className="max-h-40 overflow-auto space-y-2 font-mono text-xs text-primary-800">
                  {Object.entries(input).map(([key, value]) => (
                    <div key={key} className="break-all">
                      <span className="text-primary-500">{key}:</span>{' '}
                      <span className="text-primary-700">
                        {renderValue(value)}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {output && (
              <div className="border border-primary-200 bg-primary-50 p-3">
                <h4 className="text-primary-600 mb-2 text-xs font-medium">
                  Output
                </h4>
                <div className="max-h-40 overflow-auto font-mono text-xs text-primary-800">
                  {renderValue(output)}
                </div>
              </div>
            )}

            {state === 'output-error' && toolPart.errorText && (
              <div className="rounded-md bg-red-50 p-2">
                <h4 className="mb-1 text-xs font-medium text-red-600">Error</h4>
                <div className="text-xs text-red-700">{toolPart.errorText}</div>
              </div>
            )}

            {state === 'input-streaming' && (
              <div className="text-primary-500 text-xs">Processing...</div>
            )}

            {toolCallId && (
              <div className="text-primary-400 text-xs">
                <span className="font-mono tabular-nums">
                  ID: {toolCallId.slice(0, 16)}...
                </span>
              </div>
            )}
          </div>
        </CollapsiblePanel>
      </Collapsible>
    </div>
  )
}

export { Tool }
