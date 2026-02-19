import { Avatar } from '@base-ui/react/avatar'
import { Markdown } from './markdown'
import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

export type MessageProps = {
  children: React.ReactNode
  className?: string
} & React.HTMLProps<HTMLDivElement>

function Message({ children, className, ...props }: MessageProps) {
  return (
    <div className={cn('flex gap-3 w-full', className)} {...props}>
      {children}
    </div>
  )
}

export type MessageAvatarProps = {
  src: string
  alt: string
  fallback?: string
  delayMs?: number
  className?: string
}

function MessageAvatar({
  src,
  alt,
  fallback,
  delayMs,
  className,
}: MessageAvatarProps) {
  return (
    <Avatar.Root className={cn('h-8 w-8 shrink-0', className)}>
      <Avatar.Image src={src} alt={alt} />
      {fallback && (
        <Avatar.Fallback delay={delayMs}>{fallback}</Avatar.Fallback>
      )}
    </Avatar.Root>
  )
}

export type MessageContentProps = {
  children: React.ReactNode
  markdown?: boolean
  className?: string
} & React.ComponentProps<typeof Markdown> &
  React.HTMLProps<HTMLDivElement>

function MessageContent({
  children,
  markdown = false,
  className,
  ...props
}: MessageContentProps) {
  const classNames = cn(
    'rounded-[12px] break-words whitespace-normal min-w-0',
    className,
  )

  return markdown ? (
    <Markdown className={classNames} {...props}>
      {children as string}
    </Markdown>
  ) : (
    <div className={classNames} {...props}>
      {children}
    </div>
  )
}

export type MessageActionsProps = {
  children: React.ReactNode
  className?: string
} & React.HTMLProps<HTMLDivElement>

function MessageActions({
  children,
  className,
  ...props
}: MessageActionsProps) {
  return (
    <div
      className={cn('text-primary-600 flex items-center gap-2', className)}
      {...props}
    >
      {children}
    </div>
  )
}

export type MessageActionProps = {
  className?: string
  tooltip: React.ReactNode
  children: React.ReactNode
  side?: 'top' | 'bottom' | 'left' | 'right'
} & React.ComponentProps<typeof TooltipRoot>

function MessageAction({
  tooltip,
  children,
  className,
  side = 'top',
  ...props
}: MessageActionProps) {
  return (
    <TooltipProvider>
      <TooltipRoot {...props}>
        <TooltipTrigger>{children}</TooltipTrigger>
        <TooltipContent side={side} className={className}>
          {tooltip}
        </TooltipContent>
      </TooltipRoot>
    </TooltipProvider>
  )
}

export { Message, MessageAvatar, MessageContent, MessageActions, MessageAction }
