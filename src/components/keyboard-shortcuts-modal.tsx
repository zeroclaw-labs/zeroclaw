'use client'

import { AnimatePresence, motion } from 'motion/react'
import { useEffect, useState } from 'react'
import { createPortal } from 'react-dom'

const isMac =
  typeof navigator !== 'undefined' &&
  /Mac|iPod|iPhone|iPad/.test(navigator.userAgent)
const MOD = isMac ? '⌘' : 'Ctrl'

const SHORTCUT_GROUPS = [
  {
    title: 'Navigation',
    items: [
      { keys: [`${MOD}+K`], label: 'Open Search' },
      { keys: [`${MOD}+P`], label: 'Quick Open File' },
      { keys: [`${MOD}+B`], label: 'Toggle Sidebar' },
      { keys: [`${MOD}+J`], label: 'Toggle Chat Panel' },
      { keys: [`${MOD}+Shift+L`], label: 'Activity Log' },
      { keys: ['Ctrl+`'], label: 'Toggle Terminal' },
      { keys: ['?'], label: 'Keyboard Shortcuts' },
    ],
  },
  {
    title: 'Chat',
    items: [
      { keys: ['Enter'], label: 'Send Message' },
      { keys: ['Shift+Enter'], label: 'New Line' },
      { keys: ['Escape'], label: 'Close Modal / Cancel' },
    ],
  },
  {
    title: 'Editor',
    items: [{ keys: [`${MOD}+S`], label: 'Save File' }],
  },
]

export function KeyboardShortcutsModal() {
  const [isOpen, setIsOpen] = useState(false)

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      // Only trigger on '?' when no input/textarea is focused
      if (
        event.key === '?' &&
        !event.metaKey &&
        !event.ctrlKey &&
        !event.altKey
      ) {
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
        const tag = (event.target as HTMLElement)?.tagName?.toLowerCase()
        // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition -- runtime safety
        if (
          tag === 'input' ||
          tag === 'textarea' ||
          (event.target as HTMLElement)?.isContentEditable
        ) {
          return
        }
        event.preventDefault()
        setIsOpen((prev) => !prev)
      }

      if (event.key === 'Escape' && isOpen) {
        setIsOpen(false)
      }
    }

    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [isOpen])

  if (typeof document === 'undefined') return null

  return createPortal(
    <AnimatePresence>
      {isOpen && (
        <motion.div
          className="fixed inset-0 z-[100] flex items-center justify-center"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.15 }}
        >
          {/* Backdrop */}
          <motion.div
            className="absolute inset-0 bg-black/60 backdrop-blur-sm"
            onClick={() => setIsOpen(false)}
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
          />

          {/* Modal */}
          <motion.div
            className="relative z-10 mx-4 w-full max-w-lg overflow-hidden rounded-2xl border border-primary-200 bg-primary-50/95 shadow-2xl backdrop-blur-xl"
            initial={{ opacity: 0, scale: 0.95, y: 10 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.95, y: 10 }}
            transition={{ duration: 0.15, ease: 'easeOut' }}
          >
            {/* Header */}
            <div className="flex items-center justify-between border-b border-primary-200 px-5 py-3.5">
              <h2 className="text-sm font-semibold text-primary-900">
                Keyboard Shortcuts
              </h2>
              <button
                onClick={() => setIsOpen(false)}
                className="rounded-lg p-1.5 text-primary-500 transition hover:bg-primary-100 hover:text-primary-900"
                aria-label="Close"
              >
                <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                  <path
                    d="M4 4L12 12M12 4L4 12"
                    stroke="currentColor"
                    strokeWidth="1.5"
                    strokeLinecap="round"
                  />
                </svg>
              </button>
            </div>

            {/* Content */}
            <div className="max-h-[60vh] overflow-y-auto p-5">
              {SHORTCUT_GROUPS.map((group) => (
                <div key={group.title} className="mb-5 last:mb-0">
                  <h3 className="mb-2.5 text-xs font-medium uppercase tracking-wider text-primary-500">
                    {group.title}
                  </h3>
                  <div className="space-y-1.5">
                    {group.items.map((item) => (
                      <div
                        key={item.label}
                        className="flex items-center justify-between rounded-lg px-2 py-1.5"
                      >
                        <span className="text-sm text-primary-700">
                          {item.label}
                        </span>
                        <div className="flex items-center gap-1">
                          {item.keys.map((key) => (
                            <kbd
                              key={key}
                              className="inline-flex min-w-[24px] items-center justify-center rounded-md border border-primary-200 bg-primary-100/80 px-1.5 py-0.5 text-xs font-medium text-primary-700"
                            >
                              {key}
                            </kbd>
                          ))}
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
              ))}
            </div>

            {/* Footer */}
            <div className="border-t border-primary-200 px-5 py-2.5 text-center text-xs text-primary-500">
              Press{' '}
              <kbd className="mx-0.5 rounded border border-primary-200 bg-primary-100/80 px-1 text-[10px] font-medium">
                ?
              </kbd>{' '}
              to toggle ·{' '}
              <kbd className="mx-0.5 rounded border border-primary-200 bg-primary-100/80 px-1 text-[10px] font-medium">
                Esc
              </kbd>{' '}
              to close
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  )
}
