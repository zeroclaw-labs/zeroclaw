import { createContext, useCallback, useContext, useRef, useState } from 'react'

export type ToastType = 'success' | 'error'

export interface ToastItem {
  id: string
  message: string
  type: ToastType
}

interface ToastContextValue {
  toasts: ToastItem[]
  addToast: (message: string, type: ToastType) => void
  removeToast: (id: string) => void
}

export const ToastContext = createContext<ToastContextValue | null>(null)

const MAX_TOASTS = 5
const AUTO_DISMISS_MS = 4000

let counter = 0
function nextId(): string {
  counter += 1
  return String(counter)
}

export default function ToastProvider({ children }: { children: React.ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([])
  const timers = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map())

  const removeToast = useCallback((id: string) => {
    const timer = timers.current.get(id)
    if (timer !== undefined) {
      clearTimeout(timer)
      timers.current.delete(id)
    }
    setToasts(prev => prev.filter(t => t.id !== id))
  }, [])

  const addToast = useCallback((message: string, type: ToastType) => {
    const id = nextId()
    setToasts(prev => {
      const next = [...prev, { id, message, type }]
      return next.length > MAX_TOASTS ? next.slice(next.length - MAX_TOASTS) : next
    })
    const timer = setTimeout(() => removeToast(id), AUTO_DISMISS_MS)
    timers.current.set(id, timer)
  }, [removeToast])

  return (
    <ToastContext.Provider value={{ toasts, addToast, removeToast }}>
      {children}
    </ToastContext.Provider>
  )
}

export function useToastContext(): ToastContextValue {
  const ctx = useContext(ToastContext)
  if (!ctx) throw new Error('useToastContext must be used inside ToastProvider')
  return ctx
}
