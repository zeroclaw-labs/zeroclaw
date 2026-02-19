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

async function readTasks(): Promise<unknown[]> {
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

const CreateTaskSchema = z.object({
  title: z.string().trim().min(1).max(500),
  description: z.string().max(5000).default(''),
  status: z
    .enum(['backlog', 'in_progress', 'review', 'done'])
    .default('backlog'),
  priority: z.enum(['P0', 'P1', 'P2', 'P3']).default('P1'),
  project: z.string().max(100).optional(),
  tags: z.array(z.string().max(50)).max(20).default([]),
  dueDate: z.string().max(30).optional(),
  reminder: z.string().max(30).optional(),
})

export const Route = createFileRoute('/api/tasks/')({
  server: {
    handlers: {
      GET: async ({ request }) => {
        const ip = getClientIp(request)
        if (!rateLimit(`tasks-get:${ip}`, 60, 60_000))
          return rateLimitResponse()

        try {
          const tasks = await readTasks()
          return json({ tasks })
        } catch (err) {
          return json({ error: safeErrorMessage(err) }, { status: 500 })
        }
      },

      POST: async ({ request }) => {
        const ip = getClientIp(request)
        if (!rateLimit(`tasks-post:${ip}`, 30, 60_000))
          return rateLimitResponse()

        try {
          const body = await request.json().catch(() => ({}))
          const parsed = CreateTaskSchema.safeParse(body)
          if (!parsed.success) {
            return json(
              {
                error: 'Validation failed',
                details: parsed.error.flatten().fieldErrors,
              },
              { status: 400 },
            )
          }

          const now = new Date().toISOString()
          const task = {
            id: `TASK-${Date.now().toString(36).toUpperCase()}`,
            ...parsed.data,
            createdAt: now,
            updatedAt: now,
          }

          const tasks = await readTasks()
          tasks.unshift(task)
          await writeTasks(tasks)

          return json({ task }, { status: 201 })
        } catch (err) {
          return json({ error: safeErrorMessage(err) }, { status: 500 })
        }
      },
    },
  },
})
