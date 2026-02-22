import { useContext } from 'react'
import { ToastContext } from '../components/ToastProvider'

export function useToast() {
  const ctx = useContext(ToastContext)
  if (!ctx) throw new Error('useToast must be used inside ToastProvider')
  return {
    success: (msg: string) => ctx.addToast(msg, 'success'),
    error: (msg: string) => ctx.addToast(msg, 'error'),
  }
}
