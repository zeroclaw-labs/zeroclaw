import fs from 'node:fs/promises'
import path from 'node:path'
import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { z } from 'zod'
import {
  getClientIp,
  rateLimit,
  rateLimitResponse,
  safeErrorMessage,
} from '../../../server/rate-limit'

const TASKS_FILE = path.join(process.cwd(), 'data', 'tasks.json')

async function readTasks(): Promise<Record<string, unknown>[]> {
  try {
    const raw = await fs.readFile(TASKS_FILE, 'utf-8')
    const parsed = JSON.parse(raw)
    return Array.isArray(parsed) ? parsed : []
  } catch {
    return []
  }
}

async function writeTasks(tasks: unknown[]): Promise<void> {
  await fs.mkdir(path.dirname(TASKS_FILE), { recursive: true })
  await fs.writeFile(TASKS_FILE, JSON.stringify(tasks, null, 2), 'utf-8')
}

function extractTaskId(request: Request): string {
  const url = new URL(request.url)
  const segments = url.pathname.split('/')
  return decodeURIComponent(segments[segments.length - 1] ?? '').trim()
}

const UpdateTaskSchema = z.object({
  title: z.string().trim().min(1).max(500).optional(),
  description: z.string().max(5000).optional(),
  status: z.enum(['backlog', 'in_progress', 'review', 'done']).optional(),
  priority: z.enum(['P0', 'P1', 'P2', 'P3']).optional(),
  project: z.string().max(100).optional().nullable(),
  tags: z.array(z.string().max(50)).max(20).optional(),
  dueDate: z.string().max(30).optional().nullable(),
  reminder: z.string().max(30).optional().nullable(),
})

export const Route = createFileRoute('/api/tasks/$taskId')({
  server: {
    handlers: {
      PATCH: async ({ request }) => {
        const ip = getClientIp(request)
        if (!rateLimit(`tasks-patch:${ip}`, 30, 60_000))
          return rateLimitResponse()

        try {
          const taskId = extractTaskId(request)
          if (!taskId)
            return json({ error: 'taskId is required' }, { status: 400 })

          const body = await request.json().catch(() => ({}))
          const parsed = UpdateTaskSchema.safeParse(body)
          if (!parsed.success) {
            return json(
              {
                error: 'Validation failed',
                details: parsed.error.flatten().fieldErrors,
              },
              { status: 400 },
            )
          }

          const tasks = await readTasks()
          const idx = tasks.findIndex((t) => t.id === taskId)
          if (idx === -1)
            return json({ error: 'Task not found' }, { status: 404 })

          const updated = {
            ...tasks[idx],
            ...parsed.data,
            updatedAt: new Date().toISOString(),
          }
          tasks[idx] = updated
          await writeTasks(tasks)

          return json({ task: updated })
        } catch (err) {
          return json({ error: safeErrorMessage(err) }, { status: 500 })
        }
      },

      DELETE: async ({ request }) => {
        const ip = getClientIp(request)
        if (!rateLimit(`tasks-delete:${ip}`, 30, 60_000))
          return rateLimitResponse()

        try {
          const taskId = extractTaskId(request)
          if (!taskId)
            return json({ error: 'taskId is required' }, { status: 400 })

          const tasks = await readTasks()
          const filtered = tasks.filter((t) => t.id !== taskId)
          if (filtered.length === tasks.length)
            return json({ error: 'Task not found' }, { status: 404 })

          await writeTasks(filtered)
          return json({ ok: true })
        } catch (err) {
          return json({ error: safeErrorMessage(err) }, { status: 500 })
        }
      },
    },
  },
})
