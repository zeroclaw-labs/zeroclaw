'use client'

import { AlertDialog } from '@base-ui/react/alert-dialog'
import { Button } from './button'
import { cn } from '@/lib/utils'

type AlertDialogRootProps = React.ComponentProps<typeof AlertDialog.Root>

function AlertDialogRoot({ children, ...props }: AlertDialogRootProps) {
  return <AlertDialog.Root {...props}>{children}</AlertDialog.Root>
}

type AlertDialogTriggerProps = React.ComponentProps<typeof AlertDialog.Trigger>

function AlertDialogTrigger({ className, ...props }: AlertDialogTriggerProps) {
  return <AlertDialog.Trigger className={cn(className)} {...props} />
}

type AlertDialogContentProps = {
  className?: string
  children: React.ReactNode
}

function AlertDialogContent({ className, children }: AlertDialogContentProps) {
  return (
    <AlertDialog.Portal>
      <AlertDialog.Backdrop className="fixed inset-0 bg-primary-950/20 transition-all duration-150 data-[state=open]:opacity-100 data-[state=closed]:opacity-0" />
      <AlertDialog.Popup
        className={cn(
          'fixed left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2',
          'w-[min(400px,92vw)] rounded-xl border border-primary-200 bg-primary-50 p-0 shadow-xl',
          'transition-all duration-150',
          'data-[state=open]:opacity-100 data-[state=closed]:opacity-0',
          'data-[state=open]:scale-100 data-[state=closed]:scale-95',
          className,
        )}
      >
        {children}
      </AlertDialog.Popup>
    </AlertDialog.Portal>
  )
}

type AlertDialogTitleProps = React.ComponentProps<typeof AlertDialog.Title>

function AlertDialogTitle({ className, ...props }: AlertDialogTitleProps) {
  return (
    <AlertDialog.Title
      className={cn('text-lg font-medium text-primary-900', className)}
      {...props}
    />
  )
}

type AlertDialogDescriptionProps = React.ComponentProps<
  typeof AlertDialog.Description
>

function AlertDialogDescription({
  className,
  ...props
}: AlertDialogDescriptionProps) {
  return (
    <AlertDialog.Description
      className={cn('text-sm text-primary-600', className)}
      {...props}
    />
  )
}

type AlertDialogCancelProps = React.ComponentProps<typeof AlertDialog.Close>

function AlertDialogCancel({ className, ...props }: AlertDialogCancelProps) {
  return (
    <AlertDialog.Close
      render={<Button variant="outline" className={cn(className)} />}
      {...props}
    />
  )
}

type AlertDialogActionProps = React.ComponentProps<typeof AlertDialog.Close>

function AlertDialogAction({ className, ...props }: AlertDialogActionProps) {
  return (
    <AlertDialog.Close
      render={<Button variant="destructive" className={cn(className)} />}
      {...props}
    />
  )
}

export {
  AlertDialogRoot,
  AlertDialogTrigger,
  AlertDialogContent,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogCancel,
  AlertDialogAction,
}
