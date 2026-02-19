/**
 * Lightweight toast notification system.
 * Usage: import { toast } from '@/components/ui/toast'
 *        toast('Context compacted', { type: 'info' })
 */
import { useCallback, useEffect, useState } from 'react'
import { createPortal } from 'react-dom'
import { cn } from '@/lib/utils'

type ToastType = 'info' | 'success' | 'warning' | 'error'

interface ToastItem {
  id: number
  message: string
  type: ToastType
  duration: number
  icon?: string
}

let toastId = 0
const listeners: Set<(t: ToastItem) => void> = new Set()

export function toast(
  message: string,
  opts?: { type?: ToastType; duration?: number; icon?: string },
) {
  const item: ToastItem = {
    id: ++toastId,
    message,
    type: opts?.type ?? 'info',
    duration: opts?.duration ?? 5000,
    icon: opts?.icon,
  }
  listeners.forEach((fn) => fn(item))
}

const typeStyles: Record<ToastType, string> = {
  info: 'bg-accent-600 text-white',
  success: 'bg-green-600 text-white',
  warning: 'bg-amber-500 text-white',
  error: 'bg-red-600 text-white',
}

const defaultIcons: Record<ToastType, string> = {
  info: 'ℹ️',
  success: '✅',
  warning: '⚠️',
  error: '❌',
}

export function Toaster() {
  const [toasts, setToasts] = useState<ToastItem[]>([])

  const addToast = useCallback((item: ToastItem) => {
    setToasts((prev) => {
      // Dedupe: skip if same message + type already visible
      if (prev.some((t) => t.message === item.message && t.type === item.type)) {
        return prev
      }
      return [...prev.slice(-4), item] // max 5
    })
    setTimeout(() => {
      setToasts((prev) => prev.filter((t) => t.id !== item.id))
    }, item.duration)
  }, [])

  useEffect(() => {
    listeners.add(addToast)
    return () => {
      listeners.delete(addToast)
    }
  }, [addToast])

  if (!toasts.length) return null

  return createPortal(
    <div className="fixed top-4 right-4 z-[9999] flex flex-col gap-2 pointer-events-none">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={cn(
            'pointer-events-auto flex items-center gap-2.5 rounded-xl px-4 py-3 text-sm font-medium shadow-lg backdrop-blur-sm animate-in slide-in-from-right-5 fade-in duration-200',
            typeStyles[t.type],
          )}
        >
          <span className="text-base">{t.icon ?? defaultIcons[t.type]}</span>
          <span>{t.message}</span>
          <button
            type="button"
            onClick={() =>
              setToasts((prev) => prev.filter((x) => x.id !== t.id))
            }
            className="ml-2 rounded-full p-0.5 opacity-70 hover:opacity-100 transition-opacity"
          >
            ✕
          </button>
        </div>
      ))}
    </div>,
    document.body,
  )
}
