import { useState } from 'react'
import { HugeiconsIcon } from '@hugeicons/react'
import { ArrowUp01Icon } from '@hugeicons/core-free-icons'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

type AgentChatInputProps = {
  disabled?: boolean
  isSending?: boolean
  onSend: (message: string) => Promise<void> | void
}

export function AgentChatInput({
  disabled = false,
  isSending = false,
  onSend,
}: AgentChatInputProps) {
  const [value, setValue] = useState('')

  async function submit() {
    const message = value.trim()
    if (!message || disabled || isSending) return
    setValue('')
    await onSend(message)
  }

  return (
    <div className="border-t border-primary-300/70 bg-primary-100/60 p-3 backdrop-blur-sm">
      <div className="flex items-end gap-2 rounded-2xl border border-primary-300/70 bg-primary-50/80 p-2 shadow-sm">
        <textarea
          value={value}
          rows={1}
          placeholder="Message this agent..."
          disabled={disabled || isSending}
          onChange={function handleChange(event) {
            setValue(event.target.value)
          }}
          onKeyDown={function handleKeyDown(event) {
            if (
              event.key === 'Enter' &&
              !event.shiftKey &&
              !event.nativeEvent.isComposing
            ) {
              event.preventDefault()
              void submit()
            }
          }}
          className={cn(
            'max-h-36 min-h-8 flex-1 resize-y bg-transparent px-2 py-1 text-sm text-primary-900 outline-none placeholder:text-primary-600',
            disabled ? 'cursor-not-allowed opacity-60' : '',
          )}
        />
        <Button
          size="icon-sm"
          variant="default"
          disabled={disabled || isSending || value.trim().length === 0}
          className="rounded-xl"
          onClick={function handleSendClick() {
            void submit()
          }}
          aria-label="Send message"
        >
          <HugeiconsIcon icon={ArrowUp01Icon} size={20} strokeWidth={1.5} />
        </Button>
      </div>
      <p className="mt-1 px-2 text-[11px] text-primary-700 text-pretty tabular-nums">
        Enter to send · Shift+Enter for a new line
      </p>
    </div>
  )
}
