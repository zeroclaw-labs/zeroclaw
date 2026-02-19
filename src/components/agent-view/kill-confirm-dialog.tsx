import { useState } from 'react'
import { Button } from '@/components/ui/button'
import {
  DialogContent,
  DialogDescription,
  DialogRoot,
  DialogTitle,
} from '@/components/ui/dialog'
import { toast } from '@/components/ui/toast'
import { killAgentSession } from '@/lib/gateway-api'

type KillConfirmDialogProps = {
  open: boolean
  agentName: string
  sessionKey?: string
  onOpenChange: (open: boolean) => void
  onKilled?: () => void
}

export function KillConfirmDialog({
  open,
  agentName,
  sessionKey,
  onOpenChange,
  onKilled,
}: KillConfirmDialogProps) {
  const [pending, setPending] = useState(false)

  async function handleConfirmKill() {
    const normalizedSessionKey = sessionKey?.trim() ?? ''
    if (!normalizedSessionKey || pending) return

    setPending(true)
    try {
      await killAgentSession(normalizedSessionKey)
      toast(`${agentName} terminated`, { type: 'success' })
      onOpenChange(false)
      onKilled?.()
    } catch (error) {
      const message =
        error instanceof Error ? error.message : 'Failed to terminate agent'
      toast(message, { type: 'error' })
    } finally {
      setPending(false)
    }
  }

  return (
    <DialogRoot
      open={open}
      onOpenChange={(nextOpen) => {
        if (pending && !nextOpen) return
        onOpenChange(nextOpen)
      }}
    >
      <DialogContent className="w-[min(420px,92vw)]">
        <div className="space-y-4 p-5">
          <div className="space-y-1">
            <DialogTitle className="text-base">Kill {agentName}?</DialogTitle>
            <DialogDescription>
              This will terminate the agent session immediately.
            </DialogDescription>
          </div>

          <div className="flex items-center justify-end gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={pending}
              onClick={function onClickCancel() {
                onOpenChange(false)
              }}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              size="sm"
              disabled={pending || !sessionKey}
              onClick={function onClickConfirm() {
                void handleConfirmKill()
              }}
            >
              {pending ? 'Terminating...' : 'Kill'}
            </Button>
          </div>
        </div>
      </DialogContent>
    </DialogRoot>
  )
}
