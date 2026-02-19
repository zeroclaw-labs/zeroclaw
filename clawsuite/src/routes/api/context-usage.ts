import { createFileRoute } from '@tanstack/react-router'
import { json } from '@tanstack/react-start'
import { gatewayRpc } from '@/server/gateway'

const MODEL_CONTEXT_WINDOWS: Record<string, number> = {
  'claude-opus-4-6': 200_000,
  'claude-opus-4-5': 200_000,
  'claude-sonnet-4-5': 200_000,
  'claude-sonnet-4': 200_000,
  'claude-haiku-3.5': 200_000,
  'gpt-5.2-codex': 1_000_000,
  'gpt-4.1': 1_000_000,
  'gpt-4.1-mini': 1_000_000,
  'gpt-4o': 128_000,
  'gpt-4o-mini': 128_000,
  o1: 200_000,
  'o3-mini': 200_000,
  'gemini-2.5-flash': 1_000_000,
  'gemini-2.5-pro': 1_000_000,
}

function getContextWindow(model: string): number {
  if (MODEL_CONTEXT_WINDOWS[model]) return MODEL_CONTEXT_WINDOWS[model]
  for (const [key, value] of Object.entries(MODEL_CONTEXT_WINDOWS)) {
    if (model.includes(key) || key.includes(model)) return value
  }
  return 200_000
}

const CHARS_PER_TOKEN = 4

export const Route = createFileRoute('/api/context-usage')({
  server: {
    handlers: {
      GET: async () => {
        try {
          const [sessionsResult, usageResult] = await Promise.allSettled([
            gatewayRpc<any>('sessions.list', { limit: 50 }),
            gatewayRpc<any>('sessions.usage', {
              limit: 10,
              includeContextWeight: true,
            }),
          ])

          const sessions =
            sessionsResult.status === 'fulfilled'
              ? (sessionsResult.value?.sessions ?? [])
              : []
          const usageSessions =
            usageResult.status === 'fulfilled'
              ? (usageResult.value?.sessions ?? [])
              : []

          const mainSession = sessions.find(
            (s: any) => s.kind === 'main' || (s.key && s.key.includes(':main')),
          )

          if (!mainSession) {
            return json({
              ok: true,
              contextPercent: 0,
              model: '',
              maxTokens: 0,
              usedTokens: 0,
            })
          }

          const model = mainSession.model ?? ''
          const maxTokens = getContextWindow(model)
          const mainUsage = usageSessions.find(
            (s: any) => s.key === mainSession.key,
          )
          const cw = mainUsage?.contextWeight
          const u = mainUsage?.usage

          let staticTokens = 0
          if (cw) {
            const systemChars = cw.systemPrompt?.chars ?? 0
            const skillsChars = cw.skills?.promptChars ?? 0
            const toolsChars =
              (cw.tools?.listChars ?? 0) + (cw.tools?.schemaChars ?? 0)
            staticTokens = Math.ceil(
              (systemChars + skillsChars + toolsChars) / CHARS_PER_TOKEN,
            )
          }

          let conversationTokens = 0
          if (u) {
            const cacheRead = u.cacheRead ?? 0
            const turnCount =
              u.latency?.count ?? u.messageCounts?.assistant ?? 1
            if (cacheRead > 0 && turnCount > 0) {
              conversationTokens = Math.ceil((cacheRead / turnCount) * 1.2)
            } else {
              conversationTokens = (u.input ?? 0) + (u.output ?? 0)
            }
          }

          const usedTokens = Math.min(
            staticTokens + conversationTokens,
            maxTokens,
          )
          const contextPercent =
            maxTokens > 0 ? (usedTokens / maxTokens) * 100 : 0

          return json({
            ok: true,
            contextPercent: Math.round(contextPercent * 10) / 10,
            model,
            maxTokens,
            usedTokens,
            staticTokens,
            conversationTokens: Math.min(conversationTokens, maxTokens),
          })
        } catch (err) {
          return json(
            {
              ok: false,
              error: err instanceof Error ? err.message : String(err),
              contextPercent: 0,
            },
            { status: 500 },
          )
        }
      },
    },
  },
})
