import { useEffect, useRef } from 'react'
import { useTaskStore, type Task } from '@/stores/task-store'

/** Tracks which reminders/auto-alerts have already fired this session */
const firedReminders = new Set<string>()
const firedAutoAlerts = new Set<string>()

function showNotification(title: string, body: string) {
  // Try browser Notification API first
  if (
    typeof Notification !== 'undefined' &&
    Notification.permission === 'granted'
  ) {
    new Notification(title, { body, icon: '/favicon.ico' })
    return
  }

  // Fallback: dispatch custom event for in-app toast
  window.dispatchEvent(
    new CustomEvent('clawsuite:toast', {
      detail: { title, body, type: 'warning' },
    }),
  )
}

function isOverdue(task: Task): boolean {
  if (!task.dueDate || task.status === 'done') return false
  return new Date(task.dueDate).getTime() < Date.now()
}

function isDueSoon(task: Task): boolean {
  if (!task.dueDate || task.status === 'done') return false
  const diff = new Date(task.dueDate).getTime() - Date.now()
  return diff > 0 && diff < 24 * 60 * 60 * 1000
}

export function useTaskReminders() {
  const intervalRef = useRef<ReturnType<typeof setInterval>>(null)

  useEffect(() => {
    // Request notification permission on mount
    if (
      typeof window !== 'undefined' &&
      typeof Notification !== 'undefined' &&
      Notification.permission === 'default'
    ) {
      void Notification.requestPermission()
    }

    function check() {
      const { tasks, updateTask } = useTaskStore.getState()
      const now = Date.now()

      for (const task of tasks) {
        if (task.status === 'done') continue

        // 1. Explicit reminder — fires once, then clears
        if (task.reminder && !firedReminders.has(task.id)) {
          const reminderTime = new Date(task.reminder).getTime()
          if (reminderTime <= now) {
            firedReminders.add(task.id)
            showNotification(
              `⏰ Reminder: ${task.title}`,
              task.description || `${task.priority} · ${task.status}`,
            )
            // Clear the reminder so it doesn't fire again after page reload
            updateTask(task.id, { reminder: undefined })
            // Persist to API
            void fetch(`/api/tasks/${task.id}`, {
              method: 'PATCH',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ reminder: null }),
            })
          }
        }

        // 2. Overdue notification
        if (isOverdue(task) && !firedAutoAlerts.has(`overdue-${task.id}`)) {
          firedAutoAlerts.add(`overdue-${task.id}`)
          showNotification(
            `🚨 Overdue: ${task.title}`,
            `Was due ${new Date(task.dueDate!).toLocaleDateString()}`,
          )
        }

        // 3. Due soon (within 24h)
        if (isDueSoon(task) && !firedAutoAlerts.has(`duesoon-${task.id}`)) {
          firedAutoAlerts.add(`duesoon-${task.id}`)
          showNotification(
            `⚠️ Due soon: ${task.title}`,
            `Due ${new Date(task.dueDate!).toLocaleDateString()}`,
          )
        }

        // 4. Auto-priority reminders (P0 every 4h, P1 daily if stale)
        if (task.priority === 'P0' && !task.reminder) {
          const key = `p0-${task.id}-${Math.floor(now / (4 * 60 * 60 * 1000))}`
          if (!firedAutoAlerts.has(key)) {
            firedAutoAlerts.add(key)
            showNotification(
              `🔴 P0 Active: ${task.title}`,
              `Priority task needs attention`,
            )
          }
        }

        if (
          task.priority === 'P1' &&
          task.status === 'in_progress' &&
          !task.reminder
        ) {
          const updated = new Date(task.updatedAt).getTime()
          const twoDays = 2 * 24 * 60 * 60 * 1000
          if (now - updated > twoDays) {
            const key = `p1-stale-${task.id}-${Math.floor(now / (24 * 60 * 60 * 1000))}`
            if (!firedAutoAlerts.has(key)) {
              firedAutoAlerts.add(key)
              showNotification(
                `🟡 P1 Stale: ${task.title}`,
                `In progress for ${Math.floor((now - updated) / (24 * 60 * 60 * 1000))} days`,
              )
            }
          }
        }
      }
    }

    // Run immediately then every 60s
    check()
    intervalRef.current = setInterval(check, 60_000)

    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current)
    }
  }, [])
}
