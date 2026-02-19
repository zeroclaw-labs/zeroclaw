import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '../../server/gateway'

const SESSION_STATUS_METHODS = [
  'sessions.usage',
  'session.status',
  'sessions.status',
]

async function trySessionStatus(): Promise<unknown> {
  let lastError: unknown = null
  for (const method of SESSION_STATUS_METHODS) {
    try {
      return await gatewayRpc(method)
    } catch (error) {
      lastError = error
    }
  }
  throw lastError instanceof Error
    ? lastError
    : new Error('Session status unavailable')
}

// Known model context windows
const MODEL_CONTEXT_WINDOWS: Record<string, number> = {
  'claude-opus-4-6': 200_000,
  'claude-opus-4-5': 200_000,
  'claude-sonnet-4-5': 200_000,
  'claude-sonnet-4': 200_000,
  'claude-haiku-3.5': 200_000,
  'gpt-5.2-codex': 1_000_000,
  'gpt-4.1': 1_000_000,
  'gpt-4o': 128_000,
  'gemini-2.5-flash': 1_000_000,
}

function getContextWindow(model: string): number {
  if (MODEL_CONTEXT_WINDOWS[model]) return MODEL_CONTEXT_WINDOWS[model]
  for (const [key, value] of Object.entries(MODEL_CONTEXT_WINDOWS)) {
    if (model.includes(key) || key.includes(model)) return value
  }
  return 200_000
}

export const Route = createFileRoute('/api/session-status')({
  server: {
    handlers: {
      GET: async () => {
        try {
          // Fetch both status and usage data in parallel
          const [statusResult, usageResult] = await Promise.allSettled([
            trySessionStatus(),
            gatewayRpc<any>('sessions.usage', {
              limit: 5,
              includeContextWeight: true,
            }),
          ])

          const payload =
            statusResult.status === 'fulfilled' ? statusResult.value : {}
          const usageData =
            usageResult.status === 'fulfilled' ? usageResult.value : null

          // Find main session usage
          const mainUsage = usageData?.sessions?.find((s: any) =>
            s.key?.includes(':main'),
          )

          // Enrich payload with session usage data
          const enriched: Record<string, unknown> = {
            ...(payload && typeof payload === 'object' ? payload : {}),
          }

          if (mainUsage?.usage) {
            const u = mainUsage.usage
            const model = mainUsage.model ?? mainUsage.modelOverride ?? ''
            const maxTokens = getContextWindow(model)

            // Calculate context % from cache data
            const cacheRead = u.cacheRead ?? 0
            const turnCount =
              u.latency?.count ?? u.messageCounts?.assistant ?? 1
            let estimatedContext = 0
            if (cacheRead > 0 && turnCount > 0) {
              estimatedContext = Math.ceil((cacheRead / turnCount) * 1.2)
            }
            const contextPercent =
              maxTokens > 0
                ? Math.min((estimatedContext / maxTokens) * 100, 100)
                : 0

            enriched.inputTokens = u.input ?? 0
            enriched.outputTokens = u.output ?? 0
            enriched.totalTokens = (u.input ?? 0) + (u.output ?? 0)
            enriched.cacheRead = u.cacheRead ?? 0
            enriched.cacheWrite = u.cacheWrite ?? 0
            enriched.contextPercent = Math.round(contextPercent * 10) / 10
            enriched.dailyCost = u.totalCost ?? 0
            enriched.costUsd = u.totalCost ?? 0
            enriched.model = model
            enriched.modelProvider = mainUsage.modelProvider ?? ''
            enriched.sessionKey = mainUsage.key ?? ''
            enriched.messageCounts = u.messageCounts ?? {}
            enriched.latency = u.latency ?? {}

            // Model breakdown
            if (u.modelUsage) {
              enriched.models = u.modelUsage.map((m: any) => ({
                model: m.model,
                provider: m.provider,
                inputTokens: m.totals?.input ?? 0,
                outputTokens: m.totals?.output ?? 0,
                costUsd: m.totals?.totalCost ?? 0,
                count: m.count ?? 0,
              }))
            }
          }

          return json({ ok: true, payload: enriched })
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
            },
            { status: 503 },
          )
        }
      },
    },
  },
})
