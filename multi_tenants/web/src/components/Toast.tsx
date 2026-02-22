import { CheckCircle2, XCircle, X } from 'lucide-react'
import { useToastContext } from './ToastProvider'
import type { ToastItem } from './ToastProvider'

function ToastCard({ toast, onClose }: { toast: ToastItem; onClose: () => void }) {
  const isSuccess = toast.type === 'success'

  return (
    <div
      className={`flex items-start gap-3 bg-bg-card border border-border-default border-l-4 ${
        isSuccess ? 'border-l-green-500' : 'border-l-red-500'
      } rounded-lg px-4 py-3 min-w-64 max-w-sm shadow-lg`}
      role="alert"
      aria-live="polite"
    >
      {isSuccess
        ? <CheckCircle2 className="h-5 w-5 text-green-400 shrink-0 mt-0.5" />
        : <XCircle className="h-5 w-5 text-red-400 shrink-0 mt-0.5" />
      }
      <p className="flex-1 text-sm text-text-primary">{toast.message}</p>
      <button
        onClick={onClose}
        aria-label="Close notification"
        className="text-text-muted hover:text-text-primary transition-colors shrink-0"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  )
}

export default function Toast() {
  const { toasts, removeToast } = useToastContext()

  if (toasts.length === 0) return null

  return (
    <div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 items-end">
      {toasts.map(toast => (
        <ToastCard
          key={toast.id}
          toast={toast}
          onClose={() => removeToast(toast.id)}
        />
      ))}
    </div>
  )
}
