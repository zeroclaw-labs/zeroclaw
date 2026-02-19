/**
 * Floating button to toggle the chat panel on non-chat routes.
 * Shows in bottom-right corner. Hidden when chat panel is open.
 */
import { HugeiconsIcon } from '@hugeicons/react'
import { Chat01Icon } from '@hugeicons/core-free-icons'
import { AnimatePresence, motion } from 'motion/react'
import { useWorkspaceStore } from '@/stores/workspace-store'
import { Button } from '@/components/ui/button'
import {
  TooltipContent,
  TooltipProvider,
  TooltipRoot,
  TooltipTrigger,
} from '@/components/ui/tooltip'

export function ChatPanelToggle() {
  const isOpen = useWorkspaceStore((s) => s.chatPanelOpen)
  const toggleChatPanel = useWorkspaceStore((s) => s.toggleChatPanel)

  return (
    <AnimatePresence>
      {!isOpen && (
        <motion.div
          initial={{ opacity: 0, scale: 0.8 }}
          animate={{ opacity: 1, scale: 1 }}
          exit={{ opacity: 0, scale: 0.8 }}
          transition={{ duration: 0.15 }}
          className="fixed bottom-6 right-6 z-50"
        >
          <TooltipProvider>
            <TooltipRoot>
              <TooltipTrigger
                onClick={toggleChatPanel}
                render={
                  <Button
                    size="icon"
                    className="size-12 rounded-full bg-accent-500 text-white shadow-lg hover:bg-accent-600 active:scale-95 transition-all"
                    aria-label="Open chat"
                  >
                    <HugeiconsIcon
                      icon={Chat01Icon}
                      size={22}
                      strokeWidth={1.5}
                    />
                  </Button>
                }
              />
              <TooltipContent side="left">
                <span>
                  Chat <kbd className="ml-1 text-[10px] opacity-60">⌘J</kbd>
                </span>
              </TooltipContent>
            </TooltipRoot>
          </TooltipProvider>
        </motion.div>
      )}
    </AnimatePresence>
  )
}
