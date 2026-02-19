import { Add01Icon, Delete02Icon } from '@hugeicons/core-free-icons'
import { HugeiconsIcon } from '@hugeicons/react'
import { useCallback, useEffect, useState } from 'react'
import {
  useTaskStore,
  STATUS_ORDER,
  STATUS_LABELS,
  PRIORITY_ORDER,
  type Task,
  type TaskStatus,
  type TaskPriority,
} from '@/stores/task-store'
import { cn } from '@/lib/utils'

/* ── Helpers ── */

function priorityColor(p: string): string {
  if (p === 'P0') return 'bg-red-500/15 text-red-600 dark:text-red-400'
  if (p === 'P1') return 'bg-amber-500/15 text-amber-700 dark:text-amber-400'
  if (p === 'P2') return 'bg-primary-200/60 text-primary-600'
  return 'bg-primary-100 text-primary-400'
}

function statusDotColor(s: TaskStatus): string {
  if (s === 'in_progress') return 'bg-emerald-500'
  if (s === 'review') return 'bg-blue-500'
  if (s === 'done') return 'bg-primary-300'
  return 'bg-primary-300'
}

function formatDate(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
  }).format(d)
}

/* ── Add Task Dialog ── */

function AddTaskDialog({
  onAdd,
  onClose,
}: {
  onAdd: (task: Omit<Task, 'id' | 'createdAt' | 'updatedAt'>) => void
  onClose: () => void
}) {
  const [title, setTitle] = useState('')
  const [description, setDescription] = useState('')
  const [priority, setPriority] = useState<TaskPriority>('P1')
  const [status, setStatus] = useState<TaskStatus>('backlog')
  const [dueDate, setDueDate] = useState('')
  const [reminder, setReminder] = useState('')

  const handleSubmit = useCallback(
    (e: React.FormEvent) => {
      e.preventDefault()
      if (!title.trim()) return
      onAdd({
        title: title.trim(),
        description: description.trim(),
        status,
        priority,
        tags: [],
        ...(dueDate ? { dueDate } : {}),
        ...(reminder ? { reminder } : {}),
      })
      onClose()
    },
    [title, description, priority, status, dueDate, reminder, onAdd, onClose],
  )

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <form
        onClick={(e) => e.stopPropagation()}
        onSubmit={handleSubmit}
        className="w-full max-w-md rounded-xl border border-primary-200 bg-primary-50 p-5 shadow-2xl dark:bg-primary-100"
      >
        <h2 className="mb-4 text-sm font-semibold text-ink">New Task</h2>

        <label className="mb-3 block">
          <span className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-primary-500">
            Title
          </span>
          <input
            type="text"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            className="w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-[13px] text-ink outline-none focus:border-primary-400 dark:bg-primary-50"
            autoFocus
            placeholder="Task title…"
          />
        </label>

        <label className="mb-3 block">
          <span className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-primary-500">
            Description
          </span>
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={3}
            className="w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-[13px] text-ink outline-none focus:border-primary-400 dark:bg-primary-50"
            placeholder="Optional details…"
          />
        </label>

        <div className="mb-3 flex gap-3">
          <label className="flex-1">
            <span className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-primary-500">
              Priority
            </span>
            <select
              value={priority}
              onChange={(e) => setPriority(e.target.value as TaskPriority)}
              className="w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-[13px] text-ink outline-none dark:bg-primary-50"
            >
              {PRIORITY_ORDER.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </label>
          <label className="flex-1">
            <span className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-primary-500">
              Status
            </span>
            <select
              value={status}
              onChange={(e) => setStatus(e.target.value as TaskStatus)}
              className="w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-[13px] text-ink outline-none dark:bg-primary-50"
            >
              {STATUS_ORDER.map((s) => (
                <option key={s} value={s}>
                  {STATUS_LABELS[s]}
                </option>
              ))}
            </select>
          </label>
        </div>

        <div className="mb-4 flex gap-3">
          <label className="flex-1">
            <span className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-primary-500">
              Due Date
            </span>
            <input
              type="date"
              value={dueDate}
              onChange={(e) => setDueDate(e.target.value)}
              className="w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-[13px] text-ink outline-none dark:bg-primary-50"
            />
          </label>
          <label className="flex-1">
            <span className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-primary-500">
              Reminder
            </span>
            <input
              type="datetime-local"
              value={reminder}
              onChange={(e) => setReminder(e.target.value)}
              className="w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-[13px] text-ink outline-none dark:bg-primary-50"
            />
          </label>
        </div>

        <div className="flex justify-end gap-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg px-3 py-1.5 text-[13px] text-primary-500 hover:text-ink"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={!title.trim()}
            className="rounded-lg bg-primary-900 px-4 py-1.5 text-[13px] font-medium text-primary-50 hover:bg-primary-800 disabled:opacity-40 dark:bg-primary-200 dark:text-primary-900 dark:hover:bg-primary-300"
          >
            Add Task
          </button>
        </div>
      </form>
    </div>
  )
}

/* ── Task Card ── */

function TaskCard({
  task,
  onMove,
  onDelete,
  onSelect,
}: {
  task: Task
  onMove: (id: string, status: TaskStatus) => void
  onDelete: (id: string) => void
  onSelect: (task: Task) => void
}) {
  return (
    <article
      className="group cursor-pointer rounded-lg border border-primary-200 bg-primary-50/90 px-3 py-2.5 transition-colors hover:border-primary-300"
      onClick={() => onSelect(task)}
    >
      <p className="line-clamp-2 text-[13px] font-medium text-ink">
        {task.title}
      </p>

      {task.project ? (
        <p className="mt-1 text-[11px] text-primary-400">{task.project}</p>
      ) : null}

      <div className="mt-2 flex items-center justify-between">
        <div className="flex items-center gap-1.5">
          <span
            className={cn(
              'rounded px-1.5 py-0.5 text-[10px] font-medium tabular-nums',
              priorityColor(task.priority),
            )}
          >
            {task.priority}
          </span>
          {task.dueDate ? (
            <span
              className={cn(
                'rounded px-1.5 py-0.5 text-[10px] tabular-nums',
                new Date(task.dueDate).getTime() < Date.now()
                  ? 'bg-red-500/15 text-red-600 dark:text-red-400'
                  : new Date(task.dueDate).getTime() - Date.now() <
                      24 * 60 * 60 * 1000
                    ? 'bg-amber-500/15 text-amber-700 dark:text-amber-400'
                    : 'bg-primary-100 text-primary-500',
              )}
            >
              {formatDate(task.dueDate)}
            </span>
          ) : null}
        </div>
        <div className="flex items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
          {/* Quick move buttons */}
          {task.status !== 'in_progress' && task.status !== 'done' ? (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation()
                onMove(task.id, 'in_progress')
              }}
              className="rounded px-1.5 py-0.5 text-[10px] text-primary-500 hover:bg-primary-100 hover:text-ink"
              title="Start"
            >
              Start
            </button>
          ) : null}
          {task.status === 'in_progress' ? (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation()
                onMove(task.id, 'review')
              }}
              className="rounded px-1.5 py-0.5 text-[10px] text-primary-500 hover:bg-primary-100 hover:text-ink"
              title="Review"
            >
              Review
            </button>
          ) : null}
          {task.status === 'review' ? (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation()
                onMove(task.id, 'done')
              }}
              className="rounded px-1.5 py-0.5 text-[10px] text-emerald-600 hover:bg-emerald-50"
              title="Done"
            >
              Done
            </button>
          ) : null}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation()
              onDelete(task.id)
            }}
            className="rounded p-0.5 text-primary-300 hover:text-red-500"
            title="Delete"
          >
            <HugeiconsIcon icon={Delete02Icon} size={12} strokeWidth={1.5} />
          </button>
        </div>
      </div>
    </article>
  )
}

/* ── Task Detail Panel ── */

function TaskDetailPanel({
  task,
  onClose,
  onMove,
  onUpdate,
}: {
  task: Task
  onClose: () => void
  onMove: (id: string, status: TaskStatus) => void
  onUpdate: (
    id: string,
    updates: Partial<Omit<Task, 'id' | 'createdAt'>>,
  ) => void
}) {
  const [editing, setEditing] = useState(false)
  const [editTitle, setEditTitle] = useState(task.title)
  const [editDesc, setEditDesc] = useState(task.description)

  const handleSave = () => {
    onUpdate(task.id, { title: editTitle.trim(), description: editDesc.trim() })
    setEditing(false)
  }

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
      onClick={onClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        className="w-full max-w-lg rounded-xl border border-primary-200 bg-primary-50 p-5 shadow-2xl dark:bg-primary-100"
      >
        <div className="mb-4 flex items-start justify-between gap-3">
          {editing ? (
            <input
              type="text"
              value={editTitle}
              onChange={(e) => setEditTitle(e.target.value)}
              className="flex-1 rounded-lg border border-primary-200 bg-primary-50 px-2 py-1 text-sm font-semibold text-ink outline-none focus:border-primary-400"
              autoFocus
            />
          ) : (
            <h2 className="flex-1 text-sm font-semibold text-ink">
              {task.title}
            </h2>
          )}
          <span className="shrink-0 text-[11px] text-primary-400 tabular-nums">
            {task.id}
          </span>
        </div>

        <div className="mb-4 flex flex-wrap gap-2">
          <span
            className={cn(
              'rounded px-2 py-0.5 text-[11px] font-medium',
              priorityColor(task.priority),
            )}
          >
            {task.priority}
          </span>
          <span className="flex items-center gap-1 rounded bg-primary-100 px-2 py-0.5 text-[11px] text-primary-600">
            <span
              className={cn(
                'size-1.5 rounded-full',
                statusDotColor(task.status),
              )}
            />
            {STATUS_LABELS[task.status]}
          </span>
          {task.project ? (
            <span className="rounded bg-primary-100 px-2 py-0.5 text-[11px] text-primary-500">
              {task.project}
            </span>
          ) : null}
        </div>

        {editing ? (
          <textarea
            value={editDesc}
            onChange={(e) => setEditDesc(e.target.value)}
            rows={4}
            className="mb-4 w-full rounded-lg border border-primary-200 bg-primary-50 px-3 py-2 text-[13px] text-ink outline-none focus:border-primary-400"
          />
        ) : (
          <p className="mb-4 whitespace-pre-wrap text-[13px] text-primary-600">
            {task.description || 'No description'}
          </p>
        )}

        <div className="mb-4 flex gap-2 text-[11px] text-primary-400">
          <span>Created {formatDate(task.createdAt)}</span>
          <span>·</span>
          <span>Updated {formatDate(task.updatedAt)}</span>
        </div>

        {/* Status transitions */}
        <div className="mb-4 flex flex-wrap gap-1.5">
          {STATUS_ORDER.filter((s) => s !== task.status).map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => onMove(task.id, s)}
              className="rounded-lg border border-primary-200 px-2.5 py-1 text-[11px] font-medium text-primary-600 transition-colors hover:border-primary-300 hover:text-ink"
            >
              Move to {STATUS_LABELS[s]}
            </button>
          ))}
        </div>

        <div className="flex justify-end gap-2">
          {editing ? (
            <>
              <button
                type="button"
                onClick={() => setEditing(false)}
                className="rounded-lg px-3 py-1.5 text-[13px] text-primary-500 hover:text-ink"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={handleSave}
                className="rounded-lg bg-primary-900 px-4 py-1.5 text-[13px] font-medium text-primary-50 hover:bg-primary-800 dark:bg-primary-200 dark:text-primary-900"
              >
                Save
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                onClick={() => setEditing(true)}
                className="rounded-lg px-3 py-1.5 text-[13px] text-primary-500 hover:text-ink"
              >
                Edit
              </button>
              <button
                type="button"
                onClick={onClose}
                className="rounded-lg bg-primary-900 px-4 py-1.5 text-[13px] font-medium text-primary-50 hover:bg-primary-800 dark:bg-primary-200 dark:text-primary-900"
              >
                Close
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  )
}

/* ── Main Screen ── */

export function TasksScreen() {
  const { tasks, addTask, moveTask, updateTask, deleteTask, syncFromApi } =
    useTaskStore()
  const [showAdd, setShowAdd] = useState(false)
  const [selectedTask, setSelectedTask] = useState<Task | null>(null)

  // Sync from API on mount
  useEffect(() => {
    void syncFromApi()
  }, [syncFromApi])

  const columns = STATUS_ORDER.map((status) => ({
    status,
    label: STATUS_LABELS[status],
    tasks: tasks
      .filter((t) => t.status === status)
      .sort((a, b) => {
        const pOrder = ['P0', 'P1', 'P2', 'P3']
        return pOrder.indexOf(a.priority) - pOrder.indexOf(b.priority)
      }),
  }))

  return (
    <main className="h-full overflow-y-auto bg-surface px-4 pt-6 pb-24 text-primary-900 md:px-6 md:pt-8 md:pb-0">
      <section className="mx-auto w-full max-w-[1600px]">
        {/* Header */}
        <header className="mb-4 flex flex-wrap items-center justify-between gap-2.5 md:mb-6">
          <div className="flex items-center gap-3">
            <div>
              <h1 className="text-base font-semibold text-ink md:text-lg">
                Tasks
              </h1>
              <p className="text-[11px] text-primary-400">
                {tasks.filter((t) => t.status !== 'done').length} active ·{' '}
                {tasks.filter((t) => t.status === 'done').length} completed
              </p>
            </div>
          </div>
          <button
            type="button"
            onClick={() => setShowAdd(true)}
            className="inline-flex items-center gap-1.5 rounded-lg bg-primary-900 px-3 py-1.5 text-xs font-medium text-primary-50 transition-colors hover:bg-primary-800 md:text-[13px] dark:bg-primary-200 dark:text-primary-900 dark:hover:bg-primary-300"
          >
            <HugeiconsIcon icon={Add01Icon} size={14} strokeWidth={1.5} />
            New Task
          </button>
        </header>

        {/* Kanban Board */}
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
          {columns.map((col) => (
            <section
              key={col.status}
              className="flex flex-col rounded-xl border border-primary-200 bg-primary-50/60 p-3"
            >
              <header className="mb-3 flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <span
                    className={cn(
                      'size-2 rounded-full',
                      statusDotColor(col.status),
                    )}
                  />
                  <h2 className="text-[13px] font-medium text-ink">
                    {col.label}
                  </h2>
                </div>
                <span className="rounded-full border border-primary-200 bg-primary-50/80 px-2 py-0.5 text-[11px] font-medium text-primary-600 tabular-nums">
                  {col.tasks.length}
                </span>
              </header>

              <div className="space-y-2">
                {col.tasks.length === 0 ? (
                  <div className="rounded-lg border border-dashed border-primary-200 py-8 text-center text-[11px] text-primary-400">
                    No tasks
                  </div>
                ) : (
                  col.tasks.map((task) => (
                    <TaskCard
                      key={task.id}
                      task={task}
                      onMove={moveTask}
                      onDelete={deleteTask}
                      onSelect={setSelectedTask}
                    />
                  ))
                )}
              </div>
            </section>
          ))}
        </div>
      </section>

      {showAdd ? (
        <AddTaskDialog onAdd={addTask} onClose={() => setShowAdd(false)} />
      ) : null}
      {selectedTask ? (
        <TaskDetailPanel
          task={selectedTask}
          onClose={() => setSelectedTask(null)}
          onMove={(id, status) => {
            moveTask(id, status)
            setSelectedTask(null)
          }}
          onUpdate={updateTask}
        />
      ) : null}
    </main>
  )
}
