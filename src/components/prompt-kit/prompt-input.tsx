'use client'

import React, {
  createContext,
  useContext,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

type PromptInputContextType = {
  isLoading: boolean
  value: string
  setValue: (value: string) => void
  maxHeight: number | string
  onSubmit?: () => void
  disabled?: boolean
  textareaRef: React.RefObject<HTMLTextAreaElement | null>
}

const PromptInputContext = createContext<PromptInputContextType>({
  isLoading: false,
  value: '',
  setValue: () => {},
  maxHeight: 240,
  onSubmit: undefined,
  disabled: false,
  textareaRef: React.createRef<HTMLTextAreaElement>(),
})

let globalPromptTarget: HTMLTextAreaElement | null = null
let isGlobalListenerBound = false

function bindGlobalPromptListener() {
  if (isGlobalListenerBound || typeof window === 'undefined') return
  isGlobalListenerBound = true
  window.addEventListener('keydown', (event) => {
    if (event.defaultPrevented) return
    if (event.metaKey || event.ctrlKey || event.altKey) return
    const target = event.target as HTMLElement | null
    if (!target) return
    const tag = target.tagName.toLowerCase()
    if (
      tag === 'input' ||
      tag === 'textarea' ||
      tag === 'select' ||
      target.isContentEditable
    ) {
      return
    }
    const isPrintable = event.key.length === 1
    const isEditKey = event.key === 'Backspace'
    if (!isPrintable && !isEditKey) return
    if (!globalPromptTarget || globalPromptTarget.disabled) return
    focusTextareaTarget(globalPromptTarget)
  })
}

function usePromptInput() {
  return useContext(PromptInputContext)
}

function focusTextareaTarget(target: HTMLTextAreaElement | null) {
  if (!target) return
  try {
    target.focus({ preventScroll: true })
  } catch {
    target.focus()
  }
}

function isInteractiveTarget(target: HTMLElement | null): boolean {
  if (!target) return false
  return Boolean(
    target.closest(
      'button, a, select, input[type="file"], [role="button"], [contenteditable]',
    ),
  )
}

export type PromptInputProps = {
  isLoading?: boolean
  value?: string
  onValueChange?: (value: string) => void
  maxHeight?: number | string
  onSubmit?: () => void
  children: React.ReactNode
  className?: string
  disabled?: boolean
} & React.ComponentProps<'div'>

function PromptInput({
  className,
  isLoading = false,
  maxHeight = 240,
  value,
  onValueChange,
  onSubmit,
  children,
  disabled = false,
  onClick,
  onPointerDown,
  ...props
}: PromptInputProps) {
  const [internalValue, setInternalValue] = useState(value || '')
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  bindGlobalPromptListener()

  function handleChange(newValue: string) {
    setInternalValue(newValue)
    onValueChange?.(newValue)
  }

  function handleClick(e: React.MouseEvent<HTMLDivElement>) {
    const target = e.target instanceof HTMLElement ? e.target : null
    if (!disabled && !isInteractiveTarget(target)) {
      focusTextareaTarget(textareaRef.current)
    }
    onClick?.(e)
  }

  function handlePointerDown(e: React.PointerEvent<HTMLDivElement>) {
    const target = e.target instanceof HTMLElement ? e.target : null
    if (
      !disabled &&
      e.pointerType === 'touch' &&
      !isInteractiveTarget(target)
    ) {
      focusTextareaTarget(textareaRef.current)
    }
    onPointerDown?.(e)
  }

  return (
    <TooltipProvider>
      <PromptInputContext.Provider
        value={{
          isLoading,
          value: value ?? internalValue,
          setValue: onValueChange ?? handleChange,
          maxHeight,
          onSubmit,
          disabled,
          textareaRef,
        }}
      >
        <div
          onClick={handleClick}
          onPointerDown={handlePointerDown}
          className={cn(
            'bg-surface cursor-text rounded-[22px] outline outline-ink/10 shadow-[0px_12px_32px_0px_rgba(0,0,0,0.05)] py-3 gap-3 flex flex-col touch-manipulation',
            disabled && 'cursor-not-allowed opacity-60',
            className,
          )}
          {...props}
        >
          {children}
        </div>
      </PromptInputContext.Provider>
    </TooltipProvider>
  )
}

export type PromptInputTextareaProps = {
  disableAutosize?: boolean
  inputRef?: React.Ref<HTMLTextAreaElement>
} & React.ComponentProps<'textarea'>

function PromptInputTextarea({
  className,
  onKeyDown,
  disableAutosize = false,
  inputRef,
  ...props
}: PromptInputTextareaProps) {
  const { value, setValue, maxHeight, onSubmit, disabled, textareaRef } =
    usePromptInput()

  function adjustHeight(el: HTMLTextAreaElement | null) {
    if (!el || disableAutosize) return

    el.style.height = 'auto'

    if (typeof maxHeight === 'number') {
      el.style.height = `${Math.min(el.scrollHeight, maxHeight)}px`
    } else {
      el.style.height = `min(${el.scrollHeight}px, ${maxHeight})`
    }
  }

  function handleRef(el: HTMLTextAreaElement | null) {
    textareaRef.current = el
    if (typeof inputRef === 'function') {
      inputRef(el)
    } else if (inputRef && 'current' in inputRef) {
      inputRef.current = el
    }
    if (el) {
      globalPromptTarget = el
    } else if (globalPromptTarget === el) {
      globalPromptTarget = null
    }
    adjustHeight(el)
  }

  useLayoutEffect(() => {
    if (!textareaRef.current || disableAutosize) return

    const el = textareaRef.current
    el.style.height = 'auto'

    if (typeof maxHeight === 'number') {
      el.style.height = `${Math.min(el.scrollHeight, maxHeight)}px`
    } else {
      el.style.height = `min(${el.scrollHeight}px, ${maxHeight})`
    }
  }, [value, maxHeight, disableAutosize])

  function handleChange(e: React.ChangeEvent<HTMLTextAreaElement>) {
    adjustHeight(e.target)
    setValue(e.target.value)
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      onSubmit?.()
    }
    onKeyDown?.(e)
  }

  /**
   * iOS Safari fix: onPointerDown ensures the textarea gets focus even when
   * tapped from deep scroll positions or after the keyboard was recently dismissed.
   * Without this, iOS sometimes swallows the tap and the keyboard never opens.
   */
  function handlePointerDown(e: React.PointerEvent<HTMLTextAreaElement>) {
    // Only on touch devices; avoid interfering with mouse selection
    if (e.pointerType !== 'touch') return
    const el = e.currentTarget
    // Use rAF to let the browser process the touch event first
    requestAnimationFrame(() => {
      try {
        el.focus({ preventScroll: true })
      } catch {
        el.focus()
      }
    })
    props.onPointerDown?.(e)
  }

  return (
    <textarea
      ref={handleRef}
      value={value}
      onChange={handleChange}
      onKeyDown={handleKeyDown}
      onPointerDown={handlePointerDown}
      className={cn(
        'text-primary-950 min-h-[28px] w-full resize-none border-none bg-transparent shadow-none outline-none focus-visible:ring-0 pl-4 pr-1 py-2 md:py-0 text-[15px] placeholder:text-primary-500',
        className,
      )}
      rows={1}
      enterKeyHint="send"
      readOnly={disabled}
      aria-disabled={disabled}
      {...props}
    />
  )
}

export type PromptInputActionsProps = React.HTMLAttributes<HTMLDivElement>

function PromptInputActions({
  children,
  className,
  ...props
}: PromptInputActionsProps) {
  return (
    <div className={cn('flex items-center gap-2', className)} {...props}>
      {children}
    </div>
  )
}

export type PromptInputActionProps = {
  className?: string
  tooltip: React.ReactNode
  children: React.ReactNode
  side?: 'top' | 'bottom' | 'left' | 'right'
} & React.ComponentProps<typeof TooltipRoot>

function PromptInputAction({
  tooltip,
  children,
  className,
  side = 'top',
  ...props
}: PromptInputActionProps) {
  const { disabled } = usePromptInput()
  const trigger = React.isValidElement(children) ? (
    children
  ) : (
    <span>{children}</span>
  )

  return (
    <TooltipRoot {...props}>
      <TooltipTrigger
        disabled={disabled}
        onClick={(event) => event.stopPropagation()}
        render={trigger}
      />
      <TooltipContent side={side} className={className}>
        {tooltip}
      </TooltipContent>
    </TooltipRoot>
  )
}

export {
  PromptInput,
  PromptInputTextarea,
  PromptInputActions,
  PromptInputAction,
}
