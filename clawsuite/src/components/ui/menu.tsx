'use client'

import { Menu } from '@base-ui/react/menu'
import { cn } from '@/lib/utils'

type MenuRootProps = React.ComponentProps<typeof Menu.Root>

function MenuRoot({ children, ...props }: MenuRootProps) {
  return <Menu.Root {...props}>{children}</Menu.Root>
}

type MenuTriggerProps = React.ComponentProps<typeof Menu.Trigger>

function MenuTrigger({ className, ...props }: MenuTriggerProps) {
  return <Menu.Trigger className={cn(className)} {...props} />
}

type MenuContentProps = {
  className?: string
  side?: 'top' | 'bottom' | 'left' | 'right'
  align?: 'start' | 'center' | 'end'
  children: React.ReactNode
}

function MenuContent({
  className,
  side = 'bottom',
  align = 'end',
  children,
}: MenuContentProps) {
  return (
    <Menu.Portal>
      <Menu.Positioner side={side} align={align}>
        <Menu.Popup
          className={cn(
            'min-w-[110px] rounded-lg bg-primary-50 p-1 text-sm text-primary-900 shadow-lg outline outline-primary-900/10',
            className,
          )}
        >
          {children}
        </Menu.Popup>
      </Menu.Positioner>
    </Menu.Portal>
  )
}

type MenuItemProps = React.ComponentProps<typeof Menu.Item>

function MenuItem({ className, ...props }: MenuItemProps) {
  return (
    <Menu.Item
      className={cn(
        'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-primary-900 hover:bg-primary-100 data-highlighted:bg-primary-100',
        'select-none font-[450]',
        className,
      )}
      {...props}
    />
  )
}

export { MenuRoot, MenuTrigger, MenuContent, MenuItem }
