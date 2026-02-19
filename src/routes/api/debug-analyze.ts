import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { analyzeError, readOpenClawLogs } from '../../server/debug-analyzer'
import {
  getClientIp,
  rateLimit,
  rateLimitResponse,
  safeErrorMessage,
} from '../../server/rate-limit'

export const Route = createFileRoute('/api/debug-analyze')({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const ip = getClientIp(request)
        if (!rateLimit(`debug:${ip}`, 10, 60_000)) {
          return rateLimitResponse()
        }

        try {
          const body = (await request.json().catch(() => ({}))) as Record<
            string,
            unknown
          >
          const terminalOutput =
            typeof body.terminalOutput === 'string' ? body.terminalOutput : ''

          const logContent = await readOpenClawLogs()
          const analysis = await analyzeError(terminalOutput, logContent)
          return json(analysis)
        } catch (error) {
          if (import.meta.env.DEV) console.error(
            '[/api/debug-analyze] Error:',
            error instanceof Error ? error.message : String(error),
          )
          return json(
            {
              summary: 'Debug analysis request failed.',
              rootCause: safeErrorMessage(error),
              suggestedCommands: [],
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
