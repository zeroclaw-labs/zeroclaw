/**
 * Task System Lite — Mission Control-inspired task management.
 * localStorage-backed, zero backend dependencies.
 */
import { create } from 'zustand'
import { persist } from 'zustand/middleware'

export type TaskStatus = 'backlog' | 'in_progress' | 'review' | 'done'
export type TaskPriority = 'P0' | 'P1' | 'P2' | 'P3'

export type Task = {
  id: string
  title: string
  description: string
  status: TaskStatus
  priority: TaskPriority
  project?: string
  tags: string[]
  dueDate?: string
  reminder?: string
  createdAt: string
  updatedAt: string
}

export const STATUS_LABELS: Record<TaskStatus, string> = {
  backlog: 'Backlog',
  in_progress: 'In Progress',
  review: 'Review',
  done: 'Done',
}

export const STATUS_ORDER: TaskStatus[] = [
  'backlog',
  'in_progress',
  'review',
  'done',
]

export const PRIORITY_ORDER: TaskPriority[] = ['P0', 'P1', 'P2', 'P3']

/** Seed data from real Mission Control tasks */
const SEED_TASKS: Task[] = []

function normalizeTaskList(payload: unknown): Task[] {
  if (
    !payload ||
    typeof payload !== 'object' ||
    !Array.isArray((payload as { tasks?: unknown }).tasks)
  ) {
    return []
  }

  const tasks = (payload as { tasks: unknown[] }).tasks
  return tasks.filter((task): task is Task => {
    if (!task || typeof task !== 'object') return false
    const maybeTask = task as Partial<Task>
    return (
      typeof maybeTask.id === 'string' &&
      typeof maybeTask.title === 'string' &&
      typeof maybeTask.description === 'string' &&
      typeof maybeTask.status === 'string' &&
      typeof maybeTask.priority === 'string' &&
      Array.isArray(maybeTask.tags) &&
      typeof maybeTask.createdAt === 'string' &&
      typeof maybeTask.updatedAt === 'string'
    )
  })
}

type TaskStore = {
  tasks: Task[]
  afterSync: boolean
  syncFromApi: () => Promise<void>
  addTask: (task: Omit<Task, 'id' | 'createdAt' | 'updatedAt'>) => void
  updateTask: (
    id: string,
    updates: Partial<Omit<Task, 'id' | 'createdAt'>>,
  ) => void
  moveTask: (id: string, status: TaskStatus) => void
  deleteTask: (id: string) => void
}

export const useTaskStore = create<TaskStore>()(
  persist(
    (set) => ({
      tasks: SEED_TASKS,
      afterSync: false,
      syncFromApi: async function syncFromApi() {
        if (typeof window === 'undefined') {
          set({ afterSync: true })
          return
        }

        try {
          const response = await fetch('/api/tasks', { method: 'GET' })
          if (!response.ok)
            throw new Error(`Failed to sync tasks (${response.status})`)
          const payload = await response.json().catch(() => ({}))
          set({
            tasks: normalizeTaskList(payload),
            afterSync: true,
          })
        } catch {
          set({ afterSync: true })
        }
      },
      addTask: (taskData) => {
        const now = new Date().toISOString()
        const task: Task = {
          ...taskData,
          id: `TASK-${Date.now().toString(36).toUpperCase()}`,
          createdAt: now,
          updatedAt: now,
        }
        set((state) => ({ tasks: [task, ...state.tasks] }))
        // Persist to API
        void fetch('/api/tasks', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(task),
        }).catch(() => {})
      },
      updateTask: (id, updates) => {
        set((state) => ({
          tasks: state.tasks.map((t) =>
            t.id === id
              ? { ...t, ...updates, updatedAt: new Date().toISOString() }
              : t,
          ),
        }))
        void fetch(`/api/tasks/${id}`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(updates),
        }).catch(() => {})
      },
      moveTask: (id, status) => {
        set((state) => ({
          tasks: state.tasks.map((t) =>
            t.id === id
              ? { ...t, status, updatedAt: new Date().toISOString() }
              : t,
          ),
        }))
        void fetch(`/api/tasks/${id}`, {
          method: 'PATCH',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ status }),
        }).catch(() => {})
      },
      deleteTask: (id) => {
        set((state) => ({ tasks: state.tasks.filter((t) => t.id !== id) }))
        void fetch(`/api/tasks/${id}`, { method: 'DELETE' }).catch(() => {})
      },
    }),
    {
      name: 'clawsuite-tasks-v1',
    },
  ),
)
