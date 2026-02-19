import { Cancel01Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { Button } from '@/components/ui/button'
import {
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogRoot,
  DialogTitle,
} from '@/components/ui/dialog'
import { ProvidersScreen } from '@/screens/settings/providers-screen'

type ProvidersDialogProps = {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function ProvidersDialog({ open, onOpenChange }: ProvidersDialogProps) {
  return (
    <DialogRoot open={open} onOpenChange={onOpenChange}>
      <DialogContent className="flex h-[min(85dvh,680px)] w-[min(640px,92vw)] max-h-[calc(100dvh-3rem)] flex-col overflow-hidden p-0">
        <div className="flex items-start justify-between border-b border-primary-200 p-4 pb-3">
          <div>
            <DialogTitle className="mb-1 text-balance">Providers</DialogTitle>
            <DialogDescription className="text-pretty">
              Configure provider access without leaving your current page.
            </DialogDescription>
          </div>
          <DialogClose
            render={
              <Button
                size="icon-sm"
                variant="ghost"
                className="text-primary-500 hover:bg-primary-100 hover:text-primary-700"
                aria-label="Close providers dialog"
              >
                <HugeiconsIcon
                  icon={Cancel01Icon}
                  size={20}
                  strokeWidth={1.5}
                />
              </Button>
            }
          />
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto">
          <ProvidersScreen embedded />
        </div>
      </DialogContent>
    </DialogRoot>
  )
}
